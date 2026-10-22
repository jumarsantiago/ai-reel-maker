//! Folder workflow: raw videos go in `inbox/`, finished drafts appear in `outbox/`.
//! Originals are never moved, renamed or deleted; drafts are copies. Status is derived from the
//! files in `outbox/` (no hidden database): `<stem>-draft-N.mp4` and `<stem>-FINAL.mp4`.

use crate::media::ALLOWED_EXTENSIONS;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const RULES_TEMPLATE: &str = "# My editing rules\n\
\n\
Claude reads this file before every edit. Add rules in plain language, one per line, e.g.\n\
\"Always keep the yellow caption highlight.\" Anything under \"Rules\" applies to every video.\n\
\n\
## Rules\n\
\n\
- (none yet)\n\
\n\
## Notes from past edits\n\
\n";

/// `~/Movies/AI Reel Maker` unless overridden with AIRM_HOME.
pub fn base_dir() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("AIRM_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("HOME is not set")?;
    Ok(home.join("Movies").join("AI Reel Maker"))
}

/// Creates inbox/, outbox/, my-sounds/ and my-rules.md if missing. Never overwrites anything.
pub fn init(base: &Path) -> Result<Vec<String>, String> {
    let mut created = Vec::new();
    for d in ["inbox", "outbox", "my-sounds"] {
        let p = base.join(d);
        if !p.exists() {
            std::fs::create_dir_all(&p).map_err(|e| format!("Cannot create {}: {e}", p.display()))?;
            created.push(format!("{d}/"));
        }
    }
    let rules = base.join("my-rules.md");
    if !rules.exists() {
        std::fs::write(&rules, RULES_TEMPLATE).map_err(|e| format!("Cannot write {}: {e}", rules.display()))?;
        created.push("my-rules.md".into());
    }
    Ok(created)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct InboxItem {
    pub file: String,
    pub path: String,
    pub stem: String,
    pub drafts: usize,
    pub is_final: bool,
}

impl InboxItem {
    pub fn status(&self) -> String {
        if self.is_final {
            "final".into()
        } else if self.drafts > 0 {
            format!("{} draft(s), awaiting review", self.drafts)
        } else {
            "new".into()
        }
    }
}

fn is_video(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| ALLOWED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

pub fn list_inbox(inbox: &Path, outbox: &Path) -> Result<Vec<InboxItem>, String> {
    let mut items = Vec::new();
    let entries = std::fs::read_dir(inbox).map_err(|e| format!("Cannot read {}: {e}", inbox.display()))?;
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !p.is_file() || !is_video(&p) {
            continue;
        }
        let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let prefix = format!("{stem}-draft-");
        let drafts = std::fs::read_dir(outbox)
            .map(|rd| {
                rd.flatten()
                    .filter(|f| {
                        let n = f.file_name().to_string_lossy().into_owned();
                        n.starts_with(&prefix) && n.ends_with(".mp4")
                    })
                    .count()
            })
            .unwrap_or(0);
        let is_final = outbox.join(format!("{stem}-FINAL.mp4")).exists();
        items.push(InboxItem { file: name, path: p.to_string_lossy().into_owned(), stem, drafts, is_final });
    }
    items.sort_by(|a, b| a.file.cmp(&b.file));
    Ok(items)
}

fn read_value(p: &Path) -> Result<Value, String> {
    let t = std::fs::read_to_string(p).map_err(|e| format!("Cannot read {}: {e}", p.display()))?;
    serde_json::from_str(&t).map_err(|e| format!("Invalid JSON in {}: {e}", p.display()))
}

/// Safe file-name stem: letters, digits, space, dash, underscore, dot only.
fn clean_stem(s: &str) -> String {
    let c: String = s.chars().map(|c| if c.is_alphanumeric() || " -_.".contains(c) { c } else { '_' }).collect();
    c.trim().trim_matches('.').to_string()
}

fn copy_new(from: &Path, to: &Path) -> Result<(), String> {
    if to.exists() {
        return Err(format!("{} already exists.", to.display()));
    }
    let tmp = to.with_extension("partial");
    std::fs::copy(from, &tmp).map_err(|e| format!("Cannot copy to {}: {e}", to.display()))?;
    std::fs::rename(&tmp, to).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Cannot finish {}: {e}", to.display())
    })
}

/// Copies a project's latest render (and its .srt) into outbox/ as a new draft, or as FINAL.
/// Returns the new file's path. Never overwrites an existing draft or final.
pub fn publish(project: &Path, outbox: &Path, final_: bool) -> Result<PathBuf, String> {
    let source = read_value(&project.join("source.json"))?;
    let report = read_value(&project.join("render_report.json"))?;
    let stem = clean_stem(
        &Path::new(source["fileName"].as_str().ok_or("source.json has no fileName")?)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    if stem.is_empty() {
        return Err("Could not work out a name for the draft.".into());
    }
    let video = PathBuf::from(report["output"]["output_path"].as_str().ok_or("render_report.json has no output path")?);
    if !video.is_file() {
        return Err(format!("Rendered video not found: {}", video.display()));
    }
    let srt = report["srt"].as_str().map(PathBuf::from).filter(|p| p.is_file());
    std::fs::create_dir_all(outbox).map_err(|e| format!("Cannot create {}: {e}", outbox.display()))?;

    let name = if final_ {
        let first = format!("{stem}-FINAL");
        if outbox.join(format!("{first}.mp4")).exists() {
            (2..).map(|n| format!("{stem}-FINAL-{n}")).find(|n| !outbox.join(format!("{n}.mp4")).exists()).expect("unbounded")
        } else {
            first
        }
    } else {
        (1..).map(|n| format!("{stem}-draft-{n}")).find(|n| !outbox.join(format!("{n}.mp4")).exists()).expect("unbounded")
    };
    let out = outbox.join(format!("{name}.mp4"));
    copy_new(&video, &out)?;
    if let Some(s) = srt {
        copy_new(&s, &outbox.join(format!("{name}.srt")))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("arm-inbox-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn init_creates_folders_and_rules_without_overwriting() {
        let base = tmp("init");
        let created = init(&base).unwrap();
        assert_eq!(created, ["inbox/", "outbox/", "my-sounds/", "my-rules.md"]);
        std::fs::write(base.join("my-rules.md"), "MY OWN RULES").unwrap();
        assert!(init(&base).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(base.join("my-rules.md")).unwrap(), "MY OWN RULES");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn inbox_lists_videos_with_status_and_ignores_others() {
        let base = tmp("list");
        init(&base).unwrap();
        for f in ["a.mp4", "b.MOV", "notes.txt", ".hidden.mp4"] {
            std::fs::write(base.join("inbox").join(f), b"x").unwrap();
        }
        std::fs::write(base.join("outbox/b-draft-1.mp4"), b"x").unwrap();
        std::fs::write(base.join("outbox/b-draft-2.mp4"), b"x").unwrap();
        std::fs::write(base.join("outbox/a-FINAL.mp4"), b"x").unwrap();
        let items = list_inbox(&base.join("inbox"), &base.join("outbox")).unwrap();
        let s: Vec<(String, String)> = items.iter().map(|i| (i.file.clone(), i.status())).collect();
        assert_eq!(s, [("a.mp4".into(), "final".into()), ("b.MOV".into(), "2 draft(s), awaiting review".into())]);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn publish_numbers_drafts_copies_srt_and_never_overwrites() {
        let base = tmp("pub");
        init(&base).unwrap();
        let proj = base.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("output.mp4"), b"VIDEO").unwrap();
        std::fs::write(proj.join("output.srt"), b"SRT").unwrap();
        std::fs::write(proj.join("source.json"), r#"{"fileName":"My Clip: v1.mp4"}"#).unwrap();
        let rep = format!(r#"{{"output":{{"output_path":"{}"}},"srt":"{}"}}"#, proj.join("output.mp4").display(), proj.join("output.srt").display());
        std::fs::write(proj.join("render_report.json"), rep).unwrap();
        let out = base.join("outbox");

        let d1 = publish(&proj, &out, false).unwrap();
        let d2 = publish(&proj, &out, false).unwrap();
        assert_eq!(d1.file_name().unwrap(), "My Clip_ v1-draft-1.mp4");
        assert_eq!(d2.file_name().unwrap(), "My Clip_ v1-draft-2.mp4");
        assert_eq!(std::fs::read(&d1).unwrap(), b"VIDEO");
        assert_eq!(std::fs::read(out.join("My Clip_ v1-draft-1.srt")).unwrap(), b"SRT");
        let f1 = publish(&proj, &out, true).unwrap();
        let f2 = publish(&proj, &out, true).unwrap();
        assert_eq!(f1.file_name().unwrap(), "My Clip_ v1-FINAL.mp4");
        assert_eq!(f2.file_name().unwrap(), "My Clip_ v1-FINAL-2.mp4");
        // The project's own render is untouched, and nothing was left half-copied.
        assert_eq!(std::fs::read(proj.join("output.mp4")).unwrap(), b"VIDEO");
        assert!(!std::fs::read_dir(&out).unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(".partial")));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn publish_fails_cleanly_without_a_render() {
        let base = tmp("nopub");
        let proj = base.join("p");
        std::fs::create_dir_all(&proj).unwrap();
        assert!(publish(&proj, &base.join("outbox"), false).is_err());
        std::fs::remove_dir_all(&base).ok();
    }
}
