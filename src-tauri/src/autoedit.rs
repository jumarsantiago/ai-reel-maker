//! Auto-edit pipeline orchestration. Every run lives in its own project folder:
//!   source.json, transcript.json, request.json (exact API body, no key), plan.validated.json,
//!   captions/*.png, output.srt, output.mp4, output_transcript.json, render_report.json, log.txt
//! `prepare` and `render` are fully local; the paid planning call is a separate, explicit step.

use crate::captions::{self, CaptionStyle};
use crate::media::{self, MediaInfo};
use crate::plan::{self, EditPlan, PlanSettings, ValidatedPlan};
use crate::render::{self, Fit, Overlay, Plan, PlannedSegment};
use crate::transcribe::{self, Transcript};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const CAPTION_Y_FRAC: f64 = 0.70;

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

/// Local step 2: validate a plan (from Claude or a fixture) and render the vertical MP4.
pub fn render_plan(dir: &Path, plan_path: &Path, on_progress: impl Fn(f64)) -> Result<RenderReport, String> {
    let dir = &std::fs::canonicalize(dir).map_err(|e| format!("Project folder {}: {e}", dir.display()))?;
    let info: MediaInfo = read_json(&dir.join("source.json"))?;
    let t: Transcript = read_json(&dir.join("transcript.json"))?;
    let settings: ProjectSettings = read_json(&dir.join("settings.json"))?;
    let edit: EditPlan = read_json(plan_path)?;
    log(dir, &format!("render: plan {}", plan_path.display()));

    let v: ValidatedPlan = match plan::validate(&edit, &t) {
        Ok(v) => v,
        Err(errors) => {
            write_json(&dir.join("plan.errors.json"), &errors)?;
            log(dir, &format!("plan rejected: {} error(s)", errors.len()));
            return Err(format!("Plan rejected:\n- {}", errors.join("\n- ")));
        }
    };
    write_json(&dir.join("plan.validated.json"), &v)?;
    log(dir, &format!("plan valid: {} segments, {} captions, {:.2}s", v.segments.len(), v.captions.len(), v.total_sec));

    // Captions -> PNGs + SRT.
    let cap_dir = dir.join("captions");
    std::fs::create_dir_all(&cap_dir).map_err(|e| e.to_string())?;
    let font = captions::load_font()?;
    let style = CaptionStyle::for_width(settings.width);
    let mut overlays = Vec::new();
    for (i, c) in v.captions.iter().enumerate() {
        let png = cap_dir.join(format!("{i:03}.png"));
        captions::render_png(&font, &style, &c.text, &png)?;
        overlays.push(Overlay { image: png, start: c.out_start, end: c.out_end, y_center_frac: CAPTION_Y_FRAC });
    }
    let srt_path = next_free(dir, "output", "srt");
    std::fs::write(&srt_path, captions::to_srt(&v.captions)).map_err(|e| e.to_string())?;

    let source = PathBuf::from(&info.path);
    let fps = info.fps.round() as u32;
    let render_plan = Plan {
        segments: v
            .segments
            .iter()
            .map(|s| PlannedSegment { path: source.clone(), in_point: s.start, length: s.end - s.start, has_audio: info.has_audio })
            .collect(),
        width: settings.width,
        height: settings.height,
        fps: if (1..=120).contains(&fps) { fps } else { 30 },
        fit: Fit::Crop,
        overlays,
    };
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

/// Free, offline one-shot: transcribe -> rules-based plan (no AI) -> validate -> render -> check.
/// `stage` reports human-readable steps; `on_progress` is render progress (0-1).
pub fn auto_cut(
    video: &str,
    root: &Path,
    language: &str,
    stage: impl Fn(&str),
    on_progress: impl Fn(f64),
) -> Result<(PathBuf, RenderReport), String> {
    stage("Transcribing…");
    let dir = prepare(video, root, language, &PlanSettings::default(), plan::DEFAULT_MODEL)?;
    stage("Planning cuts…");
    let t: Transcript = read_json(&dir.join("transcript.json"))?;
    let edit = crate::autoplan::auto_plan(&t);
    let plan_path = dir.join("plan.autocut.json");
    write_json(&plan_path, &edit)?;
    log(&dir, &format!("auto-cut plan (rules, no AI): {} segments, {} removals, {} captions", edit.segments.len(), edit.removals.len(), edit.captions.len()));
    stage("Rendering…");
    let report = render_plan(&dir, &plan_path, on_progress)?;
    Ok((dir, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcs_ratio() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(sequence_match(&s(&["a", "b", "c"]), &s(&["a", "x", "b", "c"])), 1.0);
        assert!((sequence_match(&s(&["a", "b", "c", "d"]), &s(&["b", "a"])) - 0.25).abs() < 1e-9);
        assert_eq!(sequence_match(&[], &s(&["a"])), 1.0);
    }
}
