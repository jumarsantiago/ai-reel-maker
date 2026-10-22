//! Travel mode: turns silent b-roll clips into a vertical reel without cutting anything out for "dead air".
//! Each clip is capped to its best few seconds (the middle), filled to 9:16 with a slow drift, given one colour
//! look, and joined with crossfades or slides. Original sound is kept (quietly under optional music) so a voice-over
//! can be added later in an editor. Hook / tags / CTA come from [`crate::extras`], same as Auto Edit.

use crate::autoedit::{check_music, log};
use crate::captions;
use crate::extras::{self, Extras};
use crate::media;
use crate::render::{self, Fit, Grade, Music, SegmentRequest, Style, Transition, TransitionStyle};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const WIDTH: u32 = 1080;
const HEIGHT: u32 = 1920;
const MUSIC_GAIN_DB: f64 = -16.0;
const NATURAL_GAIN_DB: f64 = -9.0;
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
}

impl Default for TravelStyle {
    fn default() -> Self {
        Self { clip_sec: 4.0, transition: "smooth".into(), pan: true, grade: "warm".into(), fade: true, music: None }
    }
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
    if segments.is_empty() {
        return Err("Timeline is empty. Add clips with the + button first.".into());
    }
    let music = style.music.as_deref().map(check_music).transpose()?;
    stage(&format!("Preparing {} clip(s)…", segments.len()));
    let reqs: Vec<SegmentRequest> = segments.iter().map(|s| capped(s, style.clip_sec)).collect();
    let mut plan = render::plan_segments(&reqs, media::probe)?;
    plan.width = WIDTH;
    plan.height = HEIGHT;
    plan.fit = Fit::Crop;
    plan.style = Style {
        pan: style.pan,
        grade: grade_of(&style.grade),
        transition: transition_of(&style.transition),
        music: music.map(|path| Music { path, gain_db: MUSIC_GAIN_DB }),
        music_no_duck: true,
        natural_gain_db: NATURAL_GAIN_DB,
        fade_sec: if style.fade { FADE_SEC } else { 0.0 },
        ..Default::default()
    };

    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let dir = root.join(format!("{secs}-travel"));
    let cap_dir = dir.join("captions");
    std::fs::create_dir_all(&cap_dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    let total = render::plan_total(&plan);
    if let Some(x) = extras.filter(|x| x.hook.is_some() || x.cta.is_some() || !x.tags.is_empty()) {
        let (placed, notes) = extras::place(x, &[], total);
        for n in &notes {
            log(&dir, &format!("extras: {n}"));
        }
        let font = captions::load_font_for(captions::SOFT_FONT)?;
        plan.overlays = extras::overlays(&font, &placed, WIDTH, &cap_dir)?;
        std::fs::write(dir.join("extras.json"), serde_json::to_string_pretty(x).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    }
    std::fs::write(dir.join("style.json"), serde_json::to_string_pretty(style).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    log(&dir, &format!("travel: {} clips, {:.1}s, style {:?}", plan.segments.len(), total, style));

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
