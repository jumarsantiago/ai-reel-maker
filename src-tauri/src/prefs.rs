//! The app's saved preferences: the last-used settings and the user's named presets. The UI owns the shape
//! of the data; this module only stores it safely in `~/Library/Application Support/ai-reel-maker/prefs.json`.

use std::path::{Path, PathBuf};

const MAX_BYTES: usize = 512 * 1024;

pub fn app_dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("HOME is not set")?;
    Ok(home.join("Library/Application Support/ai-reel-maker"))
}

/// Saved preferences, or `null` when nothing is saved yet (or the file is unreadable).
pub fn load(dir: &Path) -> serde_json::Value {
    std::fs::read_to_string(dir.join("prefs.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null)
}

/// Saves preferences atomically (temp file, then rename), so a crash never leaves a half-written file.
pub fn save(dir: &Path, value: &serde_json::Value) -> Result<(), String> {
    if !value.is_object() {
        return Err("Preferences must be an object.".into());
    }
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    if text.len() > MAX_BYTES {
        return Err("Preferences are too large to save.".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    let tmp = dir.join(".prefs.json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("Cannot save preferences: {e}"))?;
    std::fs::rename(&tmp, dir.join("prefs.json")).map_err(|e| format!("Cannot save preferences: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_bad_input() {
        let dir = std::env::temp_dir().join(format!("arm-prefs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load(&dir).is_null());
        let v = serde_json::json!({"last": {"orientation": "vertical"}, "presets": [{"name": "Affiliate"}]});
        save(&dir, &v).unwrap();
        assert_eq!(load(&dir), v);
        assert!(save(&dir, &serde_json::json!([1, 2])).is_err());
        std::fs::write(dir.join("prefs.json"), "{ not json").unwrap();
        assert!(load(&dir).is_null(), "a damaged file falls back to defaults");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
