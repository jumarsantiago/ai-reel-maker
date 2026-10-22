//! The user's spelling dictionary: corrections made in the caption review ("Nakakarang lay" -> "Nakakangalay")
//! are remembered and applied to the captions of the next videos automatically.
//! Stored in `~/Library/Application Support/ai-reel-maker/dictionary.json`.

use crate::plan::{normalize, EditPlan};
use serde::{Deserialize, Serialize};
use std::path::Path;

const MAX_ENTRIES: usize = 2000;
const MAX_WORDS: usize = 4;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fix {
    pub from: String,
    pub to: String,
}

pub fn load(dir: &Path) -> Vec<Fix> {
    std::fs::read_to_string(dir.join("dictionary.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(dir: &Path, fixes: &[Fix]) -> Result<(), String> {
    let clean: Vec<Fix> = fixes
        .iter()
        .filter(|f| !f.from.trim().is_empty() && !f.to.trim().is_empty() && f.from.split_whitespace().count() <= MAX_WORDS)
        .take(MAX_ENTRIES)
        .map(|f| Fix { from: f.from.trim().to_string(), to: f.to.trim().to_string() })
        .collect();
    std::fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    let tmp = dir.join(".dictionary.json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&clean).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join("dictionary.json")).map_err(|e| e.to_string())
}

/// Splits into words, keeping each word's leading/trailing punctuation apart from its comparable core.
fn words(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

/// Applies every fix (whole words, case-insensitive, punctuation kept) to one caption. Returns the new text.
pub fn apply(text: &str, fixes: &[Fix]) -> String {
    let mut toks: Vec<String> = words(text).iter().map(|s| s.to_string()).collect();
    for f in fixes {
        let from: Vec<String> = words(&f.from).iter().map(|w| normalize(w)).collect();
        if from.is_empty() || from.iter().any(|w| w.is_empty()) {
            continue;
        }
        let mut i = 0;
        while i + from.len() <= toks.len() {
            let window: Vec<String> = toks[i..i + from.len()].iter().map(|w| normalize(w)).collect();
            if window == from {
                // Keep punctuation that followed the last replaced word ("lay," -> "Nakakangalay,").
                let last = &toks[i + from.len() - 1];
                let tail: String = last.chars().rev().take_while(|c| !c.is_alphanumeric()).collect::<Vec<_>>().into_iter().rev().collect();
                let mut to = f.to.trim().to_string();
                if !tail.is_empty() && !to.ends_with(&tail) {
                    to.push_str(&tail);
                }
                let repl: Vec<String> = words(&to).iter().map(|s| s.to_string()).collect();
                let n = repl.len();
                toks.splice(i..i + from.len(), repl);
                i += n.max(1);
            } else {
                i += 1;
            }
        }
    }
    toks.join(" ")
}

/// Applies the dictionary to every caption in a plan; changed captions are marked as the user's own text.
pub fn apply_to_plan(plan: &mut EditPlan, fixes: &[Fix]) -> usize {
    let mut changed = 0;
    for c in &mut plan.captions {
        let new = apply(&c.text, fixes);
        if new != c.text {
            c.text = new;
            c.trusted = true;
            changed += 1;
        }
    }
    changed
}

/// Word-level corrections between a caption before and after the user fixed it. Same word count: each
/// changed word is its own fix. Otherwise the whole caption (if short) is one fix.
pub fn learn(before: &str, after: &str) -> Vec<Fix> {
    let (b, a) = (words(before), words(after));
    if normalize(before) == normalize(after) || a.is_empty() {
        return Vec::new();
    }
    let strip = |w: &str| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string();
    if b.len() == a.len() {
        return b
            .iter()
            .zip(&a)
            .filter(|(x, y)| normalize(x) != normalize(y))
            .map(|(x, y)| Fix { from: strip(x), to: strip(y) })
            .filter(|f| !f.from.is_empty() && !f.to.is_empty())
            .collect();
    }
    if b.len() <= MAX_WORDS {
        let from = b.iter().map(|w| strip(w)).collect::<Vec<_>>().join(" ");
        let to = a.iter().map(|w| strip(w)).collect::<Vec<_>>().join(" ");
        return vec![Fix { from, to }];
    }
    Vec::new()
}

/// Adds new fixes; a later fix for the same words replaces the older one.
pub fn merge(existing: &mut Vec<Fix>, new: Vec<Fix>) -> usize {
    let mut added = 0;
    for f in new {
        let key = normalize(&f.from);
        if let Some(old) = existing.iter_mut().find(|x| normalize(&x.from) == key) {
            if old.to != f.to {
                old.to = f.to;
                added += 1;
            }
        } else {
            existing.push(f);
            added += 1;
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix(a: &str, b: &str) -> Fix {
        Fix { from: a.into(), to: b.into() }
    }

    #[test]
    fn applies_whole_words_case_insensitive_keeping_punctuation() {
        let d = vec![fix("Nakakarang lay", "Nakakangalay"), fix("tiga", "matigas"), fix("logitec", "Logitech")];
        assert_eq!(apply("Nakakarang lay talaga", &d), "Nakakangalay talaga");
        assert_eq!(apply("tiga mo, sobrang", &d), "matigas mo, sobrang");
        assert_eq!(apply("mouse ng logitec.", &d), "mouse ng Logitech.");
        assert_eq!(apply("tigas na tiga!", &d), "tigas na matigas!", "only whole words");
        assert_eq!(apply("walang babaguhin", &d), "walang babaguhin");
    }

    #[test]
    fn learns_word_fixes_and_short_phrases() {
        assert_eq!(learn("Kasi naman, tiga mo,", "Kasi naman, matigas mo,"), vec![fix("tiga", "matigas")]);
        assert_eq!(learn("Nakakarang lay", "Nakakangalay"), vec![fix("Nakakarang lay", "Nakakangalay")]);
        assert!(learn("same text", "Same text!").is_empty(), "case/punctuation only is not a spelling fix");
        assert!(learn("one two three four five", "a b").is_empty(), "long rewrites are not learned");
    }

    #[test]
    fn merge_replaces_older_fix_and_plan_apply_marks_trusted() {
        let mut d = vec![fix("tiga", "matiga")];
        assert_eq!(merge(&mut d, vec![fix("Tiga", "matigas"), fix("lay", "ngalay")]), 2);
        assert_eq!(d, vec![fix("tiga", "matigas"), fix("lay", "ngalay")]);
        let mut plan: EditPlan = serde_json::from_value(serde_json::json!({
            "summary": {"title": "", "overall_rationale": "", "overall_confidence": "high"},
            "segments": [], "removals": [],
            "captions": [{"segment_index": 0, "start": 0.0, "end": 1.0, "text": "tiga mo"}, {"segment_index": 0, "start": 1.0, "end": 2.0, "text": "okay"}]
        }))
        .unwrap();
        assert_eq!(apply_to_plan(&mut plan, &d), 1);
        assert_eq!(plan.captions[0].text, "matigas mo");
        assert!(plan.captions[0].trusted && !plan.captions[1].trusted);
    }

    #[test]
    fn save_and_load() {
        let dir = std::env::temp_dir().join(format!("arm-dict-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load(&dir).is_empty());
        save(&dir, &[fix(" tiga ", "matigas"), fix("", "x"), fix("one two three four five", "x")]).unwrap();
        assert_eq!(load(&dir), vec![fix("tiga", "matigas")]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
