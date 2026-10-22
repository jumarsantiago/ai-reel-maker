pub mod api;
pub mod autoedit;
pub mod autoplan;
pub mod captions;
pub mod extras;
pub mod fonts;
pub mod inbox;
pub mod media;
pub mod plan;
pub mod render;
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

fn finish_auto_cut(app: &tauri::AppHandle, dir: std::path::PathBuf, report: autoedit::RenderReport, source_duration_sec: f64) -> Result<AutoCutResult, String> {
    let plan: plan::EditPlan = serde_json::from_str(
        &std::fs::read_to_string(dir.join("plan.autocut.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let count = |k: &str| plan.removals.iter().filter(|r| r.kind == k).count();
    app.asset_protocol_scope()
        .allow_file(&report.output.output_path)
        .map_err(|e| format!("Could not enable preview: {e}"))?;
    Ok(AutoCutResult {
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
    let (dir, report) = tauri::async_runtime::spawn_blocking(move || {
        autoedit::auto_cut_with(
            &path,
            &root,
            &auto_cut_language(),
            &look,
            extras.as_ref(),
            |s| {
                let _ = emitter.emit("autocut-stage", s.to_string());
            },
            |f| {
                let _ = progress.emit("autocut-progress", f);
            },
        )
    })
    .await
    .map_err(|e| format!("Auto-cut task failed: {e}"))??;
    finish_auto_cut(&app, dir, report, source.duration_sec)
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
    let (dir, report) = tauri::async_runtime::spawn_blocking(move || {
        autoedit::auto_cut_timeline(
            &segments,
            &root,
            &auto_cut_language(),
            &look,
            extras.as_ref(),
            |s| {
                let _ = emitter.emit("autocut-stage", s.to_string());
            },
            |f| {
                let _ = progress.emit("autocut-progress", f);
            },
        )
    })
    .await
    .map_err(|e| format!("Auto-cut task failed: {e}"))??;
    finish_auto_cut(&app, dir, report, source_sec)
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
) -> Result<AutoCutResult, String> {
    let look = style.unwrap_or_default();
    let root = std::fs::canonicalize(auto_cut_root()?).map_err(|e| e.to_string())?;
    let dir = std::fs::canonicalize(&project_dir).map_err(|e| format!("Project folder not found: {e}"))?;
    if !dir.starts_with(&root) || !dir.join("plan.autocut.json").is_file() {
        return Err("That is not an Auto Edit project folder.".into());
    }
    let progress = app.clone();
    let dir2 = dir.clone();
    let report = tauri::async_runtime::spawn_blocking(move || {
        autoedit::write_extras(&dir2, extras.as_ref())?;
        autoedit::apply_caption_edits(&dir2, &edits)?;
        let _ = app_emit_stage(&progress, "Rendering…");
        autoedit::render_plan(&dir2, &dir2.join("plan.autocut.json"), &look, |f| {
            let _ = progress.emit("autocut-progress", f);
        })
    })
    .await
    .map_err(|e| format!("Render task failed: {e}"))??;
    finish_auto_cut(&app, dir, report, source_duration_sec)
}

fn app_emit_stage(app: &tauri::AppHandle, text: &str) -> tauri::Result<()> {
    app.emit("autocut-stage", text.to_string())
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
    let (dir, result) = tauri::async_runtime::spawn_blocking(move || {
        travel::travel_edit(
            &segments,
            &root,
            &style,
            extras.as_ref(),
            |s| {
                let _ = emitter.emit("autocut-stage", s.to_string());
            },
            |f| {
                let _ = progress.emit("autocut-progress", f);
            },
        )
    })
    .await
    .map_err(|e| format!("Travel edit task failed: {e}"))??;
    app.asset_protocol_scope()
        .allow_file(&result.output_path)
        .map_err(|e| format!("Could not enable preview: {e}"))?;
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
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![probe_media, export_video, api_key_status, save_api_key, remove_api_key, auto_cut, auto_cut_timeline, travel_edit, list_fonts, caption_preview, auto_cut_prepare, auto_cut_render])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
