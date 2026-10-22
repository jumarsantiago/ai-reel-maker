//! FFmpeg export pipeline.
//! Re-validates every segment against a fresh ffprobe, builds discrete arguments (never a shell string),
//! renders to a hidden temp file next to the output, inspects it, then renames it into place.

use crate::media::{self, MediaInfo};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const MIN_SEGMENT_SEC: f64 = 0.1;
/// Probed durations can differ from the UI's by container rounding.
const DURATION_TOLERANCE_SEC: f64 = 0.05;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentRequest {
    pub source_path: String,
    pub in_point: f64,
    pub out_point: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    pub segments: Vec<SegmentRequest>,
    pub output_path: String,
    /// True only when the user explicitly confirmed replacing an existing file (native save panel).
    #[serde(default)]
    pub overwrite_confirmed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub output_path: String,
    pub duration_sec: f64,
    pub width: u32,
    pub height: u32,
    pub has_audio: bool,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedSegment {
    pub path: PathBuf,
    pub in_point: f64,
    pub length: f64,
    pub has_audio: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub segments: Vec<PlannedSegment>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub fit: Fit,
    pub overlays: Vec<Overlay>,
    pub style: Style,
}

/// Optional "reel" styling; all off by default so plain exports are unchanged.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Style {
    /// Per-segment punch-in zoom (1.0 = none). Missing entries mean 1.0. Crop fit only.
    pub zoom: Vec<f64>,
    /// A short sound effect mixed in at each listed output time.
    pub sfx: Option<Sfx>,
    /// Background music, looped to length and ducked under speech.
    pub music: Option<Music>,
    /// Normalize the final mix to -14 LUFS (social-media loudness).
    pub loudnorm: bool,
    /// Slow sideways drift on each clip (Crop fit only); replaces the punch-in zoom.
    pub pan: bool,
    pub grade: Grade,
    /// Crossfade/slide between clips instead of a hard cut.
    pub transition: Option<Transition>,
    /// Music plays at a steady level instead of being ducked by the clips' own sound (b-roll).
    pub music_no_duck: bool,
    /// Level of the clips' own sound when music plays under it without ducking.
    pub natural_gain_db: f64,
    /// Fade the picture and sound in at the start and out at the end (seconds, 0 = none).
    pub fade_sec: f64,
}

/// A simple colour look applied to every clip.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Grade {
    #[default]
    None,
    Warm,
    Vivid,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransitionStyle {
    /// Soft cross-dissolve every time.
    Smooth,
    /// A varied mix of slides, zooms and wipes.
    Dynamic,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transition {
    pub style: TransitionStyle,
    pub sec: f64,
}

const DYNAMIC_TRANSITIONS: &[&str] = &["slideleft", "zoomin", "smoothup", "circleopen", "fade", "slideright", "dissolve"];

fn has_audio_track(plan: &Plan) -> bool {
    // Music alone is enough: clips without sound get silence so the music still has a track to join.
    plan.segments.iter().any(|s| s.has_audio) || plan.style.music.is_some()
}

/// Seconds each junction overlaps (0 = plain concat). Never more than 40% of the shortest clip.
fn xfade_sec(plan: &Plan) -> f64 {
    let Some(t) = plan.style.transition else { return 0.0 };
    if plan.segments.len() < 2 {
        return 0.0;
    }
    let shortest = plan.segments.iter().map(|s| s.length).fold(f64::INFINITY, f64::min);
    let d = t.sec.min(shortest * 0.4);
    if d < 0.05 { 0.0 } else { d }
}

/// Output length, accounting for the overlap at every crossfade.
pub fn plan_total(plan: &Plan) -> f64 {
    let sum: f64 = plan.segments.iter().map(|s| s.length).sum();
    sum - xfade_sec(plan) * plan.segments.len().saturating_sub(1) as f64
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sfx {
    pub path: PathBuf,
    pub times: Vec<f64>,
    pub gain_db: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Music {
    pub path: PathBuf,
    pub gain_db: f64,
}

/// How each clip fills the output frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fit {
    /// Scale down to fit, letterbox the rest (timeline export).
    Pad,
    /// Scale up to cover, centre-crop the overflow (vertical reels).
    Crop,
}

/// A still image (e.g. a caption PNG) shown over the output between `start` and `end`
/// (output-timeline seconds), horizontally centred with its centre at `y_center_frac` of the height.
#[derive(Debug, Clone, PartialEq)]
pub struct Overlay {
    pub image: PathBuf,
    pub start: f64,
    pub end: f64,
    pub y_center_frac: f64,
}

fn even(n: u32) -> u32 {
    (n - n % 2).max(2)
}

/// Validates segments against probed source info. Output format follows the first segment.
pub fn plan_segments(
    requests: &[SegmentRequest],
    probe: impl Fn(&str) -> Result<MediaInfo, String>,
) -> Result<Plan, String> {
    if requests.is_empty() {
        return Err("Timeline is empty.".into());
    }
    let mut segments = Vec::with_capacity(requests.len());
    let mut first: Option<MediaInfo> = None;
    for (i, r) in requests.iter().enumerate() {
        let info = probe(&r.source_path).map_err(|e| format!("Clip {}: {e}", i + 1))?;
        if !r.in_point.is_finite() || !r.out_point.is_finite() || r.in_point < 0.0 {
            return Err(format!("Clip {}: invalid in point.", i + 1));
        }
        if r.out_point > info.duration_sec + DURATION_TOLERANCE_SEC {
            return Err(format!("Clip {}: out point is past the end of {}.", i + 1, info.file_name));
        }
        let out_point = r.out_point.min(info.duration_sec);
        let length = out_point - r.in_point;
        if length < MIN_SEGMENT_SEC {
            return Err(format!("Clip {}: must be at least {MIN_SEGMENT_SEC}s long.", i + 1));
        }
        segments.push(PlannedSegment {
            path: PathBuf::from(&info.path),
            in_point: r.in_point,
            length,
            has_audio: info.has_audio,
        });
        first.get_or_insert(info);
    }
    let first = first.expect("non-empty");
    if first.width == 0 || first.height == 0 {
        return Err("Could not read the first clip's resolution.".into());
    }
    let fps = first.fps.round() as u32;
    Ok(Plan {
        segments,
        width: even(first.width),
        height: even(first.height),
        fps: if (1..=120).contains(&fps) { fps } else { 30 },
        fit: Fit::Pad,
        overlays: Vec::new(),
        style: Style::default(),
    })
}

/// Output must be a new (or explicitly confirmed) .mp4 in an existing folder, and never a source file.
pub fn validate_output_path(output: &str, sources: &[PathBuf], overwrite_confirmed: bool) -> Result<PathBuf, String> {
    let out = PathBuf::from(output);
    if !out.is_absolute() {
        return Err("Output path must be absolute.".into());
    }
    let is_mp4 = out.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("mp4"));
    if !is_mp4 {
        return Err("Output file must end in .mp4.".into());
    }
    let parent = out.parent().filter(|p| p.is_dir()).ok_or("Output folder does not exist.")?;
    let file_name = out.file_name().ok_or("Output file name is missing.")?;
    let resolved = parent
        .canonicalize()
        .map_err(|e| format!("Cannot access output folder: {e}"))?
        .join(file_name);
    for src in sources {
        if src.canonicalize().map(|s| s == resolved).unwrap_or(false) {
            return Err("Output cannot replace one of the source clips.".into());
        }
    }
    if resolved.is_dir() {
        return Err("Output path is a folder.".into());
    }
    if resolved.exists() && !overwrite_confirmed {
        return Err(format!("{} already exists. Choose another name.", file_name.to_string_lossy()));
    }
    Ok(resolved)
}

fn secs(x: f64) -> String {
    format!("{x:.3}")
}

pub fn build_filter_graph(plan: &Plan) -> String {
    let (w, h, fps) = (plan.width, plan.height, plan.fps);
    let any_audio = has_audio_track(plan);
    let xf = xfade_sec(plan);
    let total = plan_total(plan);
    let mut graph = String::new();
    let mut concat_inputs = String::new();
    for (i, s) in plan.segments.iter().enumerate() {
        let frame = match plan.fit {
            Fit::Pad => format!("scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2"),
            Fit::Crop if plan.style.pan => {
                // Scale up 8% and drift across it: up to 70px each way over the clip, alternating direction.
                let dir = if i % 2 == 0 { 1 } else { -1 };
                format!(
                    "scale={w}:{h}:force_original_aspect_ratio=increase,scale=trunc(iw*1.08/2)*2:trunc(ih*1.08/2)*2,\
                     crop={w}:{h}:x='(iw-ow)/2+({dir})*min(70,(iw-ow)/2)*(2*t/{len}-1)':y='(ih-oh)/2'",
                    len = secs(s.length)
                )
            }
            Fit::Crop => format!("scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h}"),
        };
        let grade = match plan.style.grade {
            Grade::None => "",
            Grade::Warm => ",eq=contrast=1.06:saturation=1.2:gamma_r=1.04:gamma_b=0.95",
            Grade::Vivid => ",eq=contrast=1.08:saturation=1.4",
        };
        let z = if plan.style.pan { 1.0 } else { plan.style.zoom.get(i).copied().unwrap_or(1.0) };
        let zoom = if plan.fit == Fit::Crop && z > 1.001 {
            format!(",scale={}:{},crop={w}:{h}", even((w as f64 * z).round() as u32), even((h as f64 * z).round() as u32))
        } else {
            String::new()
        };
        graph += &format!("[{i}:v:0]setpts=PTS-STARTPTS,{frame}{zoom}{grade},setsar=1,fps={fps},format=yuv420p[v{i}];");
        concat_inputs += &format!("[v{i}]");
        if any_audio {
            let len = secs(s.length);
            if s.has_audio {
                graph += &format!(
                    "[{i}:a:0]asetpts=PTS-STARTPTS,aresample=48000,\
                     aformat=sample_fmts=fltp:channel_layouts=stereo,apad,atrim=duration={len}[a{i}];"
                );
            } else {
                // Silent filler keeps audio aligned when only some clips have sound.
                graph += &format!(
                    "anullsrc=r=48000:cl=stereo,aformat=sample_fmts=fltp,atrim=duration={len}[a{i}];"
                );
            }
            concat_inputs += &format!("[a{i}]");
        }
    }
    let n = plan.segments.len();
    let fade = plan.style.fade_sec.min(total / 3.0).max(0.0);
    let vfade = if fade > 0.05 { format!("fade=t=in:st=0:d={f:.2},fade=t=out:st={o:.3}:d={f:.2}", f = fade, o = total - fade) } else { String::new() };
    let video_out = if plan.overlays.is_empty() && vfade.is_empty() { "[outv]" } else { "[cv]" };
    let sfx_times = plan.style.sfx.as_ref().map(|x| x.times.as_slice()).unwrap_or(&[]);
    let audio_post = any_audio && (plan.style.music.is_some() || !sfx_times.is_empty() || plan.style.loudnorm || fade > 0.05);
    let audio_out = if audio_post { "[ca]" } else { "[outa]" };
    if xf > 0.0 {
        // Crossfade every junction: overlapping video (xfade) and audio (acrossfade).
        let mut acc = plan.segments[0].length;
        let (mut pv, mut pa) = ("v0".to_string(), "a0".to_string());
        for k in 1..n {
            let kind = match plan.style.transition.map(|t| t.style) {
                Some(TransitionStyle::Dynamic) => DYNAMIC_TRANSITIONS[(k - 1) % DYNAMIC_TRANSITIONS.len()],
                _ => "fade",
            };
            let last = k == n - 1;
            let lv = if last { video_out.trim_matches(['[', ']']).to_string() } else { format!("x{k}") };
            graph += &format!("[{pv}][v{k}]xfade=transition={kind}:duration={xf:.3}:offset={o:.3}[{lv}];", o = acc - xf);
            pv = lv;
            if any_audio {
                let la = if last { audio_out.trim_matches(['[', ']']).to_string() } else { format!("ax{k}") };
                graph += &format!("[{pa}][a{k}]acrossfade=d={xf:.3}:c1=tri:c2=tri[{la}];");
                pa = la;
            }
            acc += plan.segments[k].length - xf;
        }
        graph.pop(); // trailing ';'
    } else if any_audio {
        graph += &format!("{concat_inputs}concat=n={n}:v=1:a=1{video_out}{audio_out}");
    } else {
        graph += &format!("{concat_inputs}concat=n={n}:v=1:a=0{video_out}");
    }
    // Overlay inputs follow the segment inputs; each is enabled only during its time window.
    let mut prev = "cv".to_string();
    for (k, o) in plan.overlays.iter().enumerate() {
        let last = k + 1 == plan.overlays.len();
        let label = if last { "outv".to_string() } else { format!("o{k}") };
        let tail = match (last, vfade.is_empty()) {
            (true, true) => ",format=yuv420p".to_string(),
            (true, false) => format!(",{vfade},format=yuv420p"),
            _ => String::new(),
        };
        graph += &format!(
            ";[{prev}][{idx}:v]overlay=x=(main_w-overlay_w)/2:y=main_h*{y:.4}-overlay_h/2:\
             enable='between(t,{a},{b})'{tail}[{label}]",
            idx = n + k,
            y = o.y_center_frac,
            a = secs(o.start),
            b = secs(o.end),
        );
        prev = label;
    }
    if plan.overlays.is_empty() && !vfade.is_empty() {
        graph += &format!(";[cv]{vfade},format=yuv420p[outv]");
    }
    if audio_post {
        let fmt = "aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo";
        let mut idx = n + plan.overlays.len();
        let mut mix: Vec<String> = Vec::new();
        if let Some(m) = &plan.style.music {
            if plan.style.music_no_duck {
                // B-roll: the clips' own sound sits under steady music; nothing ducks the music.
                graph += &format!(
                    ";[ca]volume={n:.1}dB[sp];[{idx}:a:0]{fmt},atrim=duration={t},volume={g:.1}dB[duck]",
                    n = plan.style.natural_gain_db,
                    t = secs(total),
                    g = m.gain_db
                );
            } else {
                // Music is ducked by the speech (sidechain), then mixed under it.
                graph += &format!(
                    ";[ca]asplit=2[sp][sc];[{idx}:a:0]{fmt},atrim=duration={t},volume={g:.1}dB[mus];\
                     [mus][sc]sidechaincompress=threshold=0.02:ratio=12:attack=10:release=400[duck]",
                    t = secs(total),
                    g = m.gain_db
                );
            }
            mix.push("sp".into());
            mix.push("duck".into());
            idx += 1;
        } else {
            mix.push("ca".into());
        }
        if let Some(fx) = plan.style.sfx.as_ref().filter(|x| !x.times.is_empty()) {
            let k = fx.times.len();
            let labels: String = (0..k).map(|j| format!("[s{j}]")).collect();
            let split = if k == 1 { "anull".to_string() } else { format!("asplit={k}") };
            graph += &format!(";[{idx}:a:0]{fmt},volume={g:.1}dB,{split}{labels}", g = fx.gain_db);
            for (j, t) in fx.times.iter().enumerate() {
                let ms = (t.max(0.0) * 1000.0).round() as u64;
                graph += &format!(";[s{j}]adelay=delays={ms}:all=1[d{j}]");
                mix.push(format!("d{j}"));
            }
        }
        let base = if plan.style.loudnorm { "loudnorm=I=-14:TP=-1.5:LRA=11,aresample=48000" } else { "anull" };
        let finish = if fade > 0.05 {
            format!("{base},afade=t=in:st=0:d={f:.2},afade=t=out:st={o:.3}:d={f:.2}", f = fade, o = total - fade)
        } else {
            base.to_string()
        };
        if mix.len() == 1 {
            graph += &format!(";[{}]{finish}[outa]", mix[0]);
        } else {
            let inputs: String = mix.iter().map(|m| format!("[{m}]")).collect();
            graph += &format!(";{inputs}amix=inputs={}:duration=first:normalize=0,{finish}[outa]", mix.len());
        }
    }
    graph
}

pub fn build_ffmpeg_args(plan: &Plan, output: &Path) -> Vec<OsString> {
    // AIRM_FFMPEG_LOG=warning|info|... gives a more detailed FFmpeg log when debugging a failed render.
    let level = std::env::var("AIRM_FFMPEG_LOG").unwrap_or_else(|_| "error".into());
    let mut args: Vec<OsString> = ["-hide_banner", "-nostdin", "-v", level.as_str(), "-progress", "pipe:1", "-nostats"]
        .iter()
        .map(OsString::from)
        .collect();
    for s in &plan.segments {
        args.extend(["-ss".into(), secs(s.in_point).into(), "-t".into(), secs(s.length).into(), "-i".into()]);
        args.push(s.path.clone().into_os_string());
    }
    for o in &plan.overlays {
        // One decoder per caption image: a single thread each, or hundreds of them exhaust macOS's per-process thread limit.
        args.extend(["-threads", "1", "-i"].map(OsString::from));
        args.push(o.image.clone().into_os_string());
    }
    let any_audio = has_audio_track(plan);
    if let (true, Some(m)) = (any_audio, &plan.style.music) {
        args.extend(["-stream_loop", "-1", "-i"].map(OsString::from));
        args.push(m.path.clone().into_os_string());
    }
    if let (true, Some(fx)) = (any_audio, plan.style.sfx.as_ref().filter(|x| !x.times.is_empty())) {
        args.push("-i".into());
        args.push(fx.path.clone().into_os_string());
    }
    if plan.overlays.len() > MANY_OVERLAYS {
        // Every overlay filter wants its own worker threads; a long reel (hundreds of captions) would pass
        // macOS's 4096-threads-per-process limit and FFmpeg dies with "Resource temporarily unavailable".
        args.extend(["-filter_complex_threads", "1"].map(OsString::from));
    }
    args.push("-filter_complex".into());
    args.push(build_filter_graph(plan).into());
    args.extend(["-map", "[outv]"].map(OsString::from));
    if any_audio {
        args.extend(["-map", "[outa]", "-c:a", "aac", "-b:a", "160k"].map(OsString::from));
    } else {
        args.push("-an".into());
    }
    args.extend(
        ["-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-pix_fmt", "yuv420p", "-movflags", "+faststart", "-y"]
            .map(OsString::from),
    );
    args.push(output.as_os_str().to_owned());
    args
}

/// Hidden sibling of the output; still ends in .mp4 so FFmpeg picks the right muxer.
fn temp_path_for(out: &Path) -> PathBuf {
    let stem = out.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    out.with_file_name(format!(".{stem}.partial-{}-{nanos}.mp4", std::process::id()))
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

pub fn export(req: &ExportRequest, on_progress: impl Fn(f64)) -> Result<ExportResult, String> {
    let plan = plan_segments(&req.segments, media::probe)?;
    let sources: Vec<PathBuf> = plan.segments.iter().map(|s| s.path.clone()).collect();
    let out = validate_output_path(&req.output_path, &sources, req.overwrite_confirmed)?;
    render_to(&plan, &out, req.overwrite_confirmed, on_progress)
}

/// Above this many caption overlays FFmpeg is told to run its filter graph on one thread.
const MANY_OVERLAYS: usize = 150;

/// Apps launched from Finder start with a soft limit of 256 open files, but FFmpeg opens one input per caption
/// image (hundreds for a long reel). Raises the soft limit (child processes inherit it) up to the hard limit,
/// capped at 10240, which macOS accepts.
pub fn raise_open_file_limit() {
    let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: plain libc calls on a local struct.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            return;
        }
        let want = lim.rlim_max.min(10240);
        if lim.rlim_cur < want {
            lim.rlim_cur = want;
            let _ = libc::setrlimit(libc::RLIMIT_NOFILE, &lim);
        }
    }
}

/// Renders an already-validated plan to an already-validated output path (temp file, inspect, rename).
pub fn render_to(plan: &Plan, out: &Path, overwrite_confirmed: bool, on_progress: impl Fn(f64)) -> Result<ExportResult, String> {
    let ffmpeg = media::find_tool("ffmpeg").ok_or("FFmpeg not found. Install it (e.g. `brew install ffmpeg`).")?;
    raise_open_file_limit();
    let temp = temp_path_for(&out);
    let total = plan_total(plan);

    let mut child = Command::new(ffmpeg)
        .args(build_ffmpeg_args(&plan, &temp))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start FFmpeg: {e}"))?;

    // Drain stderr on its own thread so a chatty FFmpeg cannot block on a full pipe.
    let mut stderr = child.stderr.take().expect("piped");
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let stdout = child.stdout.take().expect("piped");
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<f64>().ok()) {
            on_progress((us / 1e6 / total).clamp(0.0, 0.99));
        }
    }
    let status = child.wait().map_err(|e| format!("FFmpeg did not finish: {e}"));
    let err_text = err_reader.join().unwrap_or_default();

    let fail = |msg: String| {
        let _ = std::fs::remove_file(&temp);
        Err(msg)
    };
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => return fail(format!("FFmpeg failed ({s}):\n{}", tail(&err_text, 8))),
        Err(e) => return fail(e),
    }
    let info = match media::probe(&temp.to_string_lossy()) {
        Ok(i) => i,
        Err(e) => return fail(format!("Rendered file failed inspection: {e}")),
    };
    if out.exists() && !overwrite_confirmed {
        return fail("Output file appeared during export; not overwriting it.".into());
    }
    if let Err(e) = std::fs::rename(&temp, &out) {
        return fail(format!("Could not move the rendered file into place: {e}"));
    }
    on_progress(1.0);
    Ok(ExportResult {
        output_path: out.to_string_lossy().into_owned(),
        duration_sec: info.duration_sec,
        width: info.width,
        height: info.height,
        has_audio: info.has_audio,
        size_bytes: info.size_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn info(path: &str, dur: f64, w: u32, h: u32, audio: bool) -> MediaInfo {
        MediaInfo {
            path: path.into(),
            file_name: Path::new(path).file_name().unwrap().to_string_lossy().into(),
            size_bytes: 1,
            duration_sec: dur,
            width: w,
            height: h,
            video_codec: "h264".into(),
            fps: 29.97,
            has_audio: audio,
        }
    }

    fn fake_probe(path: &str) -> Result<MediaInfo, String> {
        match path {
            "/v/a.mp4" => Ok(info(path, 5.0, 1281, 721, true)),
            "/v/b.mp4" => Ok(info(path, 4.0, 640, 360, false)),
            _ => Err("missing".into()),
        }
    }

    fn seg(p: &str, i: f64, o: f64) -> SegmentRequest {
        SegmentRequest { source_path: p.into(), in_point: i, out_point: o }
    }

    #[test]
    fn plan_uses_first_clip_format_and_even_dims() {
        let plan = plan_segments(&[seg("/v/a.mp4", 1.0, 3.0), seg("/v/b.mp4", 0.0, 4.0)], fake_probe).unwrap();
        assert_eq!((plan.width, plan.height, plan.fps), (1280, 720, 30));
        assert_eq!(plan.segments[0].length, 2.0);
        assert!(!plan.segments[1].has_audio);
    }

    #[test]
    fn plan_rejects_bad_segments() {
        assert!(plan_segments(&[], fake_probe).is_err());
        assert!(plan_segments(&[seg("/v/a.mp4", 1.0, 9.0)], fake_probe).is_err());
        assert!(plan_segments(&[seg("/v/a.mp4", 3.0, 2.0)], fake_probe).is_err());
        assert!(plan_segments(&[seg("/v/a.mp4", -1.0, 2.0)], fake_probe).is_err());
        assert!(plan_segments(&[seg("/v/a.mp4", f64::NAN, 2.0)], fake_probe).is_err());
        let e = plan_segments(&[seg("/v/a.mp4", 0.0, 1.0), seg("/v/zz.mp4", 0.0, 1.0)], fake_probe).unwrap_err();
        assert!(e.starts_with("Clip 2"));
        // Tiny overshoot from container rounding is clamped, not rejected.
        let p = plan_segments(&[seg("/v/a.mp4", 0.0, 5.02)], fake_probe).unwrap();
        assert_eq!(p.segments[0].length, 5.0);
    }

    #[test]
    fn args_are_discrete_and_ordered() {
        let plan = plan_segments(&[seg("/v/b.mp4", 0.5, 2.0), seg("/v/a.mp4", 1.0, 3.0)], fake_probe).unwrap();
        let args: Vec<String> = build_ffmpeg_args(&plan, Path::new("/out/x.mp4"))
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let inputs: Vec<&String> = args.windows(2).filter(|w| w[0] == "-i").map(|w| &w[1]).collect();
        assert_eq!(inputs, ["/v/b.mp4", "/v/a.mp4"]);
        let ss: Vec<&String> = args.windows(2).filter(|w| w[0] == "-ss").map(|w| &w[1]).collect();
        assert_eq!(ss, ["0.500", "1.000"]);
        let t: Vec<&String> = args.windows(2).filter(|w| w[0] == "-t").map(|w| &w[1]).collect();
        assert_eq!(t, ["1.500", "2.000"]);
        assert_eq!(args.last().unwrap(), "/out/x.mp4");
        assert!(args.contains(&"[outa]".to_string()));
        assert!(!args.iter().any(|a| a == "sh" || a == "-c"));
    }

    #[test]
    fn mixed_audio_gets_silent_filler() {
        let plan = plan_segments(&[seg("/v/a.mp4", 0.0, 1.0), seg("/v/b.mp4", 0.0, 1.0)], fake_probe).unwrap();
        let g = build_filter_graph(&plan);
        assert!(g.contains("[0:a:0]"));
        assert!(!g.contains("[1:a:0]"));
        assert!(g.contains("anullsrc"));
        assert!(g.ends_with("[v0][a0][v1][a1]concat=n=2:v=1:a=1[outv][outa]"));
    }

    #[test]
    fn no_audio_anywhere_drops_audio() {
        let plan = plan_segments(&[seg("/v/b.mp4", 0.0, 1.0)], fake_probe).unwrap();
        let g = build_filter_graph(&plan);
        assert!(!g.contains("anullsrc"));
        assert!(g.ends_with("[v0]concat=n=1:v=1:a=0[outv]"));
        let args = build_ffmpeg_args(&plan, Path::new("/o.mp4"));
        assert!(args.iter().any(|a| a == "-an"));
    }

    #[test]
    fn travel_graph_crossfades_pans_grades_and_fades() {
        let mut plan = plan_segments(&[seg("/v/a.mp4", 0.0, 2.0), seg("/v/b.mp4", 0.0, 2.0), seg("/v/a.mp4", 1.0, 3.0)], fake_probe).unwrap();
        plan.width = 1080;
        plan.height = 1920;
        plan.fit = Fit::Crop;
        plan.style = Style {
            pan: true,
            grade: Grade::Warm,
            transition: Some(Transition { style: TransitionStyle::Smooth, sec: 0.4 }),
            music: Some(Music { path: "/m/song.mp3".into(), gain_db: -16.0 }),
            music_no_duck: true,
            natural_gain_db: -9.0,
            fade_sec: 0.5,
            ..Default::default()
        };
        let g = build_filter_graph(&plan);
        assert!(g.contains("x='(iw-ow)/2+(1)*min(70,(iw-ow)/2)*(2*t/2.000-1)'"), "{g}");
        assert!(g.contains("(-1)*min(70"), "second clip drifts the other way");
        assert!(g.contains("eq=contrast=1.06:saturation=1.2"));
        assert!(g.contains("[v0][v1]xfade=transition=fade:duration=0.400:offset=1.600[x1];"));
        assert!(g.contains("[x1][v2]xfade=transition=fade:duration=0.400:offset=3.200[cv];"), "{g}");
        assert!(g.contains("[a0][a1]acrossfade=d=0.400"));
        assert!(g.contains(";[cv]fade=t=in:st=0:d=0.50,fade=t=out:st=4.700:d=0.50,format=yuv420p[outv]"), "{g}");
        // total = 6 - 2 * 0.4 = 5.2s; music steady (no sidechain), natural sound lowered.
        assert!(g.contains("[ca]volume=-9.0dB[sp]") && !g.contains("sidechaincompress"));
        assert!(g.contains("afade=t=out:st=4.700:d=0.50"));
        assert!((plan_total(&plan) - 5.2).abs() < 1e-9);
    }

    #[test]
    fn music_alone_gives_silent_clips_an_audio_track() {
        let mut plan = plan_segments(&[seg("/v/b.mp4", 0.0, 2.0)], fake_probe).unwrap();
        plan.style.music = Some(Music { path: "/m/song.mp3".into(), gain_db: -16.0 });
        let g = build_filter_graph(&plan);
        assert!(g.contains("anullsrc") && g.contains("[outa]"), "{g}");
    }

    #[test]
    fn many_overlays_run_the_filter_graph_on_one_thread() {
        let mut plan = plan_segments(&[seg("/v/a.mp4", 0.0, 2.0), seg("/v/a.mp4", 3.0, 4.0)], fake_probe).unwrap();
        let has = |p: &Plan| {
            let args = build_ffmpeg_args(p, Path::new("/o.mp4"));
            args.windows(2).any(|w| w[0] == "-filter_complex_threads" && w[1] == "1")
        };
        assert!(!has(&plan));
        plan.overlays = (0..=MANY_OVERLAYS).map(|i| Overlay { image: format!("/p/{i}.png").into(), start: 0.0, end: 1.0, y_center_frac: 0.7 }).collect();
        assert!(has(&plan));
    }

    #[test]
    fn raising_the_open_file_limit_never_lowers_it() {
        fn soft() -> libc::rlim_t {
            let mut l = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut l) };
            l.rlim_cur
        }
        let before = soft();
        raise_open_file_limit();
        assert!(soft() >= before);
    }

    #[test]
    fn crop_fit_and_caption_overlays() {
        let mut plan = plan_segments(&[seg("/v/a.mp4", 0.0, 2.0), seg("/v/a.mp4", 3.0, 4.0)], fake_probe).unwrap();
        plan.width = 1080;
        plan.height = 1920;
        plan.fit = Fit::Crop;
        plan.overlays = vec![
            Overlay { image: "/p/000.png".into(), start: 0.0, end: 1.5, y_center_frac: 0.7 },
            Overlay { image: "/p/001.png".into(), start: 1.5, end: 3.0, y_center_frac: 0.7 },
        ];
        let g = build_filter_graph(&plan);
        assert!(g.contains("scale=1080:1920:force_original_aspect_ratio=increase,crop=1080:1920"));
        assert!(!g.contains("pad="));
        assert!(g.contains("concat=n=2:v=1:a=1[cv][outa]"));
        assert!(g.contains("[cv][2:v]overlay=x=(main_w-overlay_w)/2:y=main_h*0.7000-overlay_h/2:enable='between(t,0.000,1.500)'[o0]"));
        assert!(g.ends_with("[o0][3:v]overlay=x=(main_w-overlay_w)/2:y=main_h*0.7000-overlay_h/2:enable='between(t,1.500,3.000)',format=yuv420p[outv]"));
        let args: Vec<String> = build_ffmpeg_args(&plan, Path::new("/o.mp4")).into_iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let inputs: Vec<&String> = args.windows(2).filter(|w| w[0] == "-i").map(|w| &w[1]).collect();
        assert_eq!(inputs, ["/v/a.mp4", "/v/a.mp4", "/p/000.png", "/p/001.png"]);
    }

    #[test]
    fn reel_style_zoom_sfx_music_loudness() {
        let mut plan = plan_segments(&[seg("/v/a.mp4", 0.0, 2.0), seg("/v/a.mp4", 3.0, 4.0)], fake_probe).unwrap();
        plan.width = 1080;
        plan.height = 1920;
        plan.fit = Fit::Crop;
        plan.overlays = vec![Overlay { image: "/p/0.png".into(), start: 0.0, end: 1.0, y_center_frac: 0.7 }];
        plan.style = Style {
            zoom: vec![1.0, 1.15],
            sfx: Some(Sfx { path: "/fx/whoosh.wav".into(), times: vec![1.9], gain_db: -12.0 }),
            music: Some(Music { path: "/m/song.mp3".into(), gain_db: -18.0 }),
            loudnorm: true,
            ..Default::default()
        };
        let g = build_filter_graph(&plan);
        // Segment 0 not zoomed, segment 1 punched in to 1.15x then re-cropped.
        assert!(g.contains("[0:v:0]setpts=PTS-STARTPTS,scale=1080:1920:force_original_aspect_ratio=increase,crop=1080:1920,setsar=1"));
        assert!(g.contains("crop=1080:1920,scale=1242:2208,crop=1080:1920,setsar=1"));
        assert!(g.contains("concat=n=2:v=1:a=1[cv][ca]"));
        // Inputs: 2 segments, 1 overlay (idx 2), music (idx 3), sfx (idx 4).
        assert!(g.contains("[3:a:0]aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo,atrim=duration=3.000,volume=-18.0dB[mus]"));
        assert!(g.contains("[mus][sc]sidechaincompress"));
        assert!(g.contains("[4:a:0]aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo,volume=-12.0dB,anull[s0]"));
        assert!(g.contains("[s0]adelay=delays=1900:all=1[d0]"));
        assert!(g.ends_with("[sp][duck][d0]amix=inputs=3:duration=first:normalize=0,loudnorm=I=-14:TP=-1.5:LRA=11,aresample=48000[outa]"));
        let args: Vec<String> = build_ffmpeg_args(&plan, Path::new("/o.mp4")).into_iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let inputs: Vec<&String> = args.windows(2).filter(|w| w[0] == "-i").map(|w| &w[1]).collect();
        assert_eq!(inputs, ["/v/a.mp4", "/v/a.mp4", "/p/0.png", "/m/song.mp3", "/fx/whoosh.wav"]);
        assert!(args.windows(3).any(|w| w[0] == "-stream_loop" && w[1] == "-1" && w[2] == "-i"));
    }

    #[test]
    fn loudnorm_only_and_default_untouched() {
        let mut plan = plan_segments(&[seg("/v/a.mp4", 0.0, 2.0)], fake_probe).unwrap();
        let plain = build_filter_graph(&plan);
        assert!(plain.ends_with("concat=n=1:v=1:a=1[outv][outa]"));
        plan.style.loudnorm = true;
        assert!(build_filter_graph(&plan).ends_with("concat=n=1:v=1:a=1[outv][ca];[ca]loudnorm=I=-14:TP=-1.5:LRA=11,aresample=48000[outa]"));
    }

    #[test]
    fn output_path_rules() {
        let dir = std::env::temp_dir().join(format!("arm-out-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mp4");
        std::fs::write(&src, b"x").unwrap();
        let srcs = vec![src.clone()];
        let p = |name: &str| dir.join(name).to_string_lossy().into_owned();
        assert!(validate_output_path("rel.mp4", &srcs, false).is_err());
        assert!(validate_output_path(&p("out.mov"), &srcs, false).is_err());
        assert!(validate_output_path("/definitely/missing/dir/out.mp4", &srcs, false).is_err());
        assert!(validate_output_path(&p("src.mp4"), &srcs, true).is_err()); // never a source, even if confirmed
        assert!(validate_output_path(&p("new.mp4"), &srcs, false).is_ok());
        std::fs::write(dir.join("taken.mp4"), b"y").unwrap();
        assert!(validate_output_path(&p("taken.mp4"), &srcs, false).unwrap_err().contains("already exists"));
        assert!(validate_output_path(&p("taken.mp4"), &srcs, true).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    fn gen_clip(ffmpeg: &Path, out: &Path, size: &str, dur: u32, audio: bool) {
        let mut cmd = Command::new(ffmpeg);
        cmd.args(["-y", "-v", "error", "-f", "lavfi", "-i"])
            .arg(format!("testsrc2=size={size}:rate=30:duration={dur}"));
        if audio {
            cmd.args(["-f", "lavfi", "-i"]).arg(format!("sine=frequency=440:duration={dur}"));
            cmd.args(["-c:a", "aac", "-shortest"]);
        }
        let ok = cmd.args(["-c:v", "libx264", "-pix_fmt", "yuv420p"]).arg(out).status().unwrap().success();
        assert!(ok);
    }

    /// End-to-end: real FFmpeg renders single-clip and two-clip exports; outputs are inspected and
    /// sources are verified byte-for-byte unchanged.
    #[test]
    fn real_export_end_to_end() {
        let Some(ffmpeg) = media::find_tool("ffmpeg") else {
            eprintln!("skipped: ffmpeg not installed");
            return;
        };
        let dir = std::env::temp_dir().join(format!("arm-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.mp4");
        let b = dir.join("b.mp4");
        gen_clip(&ffmpeg, &a, "640x360", 3, true);
        gen_clip(&ffmpeg, &b, "320x240", 2, false);
        let (a_before, b_before) = (std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
        let s = |p: &Path| p.to_string_lossy().into_owned();

        // 1) Single trimmed clip.
        let single = ExportRequest {
            segments: vec![seg(&s(&a), 0.5, 2.0)],
            output_path: s(&dir.join("single.mp4")),
            overwrite_confirmed: false,
        };
        let r = export(&single, |_| {}).unwrap();
        assert!((r.duration_sec - 1.5).abs() < 0.15, "single duration {}", r.duration_sec);
        assert_eq!((r.width, r.height), (640, 360));
        assert!(r.has_audio && r.size_bytes > 0);

        // 2) Two clips, b first (no audio, different size), then a.
        let progress = RefCell::new(Vec::new());
        let concat = ExportRequest {
            segments: vec![seg(&s(&b), 0.5, 2.0), seg(&s(&a), 1.0, 3.0)],
            output_path: s(&dir.join("concat.mp4")),
            overwrite_confirmed: false,
        };
        let r = export(&concat, |f| progress.borrow_mut().push(f)).unwrap();
        assert!((r.duration_sec - 3.5).abs() < 0.15, "concat duration {}", r.duration_sec);
        assert_eq!((r.width, r.height), (320, 240));
        assert!(r.has_audio);
        assert_eq!(progress.borrow().last(), Some(&1.0));

        // 3) No silent overwrite; never write over a source.
        assert!(export(&concat, |_| {}).unwrap_err().contains("already exists"));
        let onto_source = ExportRequest { output_path: s(&a), overwrite_confirmed: true, ..concat.clone() };
        assert!(export(&onto_source, |_| {}).is_err());

        // 4) Sources unchanged, no temp files left behind.
        assert_eq!(std::fs::read(&a).unwrap(), a_before);
        assert_eq!(std::fs::read(&b).unwrap(), b_before);
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".partial-"))
            .count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(&dir).ok();
    }
}
