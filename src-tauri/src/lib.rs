mod media;
mod render;

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![probe_media, export_video])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
