pub mod api;
pub mod autoedit;
pub mod autoplan;
pub mod captions;
pub mod media;
pub mod plan;
pub mod render;
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

/// Free, offline Auto-cut (rules, no AI). Emits `autocut-stage` (text) and `autocut-progress` (0-1).
#[tauri::command]
async fn auto_cut(app: tauri::AppHandle, path: String) -> Result<AutoCutResult, String> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).ok_or("HOME is not set")?;
    let root = home.join("Movies").join("AI Reel Maker");
    std::fs::create_dir_all(&root).map_err(|e| format!("Cannot create {}: {e}", root.display()))?;
    let source = media::probe(&path)?;
    let emitter = app.clone();
    let progress = app.clone();
    let (dir, report) = tauri::async_runtime::spawn_blocking(move || {
        autoedit::auto_cut(
            &path,
            &root,
            "auto",
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
    let plan: plan::EditPlan = serde_json::from_str(
        &std::fs::read_to_string(dir.join("plan.autocut.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let count = |k: &str| plan.removals.iter().filter(|r| r.kind == k).count();
    app.asset_protocol_scope()
        .allow_file(&report.output.output_path)
        .map_err(|e| format!("Could not enable preview: {e}"))?;
    Ok(AutoCutResult {
        project_dir: dir.to_string_lossy().into_owned(),
        output_path: report.output.output_path.clone(),
        duration_sec: report.output.duration_sec,
        source_duration_sec: source.duration_sec,
        segments: report.segments,
        captions: report.captions,
        pauses_removed: count("dead_air"),
        fillers_removed: count("filler"),
        repeats_removed: count("repeat"),
        kept_words_match: report.kept_words_match,
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![probe_media, export_video, api_key_status, save_api_key, remove_api_key, auto_cut])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
