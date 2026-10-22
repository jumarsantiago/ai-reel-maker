pub mod api;
pub mod autoedit;
pub mod autoplan;
pub mod beats;
pub mod captions;
pub mod claude;
pub mod claude_code;
pub mod dictionary;
pub mod engine;
pub mod extras;
pub mod fonts;
pub mod frames;
pub mod inbox;
pub mod kit_review;
pub mod media;
pub mod motion;
pub mod plan;
pub mod prefs;
pub mod render;
pub mod thumbnail;
pub mod travel;
pub mod transcribe;

use media::MediaInfo;
use tauri::{Emitter, Manager};

/// Probes a user-picked video and allows the webview to stream that one file for preview.
#[tauri::command]
fn probe_media(app: tauri::AppHandle, path: String) -> Result<MediaInfo, String> {
    let info = media::probe(&path)?;
    app.asset_protocol_scope()
        .allow_file(&info.path)
        .map_err(|e| format!("Could not enable preview for this file: {e}"))?;
    Ok(info)
}

/// Renders the timeline to MP4 off the main thread, emitting `export-progress` (0.0–1.0).
#[tauri::command]
async fn export_video(app: tauri::AppHandle, request: render::ExportRequest) -> Result<render::ExportResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        render::export(&request, |fraction| {
            let _ = app.emit("export-progress", fraction);
        })
    })
    .await
    .map_err(|e| format!("Export task failed: {e}"))?
}

/// Settings: whether an API key is configured. Never returns the key.
#[tauri::command]
fn api_key_status() -> api::KeyStatus {
    api::key_status()
}

/// Settings: saves the pasted key to the macOS login keychain.
#[tauri::command]
async fn save_api_key(key: String) -> Result<api::KeyStatus, String> {
    tauri::async_runtime::spawn_blocking(move || api::save_key(&key)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn remove_api_key() -> Result<api::KeyStatus, String> {
    tauri::async_runtime::spawn_blocking(api::remove_key).await.map_err(|e| e.to_string())?
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AutoCutResult {
    /// "talk" (Auto Edit of speech) or "travel" (b-roll reel).
    mode: String,
    project_dir: String,
    output_path: String,
    duration_sec: f64,
    source_duration_sec: f64,
    segments: usize,
    captions: usize,
    pauses_removed: usize,
    fillers_removed: usize,
    repeats_removed: usize,
    kept_words_match: f64,
    /// Captions as a subtitle file (for CapCut and YouTube).
    srt_path: Option<String>,
    /// The same edit in the other shape, when both were asked for.
    other_output_path: Option<String>,
}

fn auto_cut_root() -> Result<std::path::PathBuf, String> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).ok_or("HOME is not set")?;
    let root = home.join("Movies").join("AI Reel Maker");
    std::fs::create_dir_all(&root).map_err(|e| format!("Cannot create {}: {e}", root.display()))?;
    Ok(root)
}

// Taglish reels: "auto" detects the language per chunk and can hallucinate other languages.
// Set AIRM_LANGUAGE=auto (or en) to override.
fn auto_cut_language() -> String {
    std::env::var("AIRM_LANGUAGE").unwrap_or_else(|_| "tl".into())
}

fn finish_auto_cut(
    app: &tauri::AppHandle,
    dir: std::path::PathBuf,
    report: autoedit::RenderReport,
    source_duration_sec: f64,
    other: Option<render::ExportResult>,
) -> Result<AutoCutResult, String> {
    let plan: plan::EditPlan = serde_json::from_str(
        &std::fs::read_to_string(dir.join("plan.autocut.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let count = |k: &str| plan.removals.iter().filter(|r| r.kind == k).count();
    app.asset_protocol_scope()
        .allow_file(&report.output.output_path)
        .map_err(|e| format!("Could not enable preview: {e}"))?;
    if let Some(o) = &other {
        app.asset_protocol_scope().allow_file(&o.output_path).map_err(|e| format!("Could not enable preview: {e}"))?;
    }
    Ok(AutoCutResult {
        srt_path: Some(report.srt.clone()),
        other_output_path: other.map(|o| o.output_path),
        mode: "talk".into(),
        project_dir: dir.to_string_lossy().into_owned(),
        output_path: report.output.output_path.clone(),
        duration_sec: report.output.duration_sec,
        source_duration_sec,
        segments: report.segments,
        captions: report.captions,
        pauses_removed: count("dead_air"),
        fillers_removed: count("filler"),
        repeats_removed: count("repeat"),
        kept_words_match: report.kept_words_match,
    })
}

/// Free, offline Auto-cut (rules, no AI). Emits `autocut-stage` (text) and `autocut-progress` (0-1).
#[tauri::command]
async fn auto_cut(
    app: tauri::AppHandle,
    path: String,
    style: Option<autoedit::EditStyle>,
    extras: Option<extras::Extras>,
) -> Result<AutoCutResult, String> {
    let look = style.unwrap_or_default();
    let root = auto_cut_root()?;
    let source = media::probe(&path)?;
    let emitter = app.clone();
    let progress = app.clone();
    let (dir, report, other) = tauri::async_runtime::spawn_blocking(move || {
        let stage = |s: &str| {
            let _ = emitter.emit("autocut-stage", s.to_string());
        };
        let prog = |f: f64| {
            let _ = progress.emit("autocut-progress", f);
        };
        let (dir, report) = autoedit::auto_cut_with(&path, &root, &auto_cut_language(), &look, extras.as_ref(), stage, prog)?;
        let other = autoedit::other_shape_if_wanted(&dir, &look, stage, prog)?;
        Ok::<_, String>((dir, report, other))
    })
    .await
    .map_err(|e| format!("Auto-cut task failed: {e}"))??;
    finish_auto_cut(&app, dir, report, source.duration_sec, other)
}

/// Auto Edit for everything on the timeline: the clips (in order, with their trims) become one reel.
#[tauri::command]
async fn auto_cut_timeline(
    app: tauri::AppHandle,
    segments: Vec<render::SegmentRequest>,
    style: Option<autoedit::EditStyle>,
    extras: Option<extras::Extras>,
) -> Result<AutoCutResult, String> {
    let look = style.unwrap_or_default();
    let root = auto_cut_root()?;
    let source_sec: f64 = segments.iter().map(|s| (s.out_point - s.in_point).max(0.0)).sum();
    let emitter = app.clone();
    let progress = app.clone();
    let (dir, report, other) = tauri::async_runtime::spawn_blocking(move || {
        let stage = |s: &str| {
            let _ = emitter.emit("autocut-stage", s.to_string());
        };
        let prog = |f: f64| {
            let _ = progress.emit("autocut-progress", f);
        };
        let (dir, report) = autoedit::auto_cut_timeline(&segments, &root, &auto_cut_language(), &look, extras.as_ref(), stage, prog)?;
        let other = autoedit::other_shape_if_wanted(&dir, &look, stage, prog)?;
        Ok::<_, String>((dir, report, other))
    })
    .await
    .map_err(|e| format!("Auto-cut task failed: {e}"))??;
    finish_auto_cut(&app, dir, report, source_sec, other)
}

/// What Auto Edit works on: one video, or every clip on the timeline.
#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum AutoSource {
    Video { path: String },
    Timeline { segments: Vec<render::SegmentRequest> },
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PrepareResult {
    project_dir: String,
    source_duration_sec: f64,
    captions: Vec<autoedit::CaptionDraft>,
}

/// First half of Auto Edit with caption review: transcribe and plan, then hand back the captions to check.
#[tauri::command]
async fn auto_cut_prepare(
    app: tauri::AppHandle,
    source: AutoSource,
    style: Option<autoedit::EditStyle>,
    extras: Option<extras::Extras>,
) -> Result<PrepareResult, String> {
    let look = style.unwrap_or_default();
    let root = auto_cut_root()?;
    let emitter = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let stage = |s: &str| {
            let _ = emitter.emit("autocut-stage", s.to_string());
        };
        let (video, source_duration_sec) = match &source {
            AutoSource::Video { path } => (path.clone(), media::probe(path)?.duration_sec),
            AutoSource::Timeline { segments } => {
                let sec: f64 = segments.iter().map(|s| (s.out_point - s.in_point).max(0.0)).sum();
                (autoedit::join_timeline(segments, &root, &look, stage)?.to_string_lossy().into_owned(), sec)
            }
        };
        let dir = autoedit::auto_cut_prepare(&video, &root, &auto_cut_language(), &look, extras.as_ref(), stage)?;
        let captions = autoedit::caption_drafts(&dir)?;
        Ok(PrepareResult { project_dir: dir.to_string_lossy().into_owned(), source_duration_sec, captions })
    })
    .await
    .map_err(|e| format!("Auto-cut task failed: {e}"))?
}

/// Second half: apply the user's caption edits and render. Only folders made by this app are accepted.
#[tauri::command]
async fn auto_cut_render(
    app: tauri::AppHandle,
    project_dir: String,
    source_duration_sec: f64,
    style: Option<autoedit::EditStyle>,
    extras: Option<extras::Extras>,
    edits: Vec<autoedit::CaptionEdit>,
    remember_fixes: Option<bool>,
) -> Result<AutoCutResult, String> {
    let look = style.unwrap_or_default();
    let dir = checked_project_dir(&project_dir)?;
    if remember_fixes.unwrap_or(true) {
        // Spelling fixes made in the review are remembered for the next videos.
        let learned = autoedit::learned_fixes(&dir, &edits)?;
        if !learned.is_empty() {
            let app_dir = prefs::app_dir()?;
            let mut d = dictionary::load(&app_dir);
            if dictionary::merge(&mut d, learned) > 0 {
                dictionary::save(&app_dir, &d)?;
            }
        }
    }
    let progress = app.clone();
    let dir2 = dir.clone();
    let (report, other) = tauri::async_runtime::spawn_blocking(move || {
        autoedit::write_extras(&dir2, extras.as_ref())?;
        autoedit::apply_caption_edits(&dir2, &edits)?;
        let _ = app_emit_stage(&progress, "Rendering…");
        let prog = |f: f64| {
            let _ = progress.emit("autocut-progress", f);
        };
        let report = autoedit::render_plan(&dir2, &dir2.join("plan.autocut.json"), &look, prog)?;
        let other = autoedit::other_shape_if_wanted(&dir2, &look, |s| { let _ = app_emit_stage(&progress, s); }, prog)?;
        Ok::<_, String>((report, other))
    })
    .await
    .map_err(|e| format!("Render task failed: {e}"))??;
    finish_auto_cut(&app, dir, report, source_duration_sec, other)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewResult {
    output_path: String,
    duration_sec: f64,
}

const PREVIEW_SEC: f64 = 10.0;

fn checked_project_dir(project_dir: &str) -> Result<std::path::PathBuf, String> {
    let root = std::fs::canonicalize(auto_cut_root()?).map_err(|e| e.to_string())?;
    let dir = std::fs::canonicalize(project_dir).map_err(|e| format!("Project folder not found: {e}"))?;
    if !dir.starts_with(&root) || !dir.join("plan.autocut.json").is_file() {
        return Err("That is not an Auto Edit project folder.".into());
    }
    Ok(dir)
}

/// The first 10 seconds of a reviewed Auto Edit project, with the caption edits applied.
#[tauri::command]
async fn auto_cut_preview(
    app: tauri::AppHandle,
    project_dir: String,
    style: Option<autoedit::EditStyle>,
    extras: Option<extras::Extras>,
    edits: Vec<autoedit::CaptionEdit>,
) -> Result<PreviewResult, String> {
    let look = style.unwrap_or_default();
    let dir = checked_project_dir(&project_dir)?;
    let progress = app.clone();
    let r = tauri::async_runtime::spawn_blocking(move || {
        autoedit::write_extras(&dir, extras.as_ref())?;
        autoedit::apply_caption_edits(&dir, &edits)?;
        autoedit::render_preview(&dir, &dir.join("plan.autocut.json"), &look, PREVIEW_SEC, |f| {
            let _ = progress.emit("preview-progress", f);
        })
    })
    .await
    .map_err(|e| format!("Preview task failed: {e}"))??;
    app.asset_protocol_scope().allow_file(&r.output_path).map_err(|e| format!("Could not enable preview: {e}"))?;
    Ok(PreviewResult { output_path: r.output_path, duration_sec: r.duration_sec })
}

/// The first 10 seconds of a travel reel.
#[tauri::command]
async fn travel_preview(
    app: tauri::AppHandle,
    segments: Vec<render::SegmentRequest>,
    style: Option<travel::TravelStyle>,
    extras: Option<extras::Extras>,
) -> Result<PreviewResult, String> {
    let style = style.unwrap_or_default();
    let root = auto_cut_root()?;
    let progress = app.clone();
    let r = tauri::async_runtime::spawn_blocking(move || {
        travel::travel_preview(&segments, &root, &style, extras.as_ref(), PREVIEW_SEC, |f| {
            let _ = progress.emit("preview-progress", f);
        })
    })
    .await
    .map_err(|e| format!("Preview task failed: {e}"))??;
    app.asset_protocol_scope().allow_file(&r.output_path).map_err(|e| format!("Could not enable preview: {e}"))?;
    Ok(PreviewResult { output_path: r.output_path, duration_sec: r.duration_sec })
}

fn app_emit_stage(app: &tauri::AppHandle, text: &str) -> tauri::Result<()> {
    app.emit("autocut-stage", text.to_string())
}

/// Saved settings and presets (the UI decides their shape).
#[tauri::command]
fn get_prefs() -> Result<serde_json::Value, String> {
    Ok(prefs::load(&prefs::app_dir()?))
}

#[tauri::command]
fn set_prefs(value: serde_json::Value) -> Result<(), String> {
    prefs::save(&prefs::app_dir()?, &value)
}

/// The remembered spelling fixes.
#[tauri::command]
fn get_dictionary() -> Result<Vec<dictionary::Fix>, String> {
    Ok(dictionary::load(&prefs::app_dir()?))
}

#[tauri::command]
fn set_dictionary(fixes: Vec<dictionary::Fix>) -> Result<(), String> {
    dictionary::save(&prefs::app_dir()?, &fixes)
}

/// Cover image for a finished reel (sharpest well-lit frame + hook text). Only videos this app made.
#[tauri::command]
async fn make_thumbnail(
    app: tauri::AppHandle,
    video_path: String,
    text: Option<String>,
    font: Option<String>,
    style: Option<autoedit::EditStyle>,
) -> Result<String, String> {
    let root = std::fs::canonicalize(auto_cut_root()?).map_err(|e| e.to_string())?;
    let video = std::fs::canonicalize(&video_path).map_err(|e| format!("Video not found: {e}"))?;
    if !video.starts_with(&root) {
        return Err("Thumbnails are made only for videos made by this app.".into());
    }
    let out = tauri::async_runtime::spawn_blocking(move || {
        // Auto Edit projects get a frame without captions; travel reels use the finished video directly.
        let dir = video.parent().map(|d| d.to_path_buf()).unwrap_or_default();
        match style {
            Some(look) if dir.join("plan.autocut.json").is_file() => autoedit::project_thumbnail(&dir, &video, &look, text.as_deref()),
            _ => thumbnail::make_thumbnail(&video, text.as_deref(), font.as_deref()),
        }
    })
        .await
        .map_err(|e| e.to_string())??;
    app.asset_protocol_scope().allow_file(&out).map_err(|e| format!("Could not show thumbnail: {e}"))?;
    Ok(out.to_string_lossy().into_owned())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct KitStatus {
    found: bool,
    dir: Option<String>,
    message: String,
}

/// Whether HyperFrames (the free247editor kit) can be found, for the Motion tab and Settings.
#[tauri::command]
fn kit_status(dir: Option<String>) -> KitStatus {
    match motion::Kit::find(dir.as_deref()) {
        Ok(k) => KitStatus { found: true, dir: Some(k.dir.to_string_lossy().into_owned()), message: "HyperFrames is ready.".into() },
        Err(e) => KitStatus { found: false, dir: None, message: e },
    }
}

/// One motion graphic playing over the user's video (or a plain backdrop), as a short MP4 the UI can play.
#[tauri::command]
async fn motion_preview(
    app: tauri::AppHandle,
    spec: motion::MotionSpec,
    orientation: render::Orientation,
    kit_dir: Option<String>,
    background: Option<String>,
    use_kit: Option<bool>,
) -> Result<String, String> {
    let out = tauri::async_runtime::spawn_blocking(move || {
        motion::ensure_kit_enabled(use_kit)?;
        let kit = motion::Kit::find(kit_dir.as_deref())?;
        let bg = background.as_deref().map(std::path::Path::new);
        motion::preview(&kit, &spec, orientation, bg, &motion::cache_dir()?)
    })
    .await
    .map_err(|e| format!("Preview task failed: {e}"))??;
    app.asset_protocol_scope().allow_file(&out).map_err(|e| format!("Could not enable preview: {e}"))?;
    Ok(out.to_string_lossy().into_owned())
}

/// Applies the "Use Claude AI" switch to the whole process. (The kit switch travels with each request.)
#[tauri::command]
fn set_engine(use_claude: bool) {
    engine::set_claude_enabled(use_claude);
}

/// A real check of both engines: Node and HyperFrames are run, the saved key's source is looked up (never read out).
#[tauri::command]
async fn engine_status(request: engine::EngineRequest) -> Result<engine::EngineStatus, String> {
    let ledger = claude::ledger_path()?;
    tauri::async_runtime::spawn_blocking(move || {
        engine::status(request.use_kit, request.use_claude, request.kit_dir.as_deref(), &ledger, claude::MAX_PAID_CALLS, claude::MAX_USD)
    })
    .await
    .map_err(|e| e.to_string())
}

/// The exact price of asking Claude to plan this project (counting tokens is free).
#[tauri::command]
async fn claude_estimate(project_dir: String) -> Result<claude::Estimate, String> {
    let dir = checked_project_dir(&project_dir)?;
    let ledger = claude::ledger_path()?;
    tauri::async_runtime::spawn_blocking(move || claude::estimate(&dir, &ledger)).await.map_err(|e| e.to_string())?
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ClaudePlanResult {
    outcome: claude::PlanOutcome,
    /// The captions of the plan now in use (only when Claude's plan was applied).
    captions: Option<Vec<autoedit::CaptionDraft>>,
}

/// The one paid call, after the user confirmed the worst-case cost.
#[tauri::command]
async fn claude_plan(project_dir: String, confirmed_usd: f64) -> Result<ClaudePlanResult, String> {
    let dir = checked_project_dir(&project_dir)?;
    let ledger = claude::ledger_path()?;
    tauri::async_runtime::spawn_blocking(move || {
        let outcome = claude::run_plan(&dir, &ledger, confirmed_usd)?;
        let captions = if outcome.applied { Some(autoedit::caption_drafts(&dir)?) } else { None };
        Ok(ClaudePlanResult { outcome, captions })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Is Claude Code (the user's `claude` command) installed and logged in?
#[tauri::command]
async fn claude_code_status() -> Result<claude_code::CliStatus, String> {
    engine::require_claude()?;
    tauri::async_runtime::spawn_blocking(|| claude_code::status(None)).await.map_err(|e| e.to_string())
}

/// Asks Claude Code (the user's subscription, not the API) to plan this edit. It can take a few minutes.
#[tauri::command]
async fn claude_code_plan(project_dir: String) -> Result<ClaudePlanResult, String> {
    engine::require_claude()?;
    let dir = checked_project_dir(&project_dir)?;
    tauri::async_runtime::spawn_blocking(move || {
        let cli = claude_code::find_cli().ok_or(claude_code::INSTALL_HELP)?;
        let outcome = claude_code::run_plan(&dir, &cli, claude_code::DEFAULT_TIMEOUT)?;
        let captions = if outcome.applied { Some(autoedit::caption_drafts(&dir)?) } else { None };
        Ok(ClaudePlanResult { outcome, captions })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Stops a running Claude Code request.
#[tauri::command]
fn claude_code_cancel() -> bool {
    claude_code::cancel()
}

/// Puts the rules-based plan back after a Claude plan. Returns the captions of the restored plan.
#[tauri::command]
async fn claude_undo(project_dir: String) -> Result<Vec<autoedit::CaptionDraft>, String> {
    let dir = checked_project_dir(&project_dir)?;
    tauri::async_runtime::spawn_blocking(move || {
        claude::undo(&dir)?;
        autoedit::caption_drafts(&dir)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// herk-edit-kit's edit-review page for a finished Auto Edit, opened in the browser.
#[tauri::command]
async fn kit_edl_review(project_dir: String, video_path: String, kit_dir: Option<String>, use_kit: bool) -> Result<String, String> {
    engine::require_kit(Some(use_kit))?;
    let dir = checked_project_dir(&project_dir)?;
    let video = std::fs::canonicalize(&video_path).map_err(|e| format!("Video not found: {e}"))?;
    if !video.starts_with(&dir) {
        return Err("That video does not belong to this project.".into());
    }
    let html = tauri::async_runtime::spawn_blocking(move || {
        let kit = motion::Kit::find(kit_dir.as_deref())?;
        kit_review::build(&kit, &dir, &video)
    })
    .await
    .map_err(|e| e.to_string())??;
    let _ = std::process::Command::new("/usr/bin/open").arg(&html).status();
    Ok(html.to_string_lossy().into_owned())
}

/// Caption font choices for the picker (system and user fonts that can draw Tagalog and English).
#[tauri::command]
async fn list_fonts() -> Result<Vec<fonts::FontInfo>, String> {
    tauri::async_runtime::spawn_blocking(fonts::list_fonts).await.map_err(|e| e.to_string())
}

/// A real caption drawn in the chosen font, as a PNG data URL.
#[tauri::command]
async fn caption_preview(font: Option<String>, text: Option<String>, template: Option<captions::CaptionTemplate>) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || fonts::preview_data_url(font.as_deref(), text.as_deref(), template.unwrap_or_default()))
        .await
        .map_err(|e| e.to_string())?
}

/// Travel mode: the timeline's b-roll clips become one vertical reel (nothing is cut for silence).
#[tauri::command]
async fn travel_edit(
    app: tauri::AppHandle,
    segments: Vec<render::SegmentRequest>,
    style: Option<travel::TravelStyle>,
    extras: Option<extras::Extras>,
) -> Result<AutoCutResult, String> {
    let style = style.unwrap_or_default();
    let root = auto_cut_root()?;
    let clips = segments.len();
    let source_sec: f64 = segments.iter().map(|s| (s.out_point - s.in_point).max(0.0)).sum();
    let emitter = app.clone();
    let progress = app.clone();
    let (dir, result, other) = tauri::async_runtime::spawn_blocking(move || {
        let stage = |s: &str| {
            let _ = emitter.emit("autocut-stage", s.to_string());
        };
        let prog = |f: f64| {
            let _ = progress.emit("autocut-progress", f);
        };
        let (dir, result) = travel::travel_edit(&segments, &root, &style, extras.as_ref(), stage, prog)?;
        let other = if style.both_shapes {
            stage("Rendering the other shape…");
            let mut flipped = style.clone();
            flipped.orientation = match style.orientation {
                render::Orientation::Vertical => render::Orientation::Horizontal,
                render::Orientation::Horizontal => render::Orientation::Vertical,
            };
            Some(travel::travel_edit(&segments, &root, &flipped, extras.as_ref(), |_| {}, prog)?.1)
        } else {
            None
        };
        Ok::<_, String>((dir, result, other))
    })
    .await
    .map_err(|e| format!("Travel edit task failed: {e}"))??;
    app.asset_protocol_scope()
        .allow_file(&result.output_path)
        .map_err(|e| format!("Could not enable preview: {e}"))?;
    if let Some(o) = &other {
        app.asset_protocol_scope().allow_file(&o.output_path).map_err(|e| format!("Could not enable preview: {e}"))?;
    }
    Ok(AutoCutResult {
        mode: "travel".into(),
        project_dir: dir.to_string_lossy().into_owned(),
        output_path: result.output_path,
        duration_sec: result.duration_sec,
        source_duration_sec: source_sec,
        segments: clips,
        captions: 0,
        pauses_removed: 0,
        fillers_removed: 0,
        repeats_removed: 0,
        kept_words_match: 1.0,
        srt_path: None,
        other_output_path: other.map(|o| o.output_path),
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|_| {
            // The saved "Use Claude AI" switch applies from the first moment (OFF unless it was turned on).
            engine::apply_saved();
            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![probe_media, export_video, api_key_status, save_api_key, remove_api_key, auto_cut, auto_cut_timeline, travel_edit, list_fonts, caption_preview, auto_cut_prepare, auto_cut_render, auto_cut_preview, travel_preview, get_prefs, set_prefs, get_dictionary, set_dictionary, make_thumbnail, kit_status, motion_preview, set_engine, engine_status, claude_estimate, claude_plan, claude_undo, kit_edl_review, claude_code_status, claude_code_plan, claude_code_cancel])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
