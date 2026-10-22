//! Optional hook, feature tags and call-to-action for a reel, read from `extras.json` in the project folder.
//! Each one is a caption-style PNG overlay, so nothing here needs a new render path.
//!
//! ```json
//! { "hook": "NANGANGALAY NA KAMAY MO?", "hookSec": 3.0,
//!   "tags": [ { "text": "ERGONOMIC", "phrase": "ergonomic", "dur": 3.5 }, { "text": "BLUETOOTH", "start": 20.0 } ],
//!   "cta": "CLICK THE YELLOW BASKET", "ctaSec": 3.5 }
//! ```
//! `phrase` starts a tag at the first caption containing that text (case-insensitive); `start` is output seconds.

use crate::captions::{self, CaptionStyle};
use crate::plan::ValidCaption;
use crate::render::{Orientation, Overlay};
use ab_glyph::{Font, FontVec};
use serde::{Deserialize, Serialize};
use std::path::Path;

const MIN_SEC: f64 = 0.8;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Extras {
    pub hook: Option<String>,
    pub hook_sec: Option<f64>,
    pub tags: Vec<TagSpec>,
    pub cta: Option<String>,
    pub cta_sec: Option<f64>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TagSpec {
    pub text: String,
    pub phrase: Option<String>,
    pub start: Option<f64>,
    pub dur: Option<f64>,
}

/// `Ok(None)` when the project has no `extras.json`.
pub fn load(dir: &Path) -> Result<Option<Extras>, String> {
    let path = dir.join("extras.json");
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&raw).map(Some).map_err(|e| format!("{}: {e}", path.display()))
}

/// A (start, end, text, y) placement on the output timeline, before rendering to PNG.
#[derive(Debug, PartialEq)]
pub struct Placed {
    pub kind: Kind,
    pub text: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Hook,
    Tag,
    Cta,
}

fn norm(t: &str) -> String {
    t.chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect::<String>().to_lowercase()
}

/// Start time of the caption where `phrase` begins, matching across neighbouring captions
/// (captions hold only 1-3 words, so a phrase usually spans several).
fn find_phrase(captions: &[ValidCaption], phrase: &str) -> Option<f64> {
    let words: Vec<&str> = phrase.split_whitespace().collect();
    let phrase = words.iter().map(|w| norm(w)).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    if phrase.is_empty() {
        return None;
    }
    let mut joined = String::new();
    let mut starts = Vec::new(); // (byte offset in `joined`, caption index)
    for (i, c) in captions.iter().enumerate() {
        for w in c.text.split_whitespace() {
            let w = norm(w);
            if w.is_empty() {
                continue;
            }
            starts.push((joined.len(), i));
            joined.push_str(&w);
            joined.push(' ');
        }
    }
    let hay = joined.trim_end();
    let needle = phrase.as_str();
    let mut from = 0;
    while let Some(pos) = hay[from..].find(needle) {
        let at = from + pos;
        let word_start = at == 0 || hay.as_bytes()[at - 1] == b' ';
        if word_start {
            let idx = starts.iter().rev().find(|(off, _)| *off <= at).map(|(_, i)| *i)?;
            return Some(captions[idx].out_start);
        }
        from = at + 1;
    }
    None
}

/// Decides when each extra is shown. Tags never overlap the hook, the CTA or each other, and everything is
/// clamped to the video length; anything that would be shorter than 0.8s is dropped.
pub fn place(extras: &Extras, captions: &[ValidCaption], total: f64) -> (Vec<Placed>, Vec<String>) {
    let mut out = Vec::new();
    let mut notes = Vec::new();
    let mut hook_end = 0.0;
    if let Some(h) = extras.hook.as_ref().filter(|h| !h.trim().is_empty()) {
        hook_end = extras.hook_sec.unwrap_or(3.0).clamp(MIN_SEC, 6.0).min(total);
        out.push(Placed { kind: Kind::Hook, text: h.trim().to_string(), start: 0.0, end: hook_end });
    }
    let cta_start = extras.cta.as_ref().filter(|c| !c.trim().is_empty()).map(|_| (total - extras.cta_sec.unwrap_or(3.5).clamp(MIN_SEC, 8.0)).max(hook_end));
    let mut cursor = hook_end;
    let mut tags: Vec<(f64, &TagSpec)> = Vec::new();
    // Tags with neither a phrase nor a start time are spread evenly between the hook and the CTA.
    let free = extras.tags.iter().filter(|t| t.phrase.as_deref().map_or(true, |p| p.trim().is_empty()) && t.start.is_none() && !t.text.trim().is_empty()).count();
    let span_end = cta_start.unwrap_or(total).min(total);
    let mut free_i = 0usize;
    for t in &extras.tags {
        let want = match (t.phrase.as_deref().map(str::trim).filter(|p| !p.is_empty()), t.start) {
            (_, Some(s)) => Some(s),
            (Some(p), None) => find_phrase(captions, p),
            (None, None) => {
                free_i += 1;
                Some(hook_end + (span_end - hook_end) * free_i as f64 / (free + 1) as f64 - 1.5)
            }
        };
        match want {
            Some(s) if !t.text.trim().is_empty() => tags.push((s.max(0.0), t)),
            _ => notes.push(format!("tag \"{}\" skipped: no phrase match or start time", t.text)),
        }
    }
    tags.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (s, t) in tags {
        let start = s.max(cursor);
        let limit = cta_start.unwrap_or(total).min(total);
        let end = (start + t.dur.unwrap_or(3.5)).min(limit);
        if end - start < MIN_SEC {
            notes.push(format!("tag \"{}\" skipped: no room at {:.1}s", t.text, s));
            continue;
        }
        out.push(Placed { kind: Kind::Tag, text: t.text.trim().to_string(), start, end });
        cursor = end;
    }
    if let (Some(c), Some(s)) = (extras.cta.as_ref(), cta_start) {
        if total - s >= MIN_SEC {
            out.push(Placed { kind: Kind::Cta, text: c.trim().to_string(), start: s, end: total });
        }
    }
    (out, notes)
}

/// Drops characters the font cannot draw (they would show as empty boxes) and tidies the spacing.
pub fn drawable(font: &FontVec, text: &str) -> String {
    let kept: String = text.chars().filter(|c| c.is_whitespace() || font.glyph_id(*c).0 != 0).collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Renders each placed extra to a PNG in `cap_dir` and returns the overlays.
pub fn overlays(font: &FontVec, placed: &[Placed], orientation: Orientation, cap_dir: &Path) -> Result<Vec<Overlay>, String> {
    let width = orientation.short_side();
    let mut out = Vec::new();
    for (i, p) in placed.iter().enumerate() {
        let (style, y) = match p.kind {
            Kind::Hook => (CaptionStyle::hook(width), orientation.hook_y()),
            Kind::Tag => (CaptionStyle::tag(width), orientation.tag_y()),
            Kind::Cta => (CaptionStyle::cta(width), orientation.cta_y()),
        };
        let png = cap_dir.join(format!("x{i:02}-{:?}.png", p.kind).to_lowercase());
        let text = drawable(font, &p.text);
        if text.is_empty() {
            continue;
        }
        captions::render_png(font, &style, &text, &png)?;
        out.push(Overlay { image: png, start: p.start, end: p.end, y_center_frac: y });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(t: &str, a: f64, b: f64) -> ValidCaption {
        ValidCaption { text: t.into(), out_start: a, out_end: b }
    }

    #[test]
    fn places_hook_tags_and_cta_without_overlap() {
        let e = Extras {
            hook: Some("Hook".into()),
            cta: Some("Click".into()),
            tags: vec![
                TagSpec { text: "A".into(), phrase: Some("ergonomic".into()), ..Default::default() },
                TagSpec { text: "B".into(), start: Some(1.0), ..Default::default() },
                TagSpec { text: "C".into(), phrase: Some("nope".into()), ..Default::default() },
            ],
            ..Default::default()
        };
        let caps = vec![cap("sobrang", 0.0, 2.0), cap("Ergonomic niya", 8.0, 9.0)];
        let (p, notes) = place(&e, &caps, 20.0);
        let kinds: Vec<_> = p.iter().map(|x| x.kind).collect();
        assert_eq!(kinds, vec![Kind::Hook, Kind::Tag, Kind::Tag, Kind::Cta]);
        assert_eq!((p[0].start, p[0].end), (0.0, 3.0));
        assert_eq!(p[1].text, "B");
        assert_eq!(p[1].start, 3.0); // pushed after the hook
        assert_eq!(p[2].text, "A");
        assert!(p[1].end <= p[2].start && p[2].end <= p[3].start);
        assert_eq!((p[3].start, p[3].end), (16.5, 20.0));
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn phrase_matches_across_captions_and_ignores_punctuation() {
        let caps = vec![cap("without any", 5.0, 5.6), cap("pain in my", 5.6, 6.2), cap("hands.", 6.2, 6.8)];
        assert_eq!(find_phrase(&caps, "Without any pain"), Some(5.0));
        assert_eq!(find_phrase(&caps, "pain in my hands"), Some(5.6));
        assert_eq!(find_phrase(&caps, "ain in"), None);
        assert_eq!(find_phrase(&caps, "bluetooth"), None);
    }

    #[test]
    fn tags_without_anchor_are_spread_out() {
        let e = Extras {
            tags: vec![TagSpec { text: "A".into(), ..Default::default() }, TagSpec { text: "B".into(), ..Default::default() }],
            ..Default::default()
        };
        let (p, _) = place(&e, &[], 30.0);
        assert_eq!(p.len(), 2);
        assert!(p[0].start < p[1].start && p[0].end <= p[1].start);
    }

    #[test]
    fn drawable_drops_missing_glyphs() {
        let Ok(font) = captions::load_font_for(captions::SOFT_FONT) else { return };
        assert_eq!(drawable(&font, "CLICK THE BASKET \u{2193}"), "CLICK THE BASKET");
    }

    #[test]
    fn empty_extras_place_nothing() {
        let (p, n) = place(&Extras::default(), &[], 10.0);
        assert!(p.is_empty() && n.is_empty());
    }
}
