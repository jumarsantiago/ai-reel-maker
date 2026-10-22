//! Auto-edit pipeline orchestration. Every run lives in its own project folder:
//!   source.json, transcript.json, request.json (exact API body, no key), plan.validated.json,
//!   captions/*.png, output.srt, output.mp4, output_transcript.json, render_report.json, log.txt
//! `prepare` and `render` are fully local; the paid planning call is a separate, explicit step.

use crate::captions::{self, CaptionStyle, CaptionTemplate};
use crate::media::{self, MediaInfo};
use crate::plan::{self, EditPlan, PlanSettings, ValidatedPlan};
use crate::render::{self, Fit, Orientation, Overlay, Plan, PlannedSegment};
use crate::transcribe::{self, Transcript};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};


fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, text + "\n").map_err(|e| format!("Cannot write {}: {e}", path.display()))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("Invalid JSON in {}: {e}", path.display()))
}

pub fn log(dir: &Path, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("log.txt")) {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "[{secs}] {line}");
    }
}

/// Creates `<root>/<unix-seconds>-<stem>/`, never reusing an existing folder.
pub fn new_project_dir(root: &Path, info: &MediaInfo) -> Result<PathBuf, String> {
    let stem: String = Path::new(&info.file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().chars().map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' }).take(40).collect())
        .unwrap_or_else(|| "clip".into());
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    for n in 0..100 {
        let name = if n == 0 { format!("{secs}-{stem}") } else { format!("{secs}-{stem}-{n}") };
        let dir = root.join(name);
        if !dir.exists() {
            std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create project folder: {e}"))?;
            return Ok(dir);
        }
    }
    Err("Could not create a unique project folder.".into())
}

#[derive(Serialize, serde::Deserialize)]
pub struct ProjectSettings {
    pub model: String,
    pub language: String,
    pub target_min_sec: u32,
    pub target_max_sec: u32,
    pub width: u32,
    pub height: u32,
}

/// Local step 1: probe, transcribe, and write the exact (unsent) request body for review.
pub fn prepare(video: &str, root: &Path, language: &str, settings: &PlanSettings, model: &str) -> Result<PathBuf, String> {
    let info = media::probe(video)?;
    let dir = new_project_dir(root, &info)?;
    log(&dir, &format!("prepare: source {} ({:.2}s)", info.file_name, info.duration_sec));
    write_json(&dir.join("source.json"), &info)?;
    write_json(
        &dir.join("settings.json"),
        &ProjectSettings {
            model: model.into(),
            language: language.into(),
            target_min_sec: settings.target_min_sec,
            target_max_sec: settings.target_max_sec,
            width: settings.width,
            height: settings.height,
        },
    )?;
    let started = std::time::Instant::now();
    let t = transcribe::transcribe(&info, &dir.join("work"), &transcribe::default_model_path(), language)?;
    log(&dir, &format!("transcribed {} words in {:.1}s, languages {:?}", t.words.len(), started.elapsed().as_secs_f64(), t.languages_detected));
    write_json(&dir.join("transcript.json"), &t)?;
    let body = plan::build_request(&t, info.width, info.height, info.fps, settings, model);
    write_json(&dir.join("request.json"), &body)?;
    log(&dir, "wrote request.json (NOT sent)");
    Ok(dir)
}

fn next_free(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    (2..).map(|n| dir.join(format!("{stem}-{n}.{ext}"))).find(|p| !p.exists()).expect("unbounded")
}

/// Longest-common-subsequence ratio: how much of `expected` appears, in order, in `got`.
pub fn sequence_match(expected: &[String], got: &[String]) -> f64 {
    if expected.is_empty() {
        return 1.0;
    }
    let mut dp = vec![vec![0usize; got.len() + 1]; expected.len() + 1];
    for i in 1..=expected.len() {
        for j in 1..=got.len() {
            dp[i][j] = if expected[i - 1] == got[j - 1] { dp[i - 1][j - 1] + 1 } else { dp[i - 1][j].max(dp[i][j - 1]) };
        }
    }
    dp[expected.len()][got.len()] as f64 / expected.len() as f64
}

#[derive(Serialize)]
pub struct RenderReport {
    pub output: render::ExportResult,
    pub srt: String,
    pub segments: usize,
    pub captions: usize,
    pub planned_duration_sec: f64,
    pub warnings: Vec<String>,
    pub output_transcript_words: Vec<String>,
    /// Fraction of kept transcript words found, in order, when re-transcribing the output.
    pub kept_words_match: f64,
    pub filler_words_in_output: Vec<String>,
}

/// Reel styling choices for a render. `Default` = the "Auto Edit" look (everything on, no music).
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditStyle {
    /// Alternate 100% / 115% punch-in on each cut.
    pub zoom: bool,
    /// Soft whoosh at each cut.
    pub sound_effects: bool,
    /// Highlight the word being spoken in yellow.
    pub highlight_words: bool,
    /// Soft pill captions (cream text on translucent plum, rounded font) instead of the classic outlined look.
    #[serde(default = "default_soft_captions")]
    pub soft_captions: bool,
    /// Normalize to -14 LUFS.
    pub loudness: bool,
    /// Optional background music file (looped, ducked under speech).
    pub music: Option<String>,
    /// Shape of the finished video: vertical 9:16 or horizontal 16:9.
    #[serde(default)]
    pub orientation: Orientation,
    /// Clips of a different shape sit whole over a blurred copy of themselves instead of being cropped.
    #[serde(default)]
    pub fit_blur: bool,
    /// Which ready-made caption look to use (when soft captions are on).
    #[serde(default)]
    pub caption_template: CaptionTemplate,
    /// Caption font file chosen in the app (a system or user font); `None` = the built-in rounded font.
    #[serde(default)]
    pub caption_font: Option<String>,
    #[serde(flatten)]
    pub audio: AudioMix,
    /// Draw the captions into the video (true), or leave them out and rely on the .srt (for CapCut).
    #[serde(default = "default_true")]
    pub burn_captions: bool,
    /// Also render the other shape (vertical and horizontal from one edit).
    #[serde(default)]
    pub both_shapes: bool,
}

fn default_true() -> bool {
    true
}

/// Sound settings shared by Auto Edit and Travel mode (the "CapCut" volume controls).
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AudioMix {
    /// Music level, 0.0-1.0, relative to the levelled voice (1.0 = as loud as the voice).
    pub music_volume: f64,
    /// The video's own sound, 0.0-1.0.
    pub video_volume: f64,
    /// Silence the video's own sound completely.
    pub mute_video: bool,
    /// Lower the music while someone talks.
    pub duck_music: bool,
    /// Fade the music in at the start and out at the end.
    pub music_fade: bool,
    /// Where in the song to start, in seconds.
    pub music_start_sec: f64,
}

impl Default for AudioMix {
    fn default() -> Self {
        Self { music_volume: 0.2, video_volume: 1.0, mute_video: false, duck_music: true, music_fade: true, music_start_sec: 0.0 }
    }
}

impl AudioMix {
    /// The clips' own sound volume for the renderer (`None` when it is left exactly as recorded).
    pub fn video_volume(&self) -> Option<f64> {
        if self.mute_video {
            Some(0.0)
        } else if (self.video_volume - 1.0).abs() > 0.005 {
            Some(self.video_volume.clamp(0.0, 2.0))
        } else {
            None
        }
    }

    pub fn music(&self, path: PathBuf) -> render::Music {
        render::Music {
            path,
            gain_db: render::volume_db(self.music_volume.clamp(0.0, 2.0)),
            start_sec: self.music_start_sec.max(0.0),
            fade: self.music_fade,
        }
    }
}

fn default_soft_captions() -> bool {
    true
}

impl Default for EditStyle {
    fn default() -> Self {
        Self { zoom: true, sound_effects: true, highlight_words: true, soft_captions: true, loudness: true, music: None, orientation: Orientation::Vertical, fit_blur: false, caption_template: CaptionTemplate::Soft, caption_font: None, audio: AudioMix::default(), burn_captions: true, both_shapes: false }
    }
}

impl EditStyle {
    pub fn plain() -> Self {
        Self { zoom: false, sound_effects: false, highlight_words: false, soft_captions: false, loudness: false, music: None, orientation: Orientation::Vertical, fit_blur: false, caption_template: CaptionTemplate::Soft, caption_font: None, audio: AudioMix::default(), burn_captions: true, both_shapes: false }
    }
}

pub const PUNCH_IN: f64 = 1.15;
const SFX_GAIN_DB: f64 = -14.0;
const SFX_LEAD_SEC: f64 = 0.15;
pub const MUSIC_EXTENSIONS: &[&str] = &["mp3", "m4a", "aac", "wav", "aif", "aiff", "flac"];

/// Self-made whoosh (filtered pink noise), generated once locally - no third-party audio.
pub fn whoosh_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("HOME is not set")?;
    let dir = home.join("Library/Application Support/ai-reel-maker/sfx");
    let path = dir.join("whoosh.wav");
    if path.is_file() {
        return Ok(path);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    let ffmpeg = media::find_tool("ffmpeg").ok_or("FFmpeg not found.")?;
    let tmp = dir.join(".whoosh.partial.wav");
    let ok = std::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "anoisesrc=d=0.45:c=pink:a=0.6:r=48000"])
        .args(["-af", "highpass=f=350,lowpass=f=4500,afade=t=in:d=0.18:curve=qsin,afade=t=out:st=0.18:d=0.27:curve=qsin"])
        .args(["-ac", "2", "-ar", "48000"])
        .arg(&tmp)
        .status()
        .map_err(|e| format!("Cannot run FFmpeg: {e}"))?
        .success();
    if !ok {
        return Err("Could not generate the whoosh sound.".into());
    }
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Accepts an audio file for music: allowed extension, exists, and FFprobe finds an audio stream.
pub fn check_music(path: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(path);
    let ext = p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    if !p.is_absolute() || !MUSIC_EXTENSIONS.contains(&ext.as_str()) {
        return Err(format!("Music must be an absolute path to one of: {}.", MUSIC_EXTENSIONS.join(", ")));
    }
    if !p.is_file() {
        return Err(format!("Music file not found: {path}"));
    }
    let ffprobe = media::find_tool("ffprobe").ok_or("FFprobe not found.")?;
    let out = std::process::Command::new(ffprobe)
        .args(["-v", "error", "-select_streams", "a", "-show_entries", "stream=codec_type", "-of", "csv=p=0"])
        .arg(&p)
        .output()
        .map_err(|e| format!("Cannot run FFprobe: {e}"))?;
    if !String::from_utf8_lossy(&out.stdout).contains("audio") {
        return Err("That music file has no audio stream.".into());
    }
    Ok(p)
}

/// Splits a caption's time range across its words: transcript timings when the word counts
/// line up, otherwise proportional to word length. Returns contiguous (start, end) per word.
pub fn caption_word_times(c: &plan::ValidCaption, v: &ValidatedPlan, t: &Transcript) -> Vec<(f64, f64)> {
    let tokens: Vec<&str> = c.text.split_whitespace().collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    let seg = v.segments.iter().find(|s| c.out_start >= s.out_start - 1e-6 && c.out_start < s.out_end);
    let mut starts: Vec<f64> = Vec::new();
    if let Some(seg) = seg {
        let to_out = |src: f64| seg.out_start + (src - seg.start);
        let (a, b) = (seg.start + (c.out_start - seg.out_start), seg.start + (c.out_end - seg.out_start));
        let spoken: Vec<_> = t.words.iter().filter(|w| w.end > a + 0.02 && w.start < b - 0.02).collect();
        if spoken.len() == tokens.len() {
            starts = spoken.iter().map(|w| to_out(w.start).clamp(c.out_start, c.out_end)).collect();
            starts[0] = c.out_start;
        }
    }
    if starts.is_empty() {
        let total: usize = tokens.iter().map(|x| x.chars().count().max(1)).sum();
        let mut acc = 0usize;
        for tok in &tokens {
            starts.push(c.out_start + (c.out_end - c.out_start) * acc as f64 / total as f64);
            acc += tok.chars().count().max(1);
        }
    }
    (0..tokens.len())
        .map(|k| (starts[k], if k + 1 < tokens.len() { starts[k + 1].max(starts[k]) } else { c.out_end }))
        .collect()
}

#[derive(Serialize)]
struct StyleRecord<'a> {
    style: &'a EditStyle,
    zoom_per_segment: Vec<f64>,
    sfx_times: Vec<f64>,
}

/// Local step 2: validate a plan (from Claude, a fixture or Auto-cut) and render the vertical MP4.
/// Everything needed to render a project: the validated plan and the FFmpeg plan with captions drawn.
struct Built {
    plan: Plan,
    v: ValidatedPlan,
    settings: ProjectSettings,
    source: PathBuf,
    srt: Option<PathBuf>,
}

/// Validates the plan and draws the captions and extras into `cap_folder`. Only a `main` build records the
/// validated plan, SRT and style; previews and second-shape renders leave them alone.
fn build_render(dir: &Path, plan_path: &Path, look: &EditStyle, main: bool, cap_folder: &str) -> Result<Built, String> {
    let preview = !main;
    let info: MediaInfo = read_json(&dir.join("source.json"))?;
    let t: Transcript = read_json(&dir.join("transcript.json"))?;
    let settings: ProjectSettings = read_json(&dir.join("settings.json"))?;
    let edit: EditPlan = read_json(plan_path)?;
    log(dir, &format!("render: plan {} style {:?}", plan_path.display(), look));
    let music = look.music.as_deref().map(check_music).transpose()?;

    let v: ValidatedPlan = match plan::validate(&edit, &t) {
        Ok(v) => v,
        Err(errors) => {
            write_json(&dir.join("plan.errors.json"), &errors)?;
            log(dir, &format!("plan rejected: {} error(s)", errors.len()));
            return Err(format!("Plan rejected:\n- {}", errors.join("\n- ")));
        }
    };
    if !preview {
        write_json(&dir.join("plan.validated.json"), &v)?;
    }
    log(dir, &format!("plan valid: {} segments, {} captions, {:.2}s", v.segments.len(), v.captions.len(), v.total_sec));

    // Captions -> PNGs (one per highlighted word, or one per caption) + SRT.
    let cap_dir = dir.join(cap_folder);
    if cap_dir.exists() {
        std::fs::remove_dir_all(&cap_dir).map_err(|e| e.to_string())?; // our own generated PNGs only
    }
    std::fs::create_dir_all(&cap_dir).map_err(|e| e.to_string())?;
    let (out_w, out_h) = look.orientation.size();
    let short = look.orientation.short_side();
    let cap_y = look.orientation.caption_y();
    let font = if look.caption_font.is_some() {
        captions::load_font_choice(look.caption_font.as_deref(), captions::SOFT_FONT)?
    } else if look.soft_captions {
        captions::load_font_for(captions::SOFT_FONT)?
    } else {
        captions::load_font()?
    };
    let cap_style = match (look.soft_captions, look.highlight_words) {
        (true, _) => look.caption_template.style(short),
        (false, true) => CaptionStyle::reels(short),
        (false, false) => CaptionStyle::for_width(short),
    };
    let mut overlays = Vec::new();
    // "Captions for CapCut": the words go only into the .srt, so they can be styled in the editor.
    let burned: &[crate::plan::ValidCaption] = if look.burn_captions { &v.captions } else { &[] };
    for (i, c) in burned.iter().enumerate() {
        if look.highlight_words {
            for (k, (a, b)) in caption_word_times(c, &v, &t).into_iter().enumerate() {
                if b - a < 0.01 {
                    continue;
                }
                let png = cap_dir.join(format!("{i:03}-{k:02}.png"));
                captions::render_words_png(&font, &cap_style, &c.text, Some(k), &png)?;
                overlays.push(Overlay { image: png, start: a, end: b, y_center_frac: cap_y });
            }
        } else {
            let png = cap_dir.join(format!("{i:03}.png"));
            captions::render_png(&font, &cap_style, &c.text, &png)?;
            overlays.push(Overlay { image: png, start: c.out_start, end: c.out_end, y_center_frac: cap_y });
        }
    }
    if let Some(extras) = crate::extras::load(dir)? {
        let (placed, notes) = crate::extras::place(&extras, &v.captions, v.total_sec);
        for n in &notes {
            log(dir, &format!("extras: {n}"));
        }
        log(dir, &format!("extras: {} overlay(s)", placed.len()));
        let xfont = captions::load_font_choice(look.caption_font.as_deref(), captions::SOFT_FONT)?;
        overlays.extend(crate::extras::overlays(&xfont, &placed, look.orientation, &cap_dir)?);
    }
    let srt = if preview {
        None
    } else {
        let srt_path = next_free(dir, "output", "srt");
        std::fs::write(&srt_path, captions::to_srt(&v.captions)).map_err(|e| e.to_string())?;
        Some(srt_path)
    };

    let zoom: Vec<f64> = (0..v.segments.len()).map(|k| if look.zoom && k % 2 == 1 { PUNCH_IN } else { 1.0 }).collect();
    let sfx_times: Vec<f64> = if look.sound_effects {
        v.segments.iter().skip(1).map(|s| (s.out_start - SFX_LEAD_SEC).max(0.0)).collect()
    } else {
        Vec::new()
    };
    let sfx = if sfx_times.is_empty() {
        None
    } else {
        Some(render::Sfx { path: whoosh_path()?, times: sfx_times.clone(), gain_db: SFX_GAIN_DB })
    };
    if !preview {
        write_json(&dir.join("style.json"), &StyleRecord { style: look, zoom_per_segment: zoom.clone(), sfx_times })?;
    }

    let source = PathBuf::from(&info.path);
    let fps = info.fps.round() as u32;
    let render_plan = Plan {
        segments: v
            .segments
            .iter()
            .map(|s| PlannedSegment {
                path: source.clone(),
                in_point: s.start,
                length: s.end - s.start,
                has_audio: info.has_audio,
                width: info.width,
                height: info.height,
            })
            .collect(),
        width: out_w,
        height: out_h,
        fps: if (1..=120).contains(&fps) { fps } else { 30 },
        fit: if look.fit_blur { Fit::Blur } else { Fit::Crop },
        overlays,
        style: render::Style {
            zoom,
            sfx,
            music: music.map(|path| look.audio.music(path)),
            video_volume: look.audio.video_volume(),
            music_no_duck: !look.audio.duck_music,
            loudnorm: look.loudness,
            ..Default::default()
        },
        fast: false,
    };
    Ok(Built { plan: render_plan, v, settings, source, srt })
}

/// Renders a project (plan -> captions -> FFmpeg), then re-transcribes the result as an independent check.
pub fn render_plan(dir: &Path, plan_path: &Path, look: &EditStyle, on_progress: impl Fn(f64)) -> Result<RenderReport, String> {
    let dir = &std::fs::canonicalize(dir).map_err(|e| format!("Project folder {}: {e}", dir.display()))?;
    let Built { plan: render_plan, v, settings, source, srt } = build_render(dir, plan_path, look, true, "captions")?;
    let srt_path = srt.expect("full build writes an SRT");
    let out_path = next_free(dir, "output", "mp4");
    let out = render::validate_output_path(&out_path.to_string_lossy(), &[source], false)?;
    let result = render::render_to(&render_plan, &out, false, on_progress)?;
    log(dir, &format!("rendered {} ({:.2}s, {}x{})", result.output_path, result.duration_sec, result.width, result.height));

    // Independent check: re-transcribe the rendered audio locally and compare with the plan.
    let out_info = media::probe(&result.output_path)?;
    let check = transcribe::transcribe(&out_info, &dir.join("work-output"), &transcribe::default_model_path(), &settings.language)?;
    write_json(&dir.join("output_transcript.json"), &check)?;
    let got: Vec<String> = check.words.iter().map(|w| plan::normalize(&w.text)).filter(|w| !w.is_empty()).collect();
    let fillers = ["um", "uh", "ah", "erm"];
    let report = RenderReport {
        srt: srt_path.to_string_lossy().into_owned(),
        segments: v.segments.len(),
        captions: v.captions.len(),
        planned_duration_sec: v.total_sec,
        warnings: v.warnings.clone(),
        kept_words_match: sequence_match(&v.expected_words, &got),
        filler_words_in_output: got.iter().filter(|w| fillers.contains(&w.as_str())).cloned().collect(),
        output_transcript_words: got,
        output: result,
    };
    write_json(&dir.join("render_report.json"), &report)?;
    log(dir, &format!("kept-words match {:.0}%", report.kept_words_match * 100.0));
    Ok(report)
}

/// Renders the same edit again in the other shape (vertical <-> horizontal) as `output-<shape>.mp4`,
/// with captions and extras placed for that shape. No re-transcription check (the cut is identical).
pub fn render_other_shape(dir: &Path, plan_path: &Path, look: &EditStyle, on_progress: impl Fn(f64)) -> Result<render::ExportResult, String> {
    let dir = &std::fs::canonicalize(dir).map_err(|e| format!("Project folder {}: {e}", dir.display()))?;
    let mut other = look.clone();
    other.orientation = match look.orientation {
        Orientation::Vertical => Orientation::Horizontal,
        Orientation::Horizontal => Orientation::Vertical,
    };
    let name = match other.orientation {
        Orientation::Vertical => "vertical",
        Orientation::Horizontal => "horizontal",
    };
    let built = build_render(dir, plan_path, &other, false, &format!("captions-{name}"))?;
    let out = next_free(dir, &format!("output-{name}"), "mp4");
    let out = render::validate_output_path(&out.to_string_lossy(), &[built.source], false)?;
    let result = render::render_to(&built.plan, &out, false, on_progress)?;
    log(dir, &format!("rendered other shape {} ({:.2}s, {}x{})", result.output_path, result.duration_sec, result.width, result.height));
    Ok(result)
}

/// A cover image for an Auto Edit project: the best moment of `video` (the rendered reel), taken from a quick
/// render of the same edit without captions, hook or tags, so the title is not drawn over other text.
pub fn project_thumbnail(dir: &Path, video: &Path, look: &EditStyle, text: Option<&str>) -> Result<PathBuf, String> {
    let dir = &std::fs::canonicalize(dir).map_err(|e| format!("Project folder {}: {e}", dir.display()))?;
    let t = crate::thumbnail::best_frame_time(video)?;
    let mut clean = look.clone();
    clean.burn_captions = false;
    clean.music = None;
    let mut built = build_render(dir, &dir.join("plan.autocut.json"), &clean, false, "thumbnail-captions")?;
    built.plan.overlays.clear();
    let plan = built.plan.truncated(t + 0.3);
    let tmp = dir.join("thumbnail-frame.mp4");
    let _ = std::fs::remove_file(&tmp); // our own previous work file
    let tmp = render::validate_output_path(&tmp.to_string_lossy(), &[built.source], false)?;
    let r = render::render_to(&plan, &tmp, false, |_| {})?;
    let out = crate::thumbnail::next_thumbnail_path(video.parent().unwrap_or(dir));
    let res = crate::thumbnail::grab_frame(&tmp, (r.duration_sec - 0.15).max(0.0).min(t), &out);
    let _ = std::fs::remove_file(&tmp);
    res?;
    if let Some(text) = text.map(str::trim).filter(|t| !t.is_empty()) {
        crate::thumbnail::add_title(&out, text, look.caption_font.as_deref())?;
    }
    Ok(out)
}

/// [`render_other_shape`] when the style asks for both shapes.
pub fn other_shape_if_wanted(dir: &Path, look: &EditStyle, stage: impl Fn(&str), on_progress: impl Fn(f64)) -> Result<Option<render::ExportResult>, String> {
    if !look.both_shapes {
        return Ok(None);
    }
    stage("Rendering the other shape…");
    render_other_shape(dir, &dir.join("plan.autocut.json"), look, on_progress).map(Some)
}

/// A quick, low-quality render of the first `seconds` of the reel, as `preview.mp4` in the project folder.
pub fn render_preview(dir: &Path, plan_path: &Path, look: &EditStyle, seconds: f64, on_progress: impl Fn(f64)) -> Result<render::ExportResult, String> {
    let dir = &std::fs::canonicalize(dir).map_err(|e| format!("Project folder {}: {e}", dir.display()))?;
    let built = build_render(dir, plan_path, look, false, "preview-captions")?;
    let plan = built.plan.truncated(seconds.clamp(3.0, 30.0));
    let out = dir.join("preview.mp4");
    let _ = std::fs::remove_file(&out); // our own previous preview
    let out = render::validate_output_path(&out.to_string_lossy(), &[built.source], false)?;
    render::render_to(&plan, &out, false, on_progress)
}

/// Free, offline one-shot: transcribe -> rules-based plan (no AI) -> validate -> styled render -> check.
/// `stage` reports human-readable steps; `on_progress` is render progress (0-1).
pub fn auto_cut(
    video: &str,
    root: &Path,
    language: &str,
    look: &EditStyle,
    stage: impl Fn(&str),
    on_progress: impl Fn(f64),
) -> Result<(PathBuf, RenderReport), String> {
    auto_cut_with(video, root, language, look, None, stage, on_progress)
}

/// Like [`auto_cut`], plus an optional hook / tags / CTA (saved as `extras.json` in the project folder).
pub fn auto_cut_with(
    video: &str,
    root: &Path,
    language: &str,
    look: &EditStyle,
    extras: Option<&crate::extras::Extras>,
    stage: impl Fn(&str),
    on_progress: impl Fn(f64),
) -> Result<(PathBuf, RenderReport), String> {
    let dir = auto_cut_prepare(video, root, language, look, extras, &stage)?;
    stage("Rendering…");
    let report = render_plan(&dir, &dir.join("plan.autocut.json"), look, on_progress)?;
    Ok((dir, report))
}

/// First half of Auto Edit: transcribe and plan the cuts and captions, but do not render yet.
/// The plan is `plan.autocut.json` in the returned project folder.
pub fn auto_cut_prepare(
    video: &str,
    root: &Path,
    language: &str,
    look: &EditStyle,
    extras: Option<&crate::extras::Extras>,
    stage: impl Fn(&str),
) -> Result<PathBuf, String> {
    if let Some(m) = &look.music {
        check_music(m)?; // fail fast, before transcribing
    }
    stage("Transcribing…");
    let dir = prepare(video, root, language, &PlanSettings::default(), plan::DEFAULT_MODEL)?;
    write_extras(&dir, extras)?;
    stage("Planning cuts…");
    let t: Transcript = read_json(&dir.join("transcript.json"))?;
    let mut edit = crate::autoplan::auto_plan(&t);
    // The user's remembered spelling fixes.
    if let Ok(app_dir) = crate::prefs::app_dir() {
        let fixes = crate::dictionary::load(&app_dir);
        let n = crate::dictionary::apply_to_plan(&mut edit, &fixes);
        if n > 0 {
            log(&dir, &format!("spelling dictionary fixed {n} caption(s)"));
        }
    }
    let plan_path = dir.join("plan.autocut.json");
    write_json(&plan_path, &edit)?;
    log(&dir, &format!("auto-cut plan (rules, no AI): {} segments, {} removals, {} captions", edit.segments.len(), edit.removals.len(), edit.captions.len()));
    Ok(dir)
}

/// Saves (or removes) the project's `extras.json`, which the render step reads.
pub fn write_extras(dir: &Path, extras: Option<&crate::extras::Extras>) -> Result<(), String> {
    let path = dir.join("extras.json");
    match extras {
        Some(x) => write_json(&path, x),
        None => {
            let _ = std::fs::remove_file(&path);
            Ok(())
        }
    }
}

/// One caption as shown in the review list: its position in the plan, its text, and when it appears in the reel.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionDraft {
    pub index: usize,
    pub text: String,
    /// Seconds on the finished reel's timeline.
    pub start: f64,
    pub end: f64,
}

/// A change the user made to one caption in the review list.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionEdit {
    pub index: usize,
    pub text: String,
    #[serde(default)]
    pub deleted: bool,
}

/// The planned captions with their time on the finished reel (cuts shift every later caption earlier).
pub fn caption_drafts(dir: &Path) -> Result<Vec<CaptionDraft>, String> {
    let t: Transcript = read_json(&dir.join("transcript.json"))?;
    let edit: EditPlan = read_json(&dir.join("plan.autocut.json"))?;
    let v = plan::validate(&edit, &t).map_err(|e| format!("Plan rejected:\n- {}", e.join("\n- ")))?;
    Ok(edit
        .captions
        .iter()
        .enumerate()
        .filter_map(|(index, c)| {
            let seg = v.segments.get(usize::try_from(c.segment_index).ok()?)?;
            let (a, b) = (c.start.max(seg.start), c.end.min(seg.end));
            Some(CaptionDraft { index, text: c.text.clone(), start: seg.out_start + (a - seg.start), end: seg.out_start + (b - seg.start) })
        })
        .collect())
}

/// Spelling fixes to remember from the user's caption edits (deleted captions teach nothing).
pub fn learned_fixes(dir: &Path, edits: &[CaptionEdit]) -> Result<Vec<crate::dictionary::Fix>, String> {
    let backup = dir.join("plan.autocut.original.json");
    let path = if backup.exists() { backup } else { dir.join("plan.autocut.json") };
    let original: EditPlan = read_json(&path)?;
    Ok(edits
        .iter()
        .filter(|e| !e.deleted)
        .filter_map(|e| original.captions.get(e.index).map(|c| crate::dictionary::learn(&c.text, &e.text)))
        .flatten()
        .collect())
}

/// Applies the user's caption edits to `plan.autocut.json` (the first original is kept beside it). Edited
/// captions are marked trusted, so the "words must match the transcript" check no longer rejects them.
pub fn apply_caption_edits(dir: &Path, edits: &[CaptionEdit]) -> Result<usize, String> {
    let plan_path = dir.join("plan.autocut.json");
    let backup = dir.join("plan.autocut.original.json");
    if !backup.exists() {
        std::fs::copy(&plan_path, &backup).map_err(|e| format!("Cannot back up the plan: {e}"))?;
    }
    let mut edit: EditPlan = read_json(&backup)?; // always apply to the untouched plan, so edits are repeatable
    let mut changed = 0;
    let mut drop: Vec<usize> = Vec::new();
    for e in edits {
        let Some(c) = edit.captions.get_mut(e.index) else {
            return Err(format!("Caption {} does not exist.", e.index));
        };
        let text = e.text.trim();
        if e.deleted || text.is_empty() {
            drop.push(e.index);
            changed += 1;
        } else if text != c.text {
            c.text = text.to_string();
            c.trusted = true;
            changed += 1;
        }
    }
    drop.sort_unstable();
    drop.dedup();
    for i in drop.into_iter().rev() {
        edit.captions.remove(i);
    }
    write_json(&plan_path, &edit)?;
    Ok(changed)
}

/// Auto Edit for several timeline clips (in timeline order, with their trims): joins them into one
/// working video first, then runs the same auto-cut on it, so cuts, captions, hook, tags and CTA span the whole reel.
pub fn auto_cut_timeline(
    segments: &[crate::render::SegmentRequest],
    root: &Path,
    language: &str,
    look: &EditStyle,
    extras: Option<&crate::extras::Extras>,
    stage: impl Fn(&str),
    on_progress: impl Fn(f64),
) -> Result<(PathBuf, RenderReport), String> {
    let joined = join_timeline(segments, root, look, &stage)?;
    auto_cut_with(&joined.to_string_lossy(), root, language, look, extras, stage, on_progress)
}

/// Joins the timeline clips (in order, with their trims) into one working video under `root`.
pub fn join_timeline(
    segments: &[crate::render::SegmentRequest],
    root: &Path,
    look: &EditStyle,
    stage: impl Fn(&str),
) -> Result<PathBuf, String> {
    if segments.is_empty() {
        return Err("Timeline is empty. Add clips with the + button first.".into());
    }
    if let Some(m) = &look.music {
        check_music(m)?;
    }
    stage(&format!("Joining {} clip(s)…", segments.len()));
    let work = root.join("timeline-sources");
    std::fs::create_dir_all(&work).map_err(|e| format!("Cannot create {}: {e}", work.display()))?;
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let joined = work.join(format!("timeline-{secs}.mp4"));
    let req = crate::render::ExportRequest {
        segments: segments.to_vec(),
        output_path: joined.to_string_lossy().into_owned(),
        overwrite_confirmed: false,
    };
    crate::render::export(&req, |_| {})?;
    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_with_transcript() -> (PathBuf, Transcript) {
        use crate::transcribe::{Interval, Word};
        let dir = std::env::temp_dir().join(format!("arm-review-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let spec = ["Sobrang", "dali", "lang", "nito,", "promise", "talaga", "mga", "ka-reels!", "Salamat", "po."];
        let mut t0 = 0.1;
        let words: Vec<Word> = spec
            .iter()
            .enumerate()
            .map(|(id, text)| {
                // a long pause before "Salamat" so the plan makes two segments and cuts the pause
                if id == 8 {
                    t0 += 1.5;
                }
                let w = Word { id, text: text.to_string(), start: t0, end: t0 + 0.3 };
                t0 += 0.35;
                w
            })
            .collect();
        let t = Transcript {
            source_id: "clip_1".into(),
            duration_sec: t0 + 0.4,
            language_requested: "tl".into(),
            languages_detected: vec![],
            words,
            silences: Vec::<Interval>::new(),
            speech_chunks: vec![],
            whisper_model: "test".into(),
            timing_method: "test".into(),
        };
        write_json(&dir.join("transcript.json"), &t).unwrap();
        write_json(&dir.join("plan.autocut.json"), &crate::autoplan::auto_plan(&t)).unwrap();
        (dir, t)
    }

    #[test]
    fn review_edits_teach_spelling_fixes() {
        let (dir, _) = project_with_transcript();
        let drafts = caption_drafts(&dir).unwrap();
        let first = &drafts[0];
        let fixed = first.text.replacen("dali", "dalí", 1);
        let edits = vec![
            CaptionEdit { index: first.index, text: fixed.clone(), deleted: false },
            CaptionEdit { index: drafts[1].index, text: String::new(), deleted: true },
        ];
        let learned = learned_fixes(&dir, &edits).unwrap();
        assert_eq!(learned, vec![crate::dictionary::Fix { from: "dali".into(), to: "dalí".into() }], "{} -> {fixed}", first.text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn caption_drafts_edits_and_deletes_keep_the_plan_valid() {
        let (dir, t) = project_with_transcript();
        let drafts = caption_drafts(&dir).unwrap();
        assert!(drafts.len() >= 3, "{drafts:?}");
        assert!(drafts.windows(2).all(|w| w[0].start <= w[1].start), "times on the reel are in order");
        // The pause was cut, so the last caption appears earlier on the reel than in the source.
        let last = drafts.last().unwrap();
        assert!(last.start < t.words[8].start - 1.0, "{last:?}");

        let edits = vec![
            CaptionEdit { index: 0, text: "Sobrang dali lang nito!!".into(), deleted: false },
            CaptionEdit { index: 1, text: "  ".into(), deleted: false }, // empty = removed
            CaptionEdit { index: drafts.len() - 1, text: "Maraming salamat po".into(), deleted: false },
        ];
        assert_eq!(apply_caption_edits(&dir, &edits).unwrap(), 3);
        let after: EditPlan = read_json(&dir.join("plan.autocut.json")).unwrap();
        assert_eq!(after.captions.len(), drafts.len() - 1);
        assert_eq!(after.captions[0].text, "Sobrang dali lang nito!!");
        assert!(after.captions[0].trusted && !after.captions[1].trusted);
        let v = plan::validate(&after, &t).unwrap_or_else(|e| panic!("plan rejected: {e:?}"));
        assert_eq!(v.captions.len(), after.captions.len());
        assert_eq!(v.captions.last().unwrap().text, "Maraming salamat po");

        // Applying again starts from the original plan, so a second edit does not stack on the first.
        let again = vec![CaptionEdit { index: 0, text: "Iba naman".into(), deleted: false }];
        apply_caption_edits(&dir, &again).unwrap();
        let after2: EditPlan = read_json(&dir.join("plan.autocut.json")).unwrap();
        assert_eq!(after2.captions.len(), drafts.len());
        assert_eq!(after2.captions[0].text, "Iba naman");
        assert_eq!(after2.captions[1].text, drafts[1].text);
        assert!(apply_caption_edits(&dir, &[CaptionEdit { index: 999, text: "x".into(), deleted: false }]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn word_times_follow_transcript_or_length() {
        use crate::plan::{ValidCaption, ValidSegment};
        use crate::transcribe::{Interval, Word};
        let w = |id, text: &str, start, end| Word { id, text: text.into(), start, end };
        let t = Transcript {
            source_id: "clip_1".into(), duration_sec: 20.0, language_requested: "auto".into(), languages_detected: vec![],
            words: vec![w(0, "Pour", 10.0, 10.3), w(1, "the", 10.3, 10.5), w(2, "milk,", 10.5, 11.0)],
            silences: Vec::<Interval>::new(), speech_chunks: vec![], whisper_model: "t".into(), timing_method: "t".into(),
        };
        let v = ValidatedPlan {
            segments: vec![ValidSegment { start: 9.9, end: 11.1, out_start: 4.0, out_end: 5.2 }],
            captions: vec![], total_sec: 1.2, removals_checked: 0, warnings: vec![], expected_words: vec![],
        };
        // Counts match -> transcript timing (mapped to output), contiguous, ends at caption end.
        let c = ValidCaption { text: "Pour the milk,".into(), out_start: 4.0, out_end: 5.2 };
        let times = caption_word_times(&c, &v, &t);
        let r = |x: f64| (x * 100.0).round() / 100.0;
        let rounded: Vec<(f64, f64)> = times.iter().map(|(a, b)| (r(*a), r(*b))).collect();
        assert_eq!(rounded, vec![(4.0, 4.4), (4.4, 4.6), (4.6, 5.2)]);
        // Counts differ (corrected text) -> proportional to word length.
        let c = ValidCaption { text: "Pour milk".into(), out_start: 4.0, out_end: 5.2 };
        let times = caption_word_times(&c, &v, &t);
        assert_eq!(times.len(), 2);
        assert!((times[0].1 - 4.0 - 1.2 * 4.0 / 8.0).abs() < 1e-9);
        assert_eq!(times[1].1, 5.2);
    }

    #[test]
    fn music_check_rejects_bad_paths() {
        assert!(check_music("relative.mp3").is_err());
        assert!(check_music("/tmp/notes.txt").is_err());
        assert!(check_music("/definitely/missing/song.mp3").is_err());
    }

    #[test]
    fn lcs_ratio() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(sequence_match(&s(&["a", "b", "c"]), &s(&["a", "x", "b", "c"])), 1.0);
        assert!((sequence_match(&s(&["a", "b", "c", "d"]), &s(&["b", "a"])) - 0.25).abs() < 1e-9);
        assert_eq!(sequence_match(&[], &s(&["a"])), 1.0);
    }
}
