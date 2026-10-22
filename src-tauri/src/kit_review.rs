//! The edit-review page from herk-edit-kit (`scripts/build-edl-review.mjs`), fed with an Auto Edit project's
//! real cut list: kept speech (green) and every cut (red) on a timeline, with the original and the edited video
//! side by side. It reads the project's own plan, so it shows exactly what was removed and why.

use crate::motion::Kit;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The kit's EDL (`source_duration`, `edited_duration`, `removed`, `keep_ranges`, `delete_ranges`) from a project's
/// validated plan (kept segments, in source time) and the removals the planner recorded (for the reasons).
pub fn edl_from_plan(source_duration: f64, validated: &Value, removals: &Value) -> Result<Value, String> {
    let segs = validated["segments"].as_array().ok_or("The project has no validated plan yet.")?;
    let mut keeps: Vec<(f64, f64)> = segs
        .iter()
        .filter_map(|s| Some((s["start"].as_f64()?, s["end"].as_f64()?)))
        .filter(|(a, b)| b > a)
        .collect();
    keeps.sort_by(|a, b| a.0.total_cmp(&b.0));
    if keeps.is_empty() {
        return Err("The plan keeps nothing, so there is nothing to review.".into());
    }
    let reasons_for = |a: f64, b: f64| -> Vec<String> {
        let found: Vec<String> = removals
            .as_array()
            .map(|r| {
                r.iter()
                    .filter(|x| x["start"].as_f64().zip(x["end"].as_f64()).is_some_and(|(s, e)| s < b && e > a))
                    .map(|x| {
                        let kind = x["kind"].as_str().unwrap_or("cut");
                        match x["rationale"].as_str().filter(|r| !r.is_empty()) {
                            Some(r) => format!("{kind}: {r}"),
                            None => kind.to_string(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        if found.is_empty() { vec!["pause".to_string()] } else { found }
    };
    let mut deletes = Vec::new();
    let mut cursor = 0.0;
    for (a, b) in &keeps {
        if *a - cursor > 0.01 {
            deletes.push(json!({"start": cursor, "end": a, "reasons": reasons_for(cursor, *a)}));
        }
        cursor = cursor.max(*b);
    }
    if source_duration - cursor > 0.01 {
        deletes.push(json!({"start": cursor, "end": source_duration, "reasons": reasons_for(cursor, source_duration)}));
    }
    let kept: f64 = keeps.iter().map(|(a, b)| b - a).sum();
    Ok(json!({
        "source_duration": source_duration,
        "edited_duration": kept,
        "removed": (source_duration - kept).max(0.0),
        "keep_ranges": keeps.iter().map(|(a, b)| json!({"start": a, "end": b})).collect::<Vec<_>>(),
        "delete_ranges": deletes,
        "params": {"gap": crate::autoplan::MAX_GAP_SEC},
    }))
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("Invalid JSON in {}: {e}", path.display()))
}

/// Writes `edit-review.html` in the project folder using the kit's script, and returns its path.
/// `edited` is the rendered video to show beside the original.
pub fn build(kit: &Kit, project: &Path, edited: &Path) -> Result<PathBuf, String> {
    let validated = read_json(&project.join("plan.validated.json"))?;
    let removals = read_json(&project.join("plan.autocut.json")).map(|p| p["removals"].clone()).unwrap_or(Value::Null);
    let source = read_json(&project.join("source.json"))?;
    let original = source["path"].as_str().ok_or("source.json has no path")?;
    let dur = source["durationSec"].as_f64().or_else(|| source["duration_sec"].as_f64()).ok_or("source.json has no duration")?;
    let edl = edl_from_plan(dur, &validated, &removals)?;
    let edl_path = project.join("edit-edl.json");
    std::fs::write(&edl_path, serde_json::to_string_pretty(&edl).map_err(|e| e.to_string())? + "\n").map_err(|e| e.to_string())?;
    let script = kit.dir.join("scripts/build-edl-review.mjs");
    if !script.is_file() {
        return Err(format!("{} is missing from the kit. Update the kit folder.", script.display()));
    }
    let out = project.join("edit-review.html");
    let output = Command::new(&kit.node)
        .arg(&script)
        .arg(&edl_path)
        .arg("--original")
        .arg(original)
        .arg("--edited")
        .arg(edited)
        .arg("--output")
        .arg(&out)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Could not run the kit's review builder: {e}"))?;
    if !output.status.success() || !out.is_file() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("The kit's review builder failed:\n{}", err.lines().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validated() -> Value {
        json!({"segments": [{"start": 0.0, "end": 4.0}, {"start": 6.0, "end": 9.0}, {"start": 9.5, "end": 12.0}], "total_sec": 9.5})
    }

    #[test]
    fn edl_has_keeps_deletes_reasons_and_totals() {
        let removals = json!([
            {"start": 4.0, "end": 6.0, "kind": "filler", "rationale": "Filler: \"um\""},
            {"start": 9.0, "end": 9.5, "kind": "dead_air"},
        ]);
        let edl = edl_from_plan(14.0, &validated(), &removals).unwrap();
        assert_eq!(edl["keep_ranges"].as_array().unwrap().len(), 3);
        let del = edl["delete_ranges"].as_array().unwrap();
        assert_eq!(del.len(), 3, "two gaps plus the tail: {del:?}");
        assert_eq!((del[0]["start"].as_f64(), del[0]["end"].as_f64()), (Some(4.0), Some(6.0)));
        assert_eq!(del[0]["reasons"][0], "filler: Filler: \"um\"");
        assert_eq!(del[1]["reasons"][0], "dead_air");
        assert_eq!(del[2]["reasons"][0], "pause", "no matching removal means a plain pause");
        assert!((edl["edited_duration"].as_f64().unwrap() - 9.5).abs() < 1e-9);
        assert!((edl["removed"].as_f64().unwrap() - 4.5).abs() < 1e-9);
        // Nothing removed: no delete ranges.
        let all = json!({"segments": [{"start": 0.0, "end": 10.0}]});
        assert!(edl_from_plan(10.0, &all, &Value::Null).unwrap()["delete_ranges"].as_array().unwrap().is_empty());
        // Bad plans give a message, not a panic.
        assert!(edl_from_plan(10.0, &json!({}), &Value::Null).is_err());
        assert!(edl_from_plan(10.0, &json!({"segments": []}), &Value::Null).unwrap_err().contains("nothing"));
    }

    /// Runs the kit's real script on a real project folder and checks the page it writes.
    #[test]
    fn the_kits_review_builder_runs_on_a_real_project() {
        let Ok(kit) = Kit::find(None) else {
            eprintln!("skipped: no herk-edit-kit on this computer");
            return;
        };
        let Some(ffmpeg) = crate::media::find_tool("ffmpeg") else { return };
        let dir = std::env::temp_dir().join(format!("arm-kitreview-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mk = |name: &str, secs: u32| {
            let p = dir.join(name);
            assert!(Command::new(&ffmpeg)
                .args(["-y", "-v", "error", "-f", "lavfi", "-i", &format!("testsrc2=size=320x180:rate=25:duration={secs}"), "-c:v", "libx264", "-pix_fmt", "yuv420p"])
                .arg(&p)
                .status()
                .unwrap()
                .success());
            p
        };
        let orig = mk("orig.mp4", 14);
        let edited = mk("edited.mp4", 10);
        std::fs::write(dir.join("plan.validated.json"), validated().to_string()).unwrap();
        std::fs::write(dir.join("plan.autocut.json"), json!({"removals": [{"start": 4.0, "end": 6.0, "kind": "filler", "rationale": "um"}]}).to_string()).unwrap();
        std::fs::write(dir.join("source.json"), json!({"path": orig, "durationSec": 14.0}).to_string()).unwrap();
        let html = build(&kit, &dir, &edited).unwrap();
        let page = std::fs::read_to_string(&html).unwrap();
        assert!(page.contains("class=\"seg cut\"") && page.contains("cut #3"), "the page draws the three cuts");
        assert!(page.contains("filler: um"), "reasons from the app's plan reach the page");
        assert!(page.contains("edited.mp4") && page.contains("orig.mp4"), "both videos are linked");
        // A broken project is reported, not turned into an empty page.
        std::fs::remove_file(dir.join("plan.validated.json")).unwrap();
        assert!(build(&kit, &dir, &edited).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
