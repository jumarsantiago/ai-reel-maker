//! Travel mode: turns silent b-roll clips into a vertical reel without cutting anything out for "dead air".
//! Each clip is capped to its best few seconds (the middle), filled to 9:16 with a slow drift, given one colour
//! look, and joined with crossfades or slides. Original sound is kept (quietly under optional music) so a voice-over
//! can be added later in an editor. Hook / tags / CTA come from [`crate::extras`], same as Auto Edit.

use crate::autoedit::{check_music, log, AudioMix};
use crate::captions;
use crate::extras::{self, Extras};
use crate::media;
use crate::render::{self, Fit, Grade, Orientation, SegmentRequest, Style, Transition, TransitionStyle};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const TRANSITION_SEC: f64 = 0.45;
const FADE_SEC: f64 = 0.6;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TravelStyle {
    /// Longest each clip may run; longer clips keep their middle. 0 keeps every clip's trim as it is.
    pub clip_sec: f64,
    /// "smooth" (cross-dissolve), "dynamic" (varied slides and zooms) or "none" (hard cuts).
    pub transition: String,
    /// Slow sideways drift on each clip.
    pub pan: bool,
    /// "natural", "warm" or "vivid".
    pub grade: String,
    /// Fade in at the start and out at the end.
    pub fade: bool,
    /// Optional music file (looped, steady level).
    pub music: Option<String>,
    /// Vertical 9:16 or horizontal 16:9.
    pub orientation: Orientation,
    /// "fill" crops each clip to the frame; "blur" keeps the whole picture over a blurred copy of itself.
    pub fit: String,
    /// Font file for the hook, tags and CTA (a system or user font); `None` = built-in rounded font.
    pub font: Option<String>,
    /// Music and video-sound volumes. B-roll sound defaults to a quiet bed under the music.
    #[serde(flatten)]
    pub audio: AudioMix,
    /// Also render the other shape (vertical and horizontal).
    pub both_shapes: bool,
    /// Keep the sharpest, steadiest stretch of each clip instead of its middle.
    pub smart_pick: bool,
    /// Cut on the music's beat (needs music with a steady pulse).
    pub beat_sync: bool,
}

impl Default for TravelStyle {
    fn default() -> Self {
        Self {
            clip_sec: 4.0,
            transition: "smooth".into(),
            pan: true,
            grade: "warm".into(),
            fade: true,
            music: None,
            orientation: Orientation::Vertical,
            fit: "fill".into(),
            font: None,
            audio: AudioMix { music_volume: 0.5, video_volume: 0.35, duck_music: false, ..AudioMix::default() },
            both_shapes: false,
            smart_pick: true,
            beat_sync: false,
        }
    }
}

/// Where a `len`-second window should start inside `req` to get its sharpest, steadiest stretch: frames are
/// scored for sharpness (focus) minus movement (shake), with a slight preference for the middle.
pub fn best_start(req: &SegmentRequest, len: f64) -> Result<f64, String> {
    let avail = req.out_point - req.in_point;
    if len >= avail - 0.05 {
        return Ok(req.in_point);
    }
    let info = media::probe(&req.source_path)?;
    let fps = (80.0 / avail).clamp(0.5, 4.0);
    let s = crate::frames::sample_gray(Path::new(&req.source_path), req.in_point, avail, fps, 96, info.width, info.height)?;
    let n = s.frames.len();
    let sharp: Vec<f64> = s.frames.iter().map(|f| crate::frames::sharpness(f, s.width, s.height)).collect();
    let mut motion: Vec<f64> = (0..n).map(|i| if i == 0 { 0.0 } else { crate::frames::motion(&s.frames[i - 1], &s.frames[i]) }).collect();
    if n > 1 {
        motion[0] = motion[1];
    }
    let (ms, mm) = (sharp.iter().cloned().fold(1e-9, f64::max), motion.iter().cloned().fold(1e-9, f64::max));
    let score: Vec<f64> = (0..n).map(|i| sharp[i] / ms - 0.6 * motion[i] / mm).collect();
    let k = ((len * fps).round() as usize).clamp(1, n);
    let mid = (n - k) as f64 / 2.0;
    let best = (0..=n - k)
        .map(|j| {
            let mean = score[j..j + k].iter().sum::<f64>() / k as f64;
            (j, mean - 0.1 * (j as f64 - mid).abs() / n as f64)
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(j, _)| j)
        .unwrap_or(0);
    Ok((req.in_point + best as f64 / fps).min(req.out_point - len))
}

/// Caps `req` to `cap` seconds, keeping the middle. `cap <= 0` leaves it alone.
pub fn capped(req: &SegmentRequest, cap: f64) -> SegmentRequest {
    let len = req.out_point - req.in_point;
    if cap <= 0.0 || len <= cap {
        return req.clone();
    }
    let start = req.in_point + (len - cap) / 2.0;
    SegmentRequest { source_path: req.source_path.clone(), in_point: start, out_point: start + cap }
}

fn grade_of(name: &str) -> Grade {
    match name {
        "warm" => Grade::Warm,
        "vivid" => Grade::Vivid,
        _ => Grade::None,
    }
}

fn transition_of(name: &str) -> Option<Transition> {
    match name {
        "none" => None,
        "dynamic" => Some(Transition { style: TransitionStyle::Dynamic, sec: TRANSITION_SEC }),
        _ => Some(Transition { style: TransitionStyle::Smooth, sec: TRANSITION_SEC }),
    }
}

/// Builds the travel reel from the timeline clips (in order). Returns the project folder and the render result.
pub fn travel_edit(
    segments: &[SegmentRequest],
    root: &Path,
    style: &TravelStyle,
    extras: Option<&Extras>,
    stage: impl Fn(&str),
    on_progress: impl Fn(f64),
) -> Result<(PathBuf, render::ExportResult), String> {
    travel_render(segments, root, style, extras, None, stage, on_progress)
}

/// A quick, low-quality render of the first `seconds` of the travel reel (in `root/previews/travel`).
pub fn travel_preview(
    segments: &[SegmentRequest],
    root: &Path,
    style: &TravelStyle,
    extras: Option<&Extras>,
    seconds: f64,
    on_progress: impl Fn(f64),
) -> Result<render::ExportResult, String> {
    travel_render(segments, root, style, extras, Some(seconds.clamp(3.0, 30.0)), |_| {}, on_progress).map(|(_, r)| r)
}

fn travel_render(
    segments: &[SegmentRequest],
    root: &Path,
    style: &TravelStyle,
    extras: Option<&Extras>,
    preview_sec: Option<f64>,
    stage: impl Fn(&str),
    on_progress: impl Fn(f64),
) -> Result<(PathBuf, render::ExportResult), String> {
    if segments.is_empty() {
        return Err("Timeline is empty. Add clips with the + button first.".into());
    }
    let music = style.music.as_deref().map(check_music).transpose()?;
    stage(&format!("Preparing {} clip(s)…", segments.len()));
    // Beat sync: each clip lasts a whole number of beats (plus the crossfade overlap), and the song starts
    // on a beat, so every cut lands on the music.
    let grid = match (&music, style.beat_sync) {
        (Some(m), true) => crate::beats::detect(m, style.audio.music_start_sec)?.filter(|g| g.confidence >= 0.1),
        _ => None,
    };
    let mut transition = transition_of(&style.transition);
    if let (Some(g), Some(t)) = (grid, transition.as_mut()) {
        t.sec = t.sec.min(0.5 * g.period); // short enough that the renderer never has to shrink it
    }
    let xf = transition.map(|t| t.sec).unwrap_or(0.0);
    let mut reqs: Vec<SegmentRequest> = Vec::with_capacity(segments.len());
    for s in segments {
        let avail = s.out_point - s.in_point;
        let want = match grid {
            Some(g) => {
                let fit = ((avail - xf) / g.period).floor().max(1.0);
                let beats = if style.clip_sec > 0.0 { (style.clip_sec / g.period).round().clamp(1.0, fit) } else { fit };
                beats * g.period + xf
            }
            None if style.clip_sec > 0.0 => style.clip_sec,
            None => avail,
        };
        if want >= avail - 0.01 {
            reqs.push(s.clone());
            continue;
        }
        let start = if style.smart_pick { best_start(s, want)? } else { s.in_point + (avail - want) / 2.0 };
        reqs.push(SegmentRequest { source_path: s.source_path.clone(), in_point: start, out_point: start + want });
    }
    let mut plan = render::plan_segments(&reqs, media::probe)?;
    let (width, height) = style.orientation.size();
    plan.width = width;
    plan.height = height;
    plan.fit = if style.fit == "blur" { Fit::Blur } else { Fit::Crop };
    plan.style = Style {
        pan: style.pan,
        grade: grade_of(&style.grade),
        transition,
        music: music.map(|path| {
            let mut m = style.audio.music(path);
            if let Some(g) = grid {
                m.start_sec += g.phase; // the first beat at 0:00
            }
            m
        }),
        music_no_duck: !style.audio.duck_music,
        video_volume: style.audio.video_volume(),
        fade_sec: if style.fade { FADE_SEC } else { 0.0 },
        ..Default::default()
    };

    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let dir = match preview_sec {
        Some(_) => {
            let d = root.join("previews").join("travel");
            let _ = std::fs::remove_dir_all(&d); // our own previous preview
            d
        }
        None => (1..).map(|n| if n == 1 { root.join(format!("{secs}-travel")) } else { root.join(format!("{secs}-travel-{n}")) }).find(|p| !p.exists()).expect("unbounded"),
    };
    let cap_dir = dir.join("captions");
    std::fs::create_dir_all(&cap_dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    let total = render::plan_total(&plan);
    if let Some(x) = extras.filter(|x| !x.motion.is_empty()) {
        let (placed, notes) = crate::motion::place(&x.motion, &[], total);
        for n in &notes {
            log(&dir, &format!("motion: {n}"));
        }
        if !placed.is_empty() {
            crate::motion::ensure_kit_enabled(x.use_kit)?;
            stage(&format!("Animating {} graphic(s)…", placed.len()));
            let kit = crate::motion::Kit::find(x.kit_dir.as_deref())?;
            plan.video_overlays = crate::motion::overlays(&kit, &placed, style.orientation, &crate::motion::cache_dir()?)?;
        }
    }
    if let Some(x) = extras.filter(|x| x.hook.is_some() || x.cta.is_some() || !x.tags.is_empty()) {
        let (placed, notes) = extras::place(x, &[], total, style.orientation);
        for n in &notes {
            log(&dir, &format!("extras: {n}"));
        }
        let font = captions::load_font_choice(style.font.as_deref(), captions::SOFT_FONT)?;
        plan.overlays = extras::overlays(&font, &placed, style.orientation, &cap_dir)?;
        std::fs::write(dir.join("extras.json"), serde_json::to_string_pretty(x).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    }
    std::fs::write(dir.join("style.json"), serde_json::to_string_pretty(style).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    log(&dir, &format!("travel: {} clips, {:.1}s, style {:?}", plan.segments.len(), total, style));
    match (style.beat_sync, grid) {
        (true, Some(g)) => log(&dir, &format!("beat sync: {:.1} BPM, first beat {:.2}s, confidence {:.2}", g.bpm(), g.phase, g.confidence)),
        (true, None) => log(&dir, "beat sync: no steady beat found (or no music); cut by clip length instead"),
        _ => {}
    }

    if let Some(sec) = preview_sec {
        plan = plan.truncated(sec);
    }
    stage("Rendering…");
    let sources: Vec<PathBuf> = plan.segments.iter().map(|s| s.path.clone()).collect();
    let out = render::validate_output_path(&dir.join("output.mp4").to_string_lossy(), &sources, false)?;
    let result = render::render_to(&plan, &out, false, on_progress)?;
    log(&dir, &format!("rendered {} ({:.2}s, {}x{})", result.output_path, result.duration_sec, result.width, result.height));
    Ok((dir, result))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(a: f64, b: f64) -> SegmentRequest {
        SegmentRequest { source_path: "/v/a.mp4".into(), in_point: a, out_point: b }
    }

    #[test]
    fn long_clips_keep_their_middle() {
        let c = capped(&req(10.0, 30.0), 4.0);
        assert_eq!((c.in_point, c.out_point), (18.0, 22.0));
        let short = capped(&req(1.0, 3.0), 4.0);
        assert_eq!((short.in_point, short.out_point), (1.0, 3.0));
        assert_eq!(capped(&req(0.0, 60.0), 0.0).out_point, 60.0);
    }

    #[test]
    fn smart_pick_avoids_the_blurry_shaky_part() {
        let Some(ffmpeg) = media::find_tool("ffmpeg") else { return };
        let dir = std::env::temp_dir().join(format!("arm-pick-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let v = dir.join("v.mp4");
        // Sharp 0-5s, blurred 5-12s.
        assert!(std::process::Command::new(&ffmpeg)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25:duration=12",
                   "-vf", "boxblur=enable='gte(t,5)':luma_radius=18:luma_power=3", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&v)
            .status()
            .unwrap()
            .success());
        let r = SegmentRequest { source_path: v.to_string_lossy().into_owned(), in_point: 0.0, out_point: 12.0 };
        let s = best_start(&r, 4.0).unwrap();
        assert!(s + 4.0 <= 5.6, "picked {s}..{} which runs into the blur", s + 4.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Beat sync with a 120 BPM click track: every clip is a whole number of beats plus the overlap.
    #[test]
    fn real_beat_synced_reel() {
        let Some(ffmpeg) = media::find_tool("ffmpeg") else { return };
        let dir = std::env::temp_dir().join(format!("arm-beatreel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let v = dir.join("v.mp4");
        let m = dir.join("m.m4a");
        assert!(std::process::Command::new(&ffmpeg)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25:duration=20", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&v).status().unwrap().success());
        assert!(std::process::Command::new(&ffmpeg)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "aevalsrc=sin(2*PI*1200*t)*exp(-50*mod(t+5\\,0.5)):s=44100:d=30", "-c:a", "aac"])
            .arg(&m).status().unwrap().success());
        let s = |a: f64, b: f64| SegmentRequest { source_path: v.to_string_lossy().into_owned(), in_point: a, out_point: b };
        let style = TravelStyle { music: Some(m.to_string_lossy().into_owned()), beat_sync: true, clip_sec: 3.0, ..Default::default() };
        let root = dir.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let (proj, r) = travel_edit(&[s(0.0, 6.0), s(6.0, 12.0), s(12.0, 20.0)], &root, &style, None, |_| {}, |_| {}).unwrap();
        let log = std::fs::read_to_string(proj.join("log.txt")).unwrap();
        let bpm: f64 = log.split("beat sync: ").nth(1).and_then(|r| r.split(' ').next()).and_then(|x| x.parse().ok()).expect("beat sync line");
        assert!((bpm - 120.0).abs() < 1.5, "{log}");
        // 3 clips of 6 beats (3.0s) + 0.25s overlap each, minus two 0.25s crossfades = 9.25s.
        assert!((r.duration_sec - 9.25).abs() < 0.2, "duration {}", r.duration_sec);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn style_names_map_to_render_options() {
        assert_eq!(grade_of("warm"), Grade::Warm);
        assert_eq!(grade_of("anything"), Grade::None);
        assert!(transition_of("none").is_none());
        assert_eq!(transition_of("dynamic").unwrap().style, TransitionStyle::Dynamic);
        assert_eq!(transition_of("smooth").unwrap().style, TransitionStyle::Smooth);
    }

    #[test]
    fn style_json_uses_defaults_for_missing_fields() {
        let s: TravelStyle = serde_json::from_str(r#"{"grade":"vivid"}"#).unwrap();
        assert_eq!(s.grade, "vivid");
        assert_eq!(s.clip_sec, 4.0);
        assert!(s.pan && s.fade);
    }

    /// Horizontal 16:9 from mixed portrait/landscape clips, blurred-background fit.
    #[test]
    fn real_horizontal_reel_with_blur_fit() {
        let Some(ffmpeg) = media::find_tool("ffmpeg") else {
            eprintln!("skipped: ffmpeg not installed");
            return;
        };
        let dir = std::env::temp_dir().join(format!("arm-travel-h-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut segs = Vec::new();
        for (i, size) in ["640x360", "360x640"].into_iter().enumerate() {
            let p = dir.join(format!("h{i}.mp4"));
            let ok = std::process::Command::new(&ffmpeg)
                .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
                .arg(format!("testsrc2=size={size}:rate=25:duration=3"))
                .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
                .arg(&p)
                .status()
                .unwrap()
                .success();
            assert!(ok);
            segs.push(SegmentRequest { source_path: p.to_string_lossy().into_owned(), in_point: 0.0, out_point: 3.0 });
        }
        let style = TravelStyle { orientation: Orientation::Horizontal, fit: "blur".into(), ..Default::default() };
        let ex = Extras { hook: Some("HELLO".into()), cta: Some("LINK BELOW".into()), ..Default::default() };
        let root = dir.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let (_, r) = travel_edit(&segs, &root, &style, Some(&ex), |_| {}, |_| {}).unwrap();
        assert_eq!((r.width, r.height), (1920, 1080));
        assert!((r.duration_sec - 5.55).abs() < 0.3, "duration {}", r.duration_sec);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn real_travel_preview_is_short_and_fast() {
        let Some(ffmpeg) = media::find_tool("ffmpeg") else { return };
        let dir = std::env::temp_dir().join(format!("arm-travel-p-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("long.mp4");
        assert!(std::process::Command::new(&ffmpeg)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25:duration=30", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&p)
            .status()
            .unwrap()
            .success());
        let s = |a: f64, b: f64| SegmentRequest { source_path: p.to_string_lossy().into_owned(), in_point: a, out_point: b };
        let segs = vec![s(0.0, 6.0), s(6.0, 12.0), s(12.0, 18.0), s(18.0, 24.0)];
        let root = dir.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let r = travel_preview(&segs, &root, &TravelStyle::default(), None, 6.0, |_| {}).unwrap();
        assert!((r.duration_sec - 6.0).abs() < 0.25, "preview {}", r.duration_sec);
        // A second preview replaces the first instead of failing on an existing file.
        travel_preview(&segs, &root, &TravelStyle::default(), None, 6.0, |_| {}).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Real render: silent and noisy landscape clips become one vertical reel with crossfades.
    #[test]
    fn real_travel_reel_from_silent_and_noisy_clips() {
        let Some(ffmpeg) = media::find_tool("ffmpeg") else {
            eprintln!("skipped: ffmpeg not installed");
            return;
        };
        let dir = std::env::temp_dir().join(format!("arm-travel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut srcs = Vec::new();
        for (i, (size, dur, audio)) in [("640x360", 6, false), ("640x360", 3, true), ("360x640", 5, false)].into_iter().enumerate() {
            let p = dir.join(format!("c{i}.mp4"));
            let mut cmd = std::process::Command::new(&ffmpeg);
            cmd.args(["-y", "-v", "error", "-f", "lavfi", "-i"]).arg(format!("testsrc2=size={size}:rate=25:duration={dur}"));
            if audio {
                cmd.args(["-f", "lavfi", "-i"]).arg(format!("sine=frequency=330:duration={dur}"));
            }
            cmd.args(["-c:v", "libx264", "-pix_fmt", "yuv420p"]);
            if audio {
                cmd.args(["-c:a", "aac", "-shortest"]);
            }
            assert!(cmd.arg(&p).status().unwrap().success());
            srcs.push(p);
        }
        let segs: Vec<SegmentRequest> = srcs
            .iter()
            .zip([6.0, 3.0, 5.0])
            .map(|(p, d)| SegmentRequest { source_path: p.to_string_lossy().into_owned(), in_point: 0.0, out_point: d })
            .collect();
        let ex = Extras { hook: Some("HELLO".into()), cta: Some("LINK BELOW".into()), ..Default::default() };
        let style = TravelStyle { transition: "dynamic".into(), ..Default::default() };
        let root = dir.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let (proj, r) = travel_edit(&segs, &root, &style, Some(&ex), |_| {}, |_| {}).unwrap();
        // clips capped to 4s: 4 + 3 + 4 = 11s, minus two 0.45s crossfades.
        assert!((r.duration_sec - 10.1).abs() < 0.3, "duration {}", r.duration_sec);
        assert_eq!((r.width, r.height), (1080, 1920));
        assert!(r.has_audio, "silent clips still get an audio track");
        assert!(proj.join("style.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
