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
    let any_audio = plan.segments.iter().any(|s| s.has_audio);
    let mut graph = String::new();
    let mut concat_inputs = String::new();
    for (i, s) in plan.segments.iter().enumerate() {
        graph += &format!(
            "[{i}:v:0]setpts=PTS-STARTPTS,scale={w}:{h}:force_original_aspect_ratio=decrease,\
             pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps={fps},format=yuv420p[v{i}];"
        );
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
    if any_audio {
        graph += &format!("{concat_inputs}concat=n={n}:v=1:a=1[outv][outa]");
    } else {
        graph += &format!("{concat_inputs}concat=n={n}:v=1:a=0[outv]");
    }
    graph
}

pub fn build_ffmpeg_args(plan: &Plan, output: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-hide_banner", "-nostdin", "-v", "error", "-progress", "pipe:1", "-nostats"]
        .iter()
        .map(OsString::from)
        .collect();
    for s in &plan.segments {
        args.extend(["-ss".into(), secs(s.in_point).into(), "-t".into(), secs(s.length).into(), "-i".into()]);
        args.push(s.path.clone().into_os_string());
    }
    args.push("-filter_complex".into());
    args.push(build_filter_graph(plan).into());
    args.extend(["-map", "[outv]"].map(OsString::from));
    if plan.segments.iter().any(|s| s.has_audio) {
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
    let ffmpeg = media::find_tool("ffmpeg").ok_or("FFmpeg not found. Install it (e.g. `brew install ffmpeg`).")?;
    let temp = temp_path_for(&out);
    let total: f64 = plan.segments.iter().map(|s| s.length).sum();

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
    if out.exists() && !req.overwrite_confirmed {
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
