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
    /// How long the hook stays (seconds), when no end time is given. Defaults to 3.
    pub hook_sec: Option<f64>,
    /// When the hook appears (seconds on the reel). Defaults to 0.
    pub hook_start: Option<f64>,
    /// When the hook goes away. Overrides `hook_sec`.
    pub hook_end: Option<f64>,
    /// Keep the hook up until the very end of the video.
    pub hook_until_end: bool,
    /// Size multiplier (1.0 = normal; 0.6 to 1.8).
    pub hook_size: Option<f64>,
    /// "top", "middle" or "bottom"; `hook_y` overrides it.
    pub hook_position: Option<String>,
    /// Exact vertical position as a fraction of the height (0.05 to 0.95), e.g. from dragging in the preview.
    pub hook_y: Option<f64>,
    /// "none", "fade" or "slide".
    pub hook_anim: Option<crate::render::Anim>,
    pub tags: Vec<TagSpec>,
    pub cta: Option<String>,
    pub cta_sec: Option<f64>,
    /// Animated cards, stickers, arrows and lower thirds (rendered by HyperFrames).
    pub motion: Vec<crate::motion::MotionSpec>,
    /// The HyperFrames kit folder chosen in Settings (the usual places are tried when empty).
    pub kit_dir: Option<String>,
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
    /// Vertical position override (fraction of the height).
    pub y: Option<f64>,
    /// Text size multiplier.
    pub scale: f64,
    pub anim: crate::render::Anim,
}

impl Placed {
    fn plain(kind: Kind, text: String, start: f64, end: f64) -> Self {
        Placed { kind, text, start, end, y: None, scale: 1.0, anim: crate::render::Anim::None }
    }
}

/// Seconds after which a hook is "long-lived": tags then show beneath it instead of waiting for it to leave.
const PERSISTENT_HOOK_SEC: f64 = 6.0;
/// How far a tag moves down to sit beneath a long-lived hook (fraction of the height).
const TAG_UNDER_HOOK: f64 = 0.115;

/// The hook's vertical position: an exact `hook_y`, else a named place, else the default.
pub fn hook_y(extras: &Extras, orientation: Orientation) -> f64 {
    if let Some(y) = extras.hook_y.filter(|y| y.is_finite()) {
        return y.clamp(0.05, 0.95);
    }
    let vertical = orientation == Orientation::Vertical;
    match extras.hook_position.as_deref() {
        Some("middle") => 0.42,
        Some("bottom") => if vertical { 0.86 } else { 0.9 },
        _ => orientation.hook_y(),
    }
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
pub fn find_phrase(captions: &[ValidCaption], phrase: &str) -> Option<f64> {
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
pub fn place(extras: &Extras, captions: &[ValidCaption], total: f64, orientation: Orientation) -> (Vec<Placed>, Vec<String>) {
    let mut out = Vec::new();
    let mut notes = Vec::new();
    let (mut hook_start, mut hook_end, mut persistent) = (0.0, 0.0, false);
    if let Some(h) = extras.hook.as_ref().filter(|h| !h.trim().is_empty()) {
        hook_start = extras.hook_start.filter(|s| s.is_finite()).unwrap_or(0.0).clamp(0.0, (total - MIN_SEC).max(0.0));
        hook_end = if extras.hook_until_end {
            total
        } else if let Some(e) = extras.hook_end.filter(|e| e.is_finite()) {
            e
        } else {
            hook_start + extras.hook_sec.unwrap_or(3.0)
        }
        .clamp(hook_start + MIN_SEC, total.max(hook_start + MIN_SEC))
        .min(total);
        persistent = extras.hook_until_end || hook_end - hook_start > PERSISTENT_HOOK_SEC;
        out.push(Placed {
            kind: Kind::Hook,
            text: h.trim().to_string(),
            start: hook_start,
            end: hook_end,
            y: Some(hook_y(extras, orientation)),
            scale: extras.hook_size.filter(|s| s.is_finite()).unwrap_or(1.0).clamp(0.6, 1.8),
            anim: extras.hook_anim.unwrap_or_default(),
        });
    }
    // A short hook is not shared with tags (they wait for it to leave); a long-lived one is, with the tags below it.
    let tags_wait = if persistent { 0.0 } else { hook_end };
    let cta_start = extras.cta.as_ref().filter(|c| !c.trim().is_empty()).map(|_| (total - extras.cta_sec.unwrap_or(3.5).clamp(MIN_SEC, 8.0)).max(tags_wait));
    let mut cursor = tags_wait;
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
                Some(tags_wait + (span_end - tags_wait) * free_i as f64 / (free + 1) as f64 - 1.5)
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
        let under_hook = persistent && start < hook_end && end > hook_start;
        let y = under_hook.then(|| (hook_y(extras, orientation) + TAG_UNDER_HOOK).min(0.95));
        out.push(Placed { y, ..Placed::plain(Kind::Tag, t.text.trim().to_string(), start, end) });
        cursor = end;
    }
    if let (Some(c), Some(s)) = (extras.cta.as_ref(), cta_start) {
        if total - s >= MIN_SEC {
            out.push(Placed::plain(Kind::Cta, c.trim().to_string(), s, total));
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
        let (mut style, default_y) = match p.kind {
            Kind::Hook => (CaptionStyle::hook(width), orientation.hook_y()),
            Kind::Tag => (CaptionStyle::tag(width), orientation.tag_y()),
            Kind::Cta => (CaptionStyle::cta(width), orientation.cta_y()),
        };
        style.font_px *= p.scale as f32;
        let y = p.y.unwrap_or(default_y);
        let png = cap_dir.join(format!("x{i:02}-{:?}.png", p.kind).to_lowercase());
        let text = drawable(font, &p.text);
        if text.is_empty() {
            continue;
        }
        captions::render_png(font, &style, &text, &png)?;
        out.push(Overlay { image: png, start: p.start, end: p.end, y_center_frac: y, anim: p.anim });
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
        let (p, notes) = place(&e, &caps, 20.0, Orientation::Vertical);
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
        let (p, _) = place(&e, &[], 30.0, Orientation::Vertical);
        assert_eq!(p.len(), 2);
        assert!(p[0].start < p[1].start && p[0].end <= p[1].start);
    }

    #[test]
    fn drawable_drops_missing_glyphs() {
        let Ok(font) = captions::load_font_for(captions::SOFT_FONT) else { return };
        assert_eq!(drawable(&font, "CLICK THE BASKET \u{2193}"), "CLICK THE BASKET");
    }

    fn hook(h: &str) -> Extras {
        Extras { hook: Some(h.into()), ..Default::default() }
    }

    fn hook_of(p: &[Placed]) -> &Placed {
        p.iter().find(|x| x.kind == Kind::Hook).expect("a hook")
    }

    #[test]
    fn hook_duration_start_end_and_until_the_end() {
        let v = Orientation::Vertical;
        // Default: 3 s from the start.
        let (p, _) = place(&hook("H"), &[], 30.0, v);
        assert_eq!((hook_of(&p).start, hook_of(&p).end), (0.0, 3.0));
        // A custom length, longer than the old 6 s limit.
        let (p, _) = place(&Extras { hook_sec: Some(12.0), ..hook("H") }, &[], 30.0, v);
        assert_eq!(hook_of(&p).end, 12.0);
        // Custom start and end win over the length.
        let (p, _) = place(&Extras { hook_start: Some(2.0), hook_end: Some(9.5), hook_sec: Some(1.0), ..hook("H") }, &[], 30.0, v);
        assert_eq!((hook_of(&p).start, hook_of(&p).end), (2.0, 9.5));
        // Until the end of the video.
        let (p, _) = place(&Extras { hook_until_end: true, hook_end: Some(4.0), ..hook("H") }, &[], 30.0, v);
        assert_eq!((hook_of(&p).start, hook_of(&p).end), (0.0, 30.0));
        // Edge cases: a start near the end keeps at least the minimum (0.8 s); an end before the start is pushed out;
        // nonsense numbers fall back to the defaults; a hook never outlasts the video.
        let (p, _) = place(&Extras { hook_start: Some(29.9), ..hook("H") }, &[], 30.0, v);
        assert!(hook_of(&p).end - hook_of(&p).start >= MIN_SEC - 1e-9 && hook_of(&p).end <= 30.0, "{:?}", hook_of(&p));
        let (p, _) = place(&Extras { hook_start: Some(10.0), hook_end: Some(5.0), ..hook("H") }, &[], 30.0, v);
        assert!(hook_of(&p).end >= 10.0 + MIN_SEC - 1e-9, "an end before the start is pushed out to the minimum length");
        let (p, _) = place(&Extras { hook_start: Some(f64::NAN), hook_end: Some(f64::INFINITY), ..hook("H") }, &[], 30.0, v);
        assert_eq!((hook_of(&p).start, hook_of(&p).end), (0.0, 3.0), "not-a-number and infinity fall back to the defaults");
        let (p, _) = place(&Extras { hook_sec: Some(99.0), ..hook("H") }, &[], 8.0, v);
        assert_eq!(hook_of(&p).end, 8.0);
        let (p, _) = place(&hook("H"), &[], 1.5, v);
        assert!(hook_of(&p).end <= 1.5);
        let (p, _) = place(&hook("   "), &[], 30.0, v);
        assert!(p.is_empty(), "a blank hook is no hook");
    }

    #[test]
    fn hook_size_position_and_animation() {
        let v = Orientation::Vertical;
        let (p, _) = place(&hook("H"), &[], 30.0, v);
        assert_eq!((hook_of(&p).scale, hook_of(&p).y, hook_of(&p).anim), (1.0, Some(v.hook_y()), crate::render::Anim::None));
        let e = Extras { hook_size: Some(5.0), hook_position: Some("middle".into()), hook_anim: Some(crate::render::Anim::Slide), ..hook("H") };
        let (p, _) = place(&e, &[], 30.0, v);
        assert_eq!((hook_of(&p).scale, hook_of(&p).y, hook_of(&p).anim), (1.8, Some(0.42), crate::render::Anim::Slide), "size is capped");
        let e = Extras { hook_size: Some(0.1), hook_y: Some(2.0), hook_position: Some("bottom".into()), ..hook("H") };
        let (p, _) = place(&e, &[], 30.0, v);
        assert_eq!((hook_of(&p).scale, hook_of(&p).y), (0.6, Some(0.95)), "an exact y beats a named place, both are bounded");
        assert!(hook_y(&Extras { hook_position: Some("bottom".into()), ..Default::default() }, Orientation::Horizontal) > 0.85);
    }

    #[test]
    fn tags_wait_for_a_short_hook_but_sit_under_a_long_one() {
        let v = Orientation::Vertical;
        let tag = |s: f64| TagSpec { text: "T".into(), start: Some(s), dur: Some(3.0), ..Default::default() };
        let short = Extras { tags: vec![tag(1.0)], ..hook("H") };
        let (p, _) = place(&short, &[], 30.0, v);
        let t = p.iter().find(|x| x.kind == Kind::Tag).unwrap();
        assert_eq!((t.start, t.y), (3.0, None), "waits for the 3 s hook and keeps its own place");
        let long = Extras { tags: vec![tag(1.0)], hook_until_end: true, ..hook("H") };
        let (p, _) = place(&long, &[], 30.0, v);
        let t = p.iter().find(|x| x.kind == Kind::Tag).unwrap();
        assert_eq!(t.start, 1.0, "no waiting for a hook that stays");
        assert!((t.y.unwrap() - (v.hook_y() + 0.115)).abs() < 1e-9, "sits under the hook");
        // A tag that falls outside a timed long hook keeps its normal place.
        let timed = Extras { tags: vec![tag(20.0)], hook_start: Some(0.0), hook_end: Some(8.0), ..hook("H") };
        let (p, _) = place(&timed, &[], 30.0, v);
        assert_eq!(p.iter().find(|x| x.kind == Kind::Tag).unwrap().y, None);
    }

    #[test]
    fn overlays_apply_size_position_and_animation() {
        let Ok(font) = captions::load_font_for(captions::SOFT_FONT) else { return };
        let dir = std::env::temp_dir().join(format!("arm-hook-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mk = |scale: f64| Placed { y: Some(0.3), scale, anim: crate::render::Anim::Fade, ..Placed::plain(Kind::Hook, "HELLO WORLD".into(), 0.0, 3.0) };
        let small = overlays(&font, &[mk(0.7)], Orientation::Vertical, &dir).unwrap();
        let big = overlays(&font, &[Placed { text: "HELLO WORLD".into(), ..mk(1.5) }], Orientation::Vertical, &dir.join("b")).unwrap_or_else(|_| {
            std::fs::create_dir_all(dir.join("b")).unwrap();
            overlays(&font, &[mk(1.5)], Orientation::Vertical, &dir.join("b")).unwrap()
        });
        assert_eq!((small[0].y_center_frac, small[0].anim), (0.3, crate::render::Anim::Fade));
        let (w1, h1) = image::image_dimensions(&small[0].image).unwrap();
        let (w2, h2) = image::image_dimensions(&big[0].image).unwrap();
        assert!(w2 > w1 && h2 > h1, "a larger hook draws a larger image ({w1}x{h1} vs {w2}x{h2})");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_extras_place_nothing() {
        let (p, n) = place(&Extras::default(), &[], 10.0, Orientation::Vertical);
        assert!(p.is_empty() && n.is_empty());
    }
}
