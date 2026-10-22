//! Edit-decision plan: the JSON schema Claude must fill, the exact Messages API request body we
//! would send (transcript + metadata only, never media), and the validator that turns a returned
//! plan into checked render instructions. Nothing in a plan is ever executed - it only becomes
//! numbers and strings fed to our own FFmpeg argument builder.

use crate::transcribe::{Transcript, Word};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
pub const MAX_TOKENS: u32 = 16000;
pub const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

const MIN_SEGMENT_SEC: f64 = 0.3;
const MIN_CAPTION_SEC: f64 = 0.2;
const OVERLAP_TOL_SEC: f64 = 0.05;
const EVIDENCE_TOL_SEC: f64 = 0.3;
const SNAP_EDGE_SEC: f64 = 0.02;
const CAPTION_MATCH_MIN: f64 = 0.6;

#[derive(Debug, Clone, Serialize)]
pub struct PlanSettings {
    pub target_min_sec: u32,
    pub target_max_sec: u32,
    pub width: u32,
    pub height: u32,
}

impl Default for PlanSettings {
    fn default() -> Self {
        Self { target_min_sec: 10, target_max_sec: 60, width: 1080, height: 1920 }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanSummary {
    pub title: String,
    pub overall_rationale: String,
    pub overall_confidence: Confidence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanSegment {
    pub source_id: String,
    pub start: f64,
    pub end: f64,
    pub purpose: String,
    pub rationale: String,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRemoval {
    pub start: f64,
    pub end: f64,
    pub kind: String,
    pub evidence_word_ids: Vec<i64>,
    pub rationale: String,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanCaption {
    pub segment_index: i64,
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditPlan {
    pub summary: PlanSummary,
    pub segments: Vec<PlanSegment>,
    pub removals: Vec<PlanRemoval>,
    pub captions: Vec<PlanCaption>,
}

pub const REMOVAL_KINDS: &[&str] = &["dead_air", "filler", "repeat", "mistake", "off_topic"];

/// JSON schema for structured outputs. Every object closes with additionalProperties:false and
/// lists all fields as required (structured-output requirements); ranges are checked by `validate`.
pub fn plan_schema() -> Value {
    let conf = json!({"type": "string", "enum": ["low", "medium", "high"]});
    json!({
        "type": "object",
        "properties": {
            "summary": {
                "type": "object",
                "properties": {
                    "title": {"type": "string"},
                    "overall_rationale": {"type": "string"},
                    "overall_confidence": conf
                },
                "required": ["title", "overall_rationale", "overall_confidence"],
                "additionalProperties": false
            },
            "segments": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "source_id": {"type": "string"},
                        "start": {"type": "number"},
                        "end": {"type": "number"},
                        "purpose": {"type": "string"},
                        "rationale": {"type": "string"},
                        "confidence": conf
                    },
                    "required": ["source_id", "start", "end", "purpose", "rationale", "confidence"],
                    "additionalProperties": false
                }
            },
            "removals": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "start": {"type": "number"},
                        "end": {"type": "number"},
                        "kind": {"type": "string", "enum": REMOVAL_KINDS},
                        "evidence_word_ids": {"type": "array", "items": {"type": "integer"}},
                        "rationale": {"type": "string"},
                        "confidence": conf
                    },
                    "required": ["start", "end", "kind", "evidence_word_ids", "rationale", "confidence"],
                    "additionalProperties": false
                }
            },
            "captions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "segment_index": {"type": "integer"},
                        "start": {"type": "number"},
                        "end": {"type": "number"},
                        "text": {"type": "string"}
                    },
                    "required": ["segment_index", "start", "end", "text"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["summary", "segments", "removals", "captions"],
        "additionalProperties": false
    })
}

pub fn system_prompt(s: &PlanSettings) -> String {
    format!(
        "You are an experienced short-form video editor for Facebook Reels, Instagram Reels and TikTok. \
The footage is usually a creator or online seller speaking Tagalog, English or Taglish. You receive a \
word-level transcript (times in seconds on the source clip) plus detected silences for one clip, and you \
return an edit decision plan for a vertical {w}x{h} video. You cannot see the picture; decide from the words \
and timing only.

Editing rules:
- Use only source_id \"clip_1\" and times between 0 and the clip duration.
- Segments are the parts to keep, listed in the order they should play. Start each one at a word's start time \
and end it at a word's end time (up to 0.15 s of padding into a silence is fine). Segments must not overlap.
- Remove dead air (pauses longer than about 0.4 s between kept words), filler words (um, uh, ah; Tagalog \
fillers such as \"ano\" or \"parang\" only when used as filler), repeated phrases, and false starts or \
verbal mistakes - but only where the transcript shows it. List each removal with the ids of the words \
that prove it (for dead air, the words on either side of the pause).
- Keep the message coherent and in its natural order unless moving a line clearly makes a stronger opening. \
Never invent content.
- Aim for {min}-{max} seconds in total; shorter is fine if the content is short.
- Captions: 1-5 words each (about 28 characters at most), in speaking order, each fully inside one segment \
(segment_index is the 0-based position in your segments list, times are source times). Use the spoken \
words; you may correct obvious transcription misspellings of Tagalog or English words, but do not change \
the meaning.
- Give a short rationale and a confidence (low, medium, high) for every segment and removal.",
        w = s.width,
        h = s.height,
        min = s.target_min_sec,
        max = s.target_max_sec
    )
}

/// Compact, human-readable transcript block: only what the edit decision needs.
pub fn user_content(t: &Transcript, width: u32, height: u32, fps: f64, s: &PlanSettings) -> String {
    let silences: Vec<String> = t.silences.iter().map(|i| format!("{:.2}-{:.2}", i.start, i.end)).collect();
    let mut out = format!(
        "clip: {id} | duration {d:.2}s | source {width}x{height} | {fps:.0} fps | audio: yes\n\
language requested: {lr} | detected: {ld}\n\
target: vertical {w}x{h}, {min}-{max} s\n\
silences (s): {sil}\n\
words (id start end text):\n",
        id = t.source_id,
        d = t.duration_sec,
        lr = t.language_requested,
        ld = if t.languages_detected.is_empty() { "unknown".into() } else { t.languages_detected.join(", ") },
        w = s.width,
        h = s.height,
        min = s.target_min_sec,
        max = s.target_max_sec,
        sil = if silences.is_empty() { "none".into() } else { silences.join(", ") },
    );
    for w in &t.words {
        out += &format!("{} {:.2} {:.2} {}\n", w.id, w.start, w.end, w.text);
    }
    out
}

/// The exact Messages API body. No credentials, file names, paths, audio, or frames.
pub fn build_request(t: &Transcript, width: u32, height: u32, fps: f64, s: &PlanSettings, model: &str) -> Value {
    json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "system": system_prompt(s),
        "messages": [{"role": "user", "content": user_content(t, width, height, fps, s)}],
        "output_config": {"effort": "medium", "format": {"type": "json_schema", "schema": plan_schema()}},
        "fallbacks": "default"
    })
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ValidSegment {
    pub start: f64,
    pub end: f64,
    pub out_start: f64,
    pub out_end: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ValidCaption {
    pub text: String,
    pub out_start: f64,
    pub out_end: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidatedPlan {
    pub segments: Vec<ValidSegment>,
    pub captions: Vec<ValidCaption>,
    pub total_sec: f64,
    pub removals_checked: usize,
    pub warnings: Vec<String>,
    /// Transcript words fully inside kept segments, in output order (for re-transcription checks).
    pub expected_words: Vec<String>,
}

pub fn normalize(word: &str) -> String {
    word.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Tolerates spelling fixes ("Salamit" -> "Salamat") and split/merged words ("langnitto").
fn token_matches(tok: &str, spoken: &[String]) -> bool {
    let joined: String = spoken.concat();
    spoken.iter().any(|w| w == tok || (tok.len() >= 4 && edit_distance(tok, w) <= 2))
        || (tok.len() >= 3 && joined.contains(tok))
}

/// Moves a boundary that falls inside a word to that word's edge, so no word is cut in half.
fn snap(t: f64, words: &[Word], is_start: bool) -> f64 {
    for w in words {
        if t > w.start + SNAP_EDGE_SEC && t < w.end - SNAP_EDGE_SEC {
            return if is_start { w.start } else { w.end };
        }
    }
    t
}

pub fn validate(plan: &EditPlan, t: &Transcript) -> Result<ValidatedPlan, Vec<String>> {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let dur = t.duration_sec;

    if plan.segments.is_empty() {
        errors.push("Plan has no segments.".into());
    }
    if plan.segments.len() > 200 {
        errors.push("Plan has more than 200 segments.".into());
    }

    let mut segs: Vec<(f64, f64)> = Vec::new();
    for (i, s) in plan.segments.iter().enumerate() {
        let tag = format!("segment {i}");
        if s.source_id != t.source_id {
            errors.push(format!("{tag}: unknown source_id {:?}.", s.source_id));
        }
        if !s.start.is_finite() || !s.end.is_finite() || s.start < 0.0 || s.end > dur + 0.05 {
            errors.push(format!("{tag}: {:.2}-{:.2} is outside 0-{dur:.2}.", s.start, s.end));
            continue;
        }
        let (a, b) = (snap(s.start, &t.words, true), snap(s.end.min(dur), &t.words, false).min(dur));
        if (a - s.start).abs() > 1e-6 || (b - s.end.min(dur)).abs() > 1e-6 {
            warnings.push(format!("{tag}: snapped {:.2}-{:.2} to word edges {a:.2}-{b:.2}.", s.start, s.end));
        }
        if b - a < MIN_SEGMENT_SEC {
            errors.push(format!("{tag}: shorter than {MIN_SEGMENT_SEC}s."));
            continue;
        }
        segs.push((a, b));
    }

    let mut sorted = segs.clone();
    sorted.sort_by(|x, y| x.0.total_cmp(&y.0));
    for w in sorted.windows(2) {
        if w[0].1 - w[1].0 > OVERLAP_TOL_SEC {
            errors.push(format!("segments {:.2}-{:.2} and {:.2}-{:.2} overlap.", w[0].0, w[0].1, w[1].0, w[1].1));
        }
    }

    for (i, r) in plan.removals.iter().enumerate() {
        let tag = format!("removal {i} ({})", r.kind);
        if !REMOVAL_KINDS.contains(&r.kind.as_str()) {
            errors.push(format!("{tag}: unknown kind."));
        }
        if !(r.start.is_finite() && r.end.is_finite() && 0.0 <= r.start && r.start < r.end && r.end <= dur + 0.05) {
            errors.push(format!("{tag}: invalid range {:.2}-{:.2}.", r.start, r.end));
            continue;
        }
        if r.evidence_word_ids.is_empty() {
            errors.push(format!("{tag}: no evidence word ids."));
        }
        for id in &r.evidence_word_ids {
            match usize::try_from(*id).ok().and_then(|i| t.words.get(i)) {
                None => errors.push(format!("{tag}: evidence word {id} does not exist.")),
                Some(w) if w.end < r.start - EVIDENCE_TOL_SEC || w.start > r.end + EVIDENCE_TOL_SEC => {
                    errors.push(format!("{tag}: evidence word {id} ({}) is not near {:.2}-{:.2}.", w.text, r.start, r.end))
                }
                _ => {}
            }
        }
        for &(a, b) in &segs {
            let overlap = r.end.min(b) - r.start.max(a);
            if overlap > OVERLAP_TOL_SEC {
                errors.push(format!("{tag}: overlaps kept segment {a:.2}-{b:.2} by {overlap:.2}s."));
            }
        }
    }

    // Output timeline in plan order.
    let mut out_segs = Vec::new();
    let mut cursor = 0.0;
    for &(a, b) in &segs {
        out_segs.push(ValidSegment { start: a, end: b, out_start: cursor, out_end: cursor + (b - a) });
        cursor += b - a;
    }

    let mut captions = Vec::new();
    for (i, c) in plan.captions.iter().enumerate() {
        let tag = format!("caption {i} {:?}", c.text);
        let Some(seg) = usize::try_from(c.segment_index).ok().and_then(|k| out_segs.get(k)) else {
            errors.push(format!("{tag}: segment_index {} does not exist.", c.segment_index));
            continue;
        };
        if c.text.trim().is_empty() {
            errors.push(format!("{tag}: empty text."));
            continue;
        }
        let (a, b) = (c.start.max(seg.start), c.end.min(seg.end));
        if !(c.start.is_finite() && c.end.is_finite()) || c.start < seg.start - 0.15 || c.end > seg.end + 0.15 || b - a < MIN_CAPTION_SEC {
            errors.push(format!("{tag}: {:.2}-{:.2} is not inside its segment {:.2}-{:.2}.", c.start, c.end, seg.start, seg.end));
            continue;
        }
        let spoken: Vec<String> = t
            .words
            .iter()
            .filter(|w| w.end > a - EVIDENCE_TOL_SEC && w.start < b + EVIDENCE_TOL_SEC)
            .map(|w| normalize(&w.text))
            .collect();
        let toks: Vec<String> = c.text.split_whitespace().map(normalize).filter(|x| !x.is_empty()).collect();
        let matched = toks.iter().filter(|tok| token_matches(tok, &spoken)).count();
        let ratio = if toks.is_empty() { 0.0 } else { matched as f64 / toks.len() as f64 };
        if ratio < CAPTION_MATCH_MIN {
            errors.push(format!("{tag}: only {matched}/{} words match what is spoken there.", toks.len()));
            continue;
        }
        let exact = toks.iter().filter(|tok| spoken.contains(tok)).count();
        if exact < toks.len() {
            warnings.push(format!("{tag}: {} word(s) differ from the transcript (spelling fixes?).", toks.len() - exact));
        }
        captions.push(ValidCaption {
            text: c.text.trim().to_string(),
            out_start: seg.out_start + (a - seg.start),
            out_end: seg.out_start + (b - seg.start),
        });
    }
    captions.sort_by(|x, y| x.out_start.total_cmp(&y.out_start));
    for k in 1..captions.len() {
        if captions[k - 1].out_end > captions[k].out_start {
            captions[k - 1].out_end = captions[k].out_start;
        }
    }
    captions.retain(|c| c.out_end - c.out_start >= 0.1);

    if errors.is_empty() && cursor < 1.0 {
        errors.push(format!("Edited video would be only {cursor:.2}s long."));
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    let expected_words = out_segs
        .iter()
        .flat_map(|s| {
            t.words
                .iter()
                .filter(move |w| w.start >= s.start - 0.05 && w.end <= s.end + 0.05)
                .map(|w| normalize(&w.text))
        })
        .filter(|w| !w.is_empty())
        .collect();

    Ok(ValidatedPlan {
        segments: out_segs,
        captions,
        total_sec: cursor,
        removals_checked: plan.removals.len(),
        warnings,
        expected_words,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcribe::Interval;

    fn transcript() -> Transcript {
        let w = |id, text: &str, start, end| Word { id, text: text.into(), start, end };
        Transcript {
            source_id: "clip_1".into(),
            duration_sec: 10.0,
            language_requested: "auto".into(),
            languages_detected: vec!["tl".into()],
            words: vec![
                w(0, "Hello", 0.1, 0.5),
                w(1, "um,", 0.6, 0.9),
                w(2, "Sobrang", 3.0, 3.5),
                w(3, "dali", 3.5, 3.9),
                w(4, "Salamit!", 6.0, 6.8),
            ],
            silences: vec![Interval { start: 0.9, end: 3.0 }, Interval { start: 3.9, end: 6.0 }],
            speech_chunks: vec![],
            whisper_model: "ggml-small.bin".into(),
            timing_method: "dtw+energy".into(),
        }
    }

    fn seg(start: f64, end: f64) -> PlanSegment {
        PlanSegment { source_id: "clip_1".into(), start, end, purpose: "p".into(), rationale: "r".into(), confidence: Confidence::High }
    }

    fn base_plan() -> EditPlan {
        EditPlan {
            summary: PlanSummary { title: "t".into(), overall_rationale: "r".into(), overall_confidence: Confidence::High },
            segments: vec![seg(3.0, 3.9), seg(0.1, 0.5), seg(6.0, 6.8)],
            removals: vec![PlanRemoval {
                start: 0.6, end: 0.9, kind: "filler".into(), evidence_word_ids: vec![1],
                rationale: "um".into(), confidence: Confidence::High,
            }],
            captions: vec![
                PlanCaption { segment_index: 0, start: 3.0, end: 3.9, text: "Sobrang dali".into() },
                PlanCaption { segment_index: 2, start: 6.0, end: 6.8, text: "Salamat!".into() },
            ],
        }
    }

    #[test]
    fn valid_plan_maps_to_output_timeline() {
        let v = validate(&base_plan(), &transcript()).unwrap();
        assert_eq!(v.segments.len(), 3);
        let s0 = &v.segments[0];
        assert_eq!((s0.start, s0.end, s0.out_start), (3.0, 3.9, 0.0));
        assert!((s0.out_end - 0.9).abs() < 1e-9);
        assert!((v.total_sec - 2.1).abs() < 1e-9);
        // Caption on segment 2 lands after segments 0 and 1 (0.9 + 0.4 = 1.3s).
        assert!((v.captions[1].out_start - 1.3).abs() < 1e-9);
        assert_eq!(v.expected_words, ["sobrang", "dali", "hello", "salamit"]);
        assert!(v.warnings.iter().any(|w| w.contains("differ"))); // Salamat vs Salamit accepted with warning
    }

    #[test]
    fn snaps_boundaries_inside_words() {
        let mut p = base_plan();
        p.segments[0] = seg(3.2, 3.7); // inside "Sobrang" .. inside "dali"
        let v = validate(&p, &transcript()).unwrap();
        assert_eq!((v.segments[0].start, v.segments[0].end), (3.0, 3.9));
        assert!(v.warnings.iter().any(|w| w.contains("snapped")));
    }

    #[test]
    fn rejects_bad_references_and_ranges() {
        let t = transcript();
        let mut p = base_plan();
        p.segments[0].source_id = "clip_9".into();
        p.segments.push(seg(8.0, 12.0));
        p.segments.push(seg(3.5, 3.9)); // overlaps segment 0
        p.removals[0].evidence_word_ids = vec![99];
        p.captions[0].segment_index = 7;
        let errs = validate(&p, &t).unwrap_err();
        let joined = errs.join("\n");
        assert!(joined.contains("unknown source_id"));
        assert!(joined.contains("outside 0-10.00"));
        assert!(joined.contains("overlap"));
        assert!(joined.contains("evidence word 99 does not exist"));
        assert!(joined.contains("segment_index 7"));
    }

    #[test]
    fn rejects_removal_that_cuts_kept_speech_and_invented_captions() {
        let t = transcript();
        let mut p = base_plan();
        p.removals[0].start = 3.0;
        p.removals[0].end = 3.9;
        p.removals[0].evidence_word_ids = vec![2];
        p.captions[0].text = "Buy now limited offer".into();
        let joined = validate(&p, &t).unwrap_err().join("\n");
        assert!(joined.contains("overlaps kept segment"));
        assert!(joined.contains("match what is spoken"));
    }

    #[test]
    fn rejects_far_evidence_and_unknown_kind() {
        let t = transcript();
        let mut p = base_plan();
        p.removals[0].evidence_word_ids = vec![4]; // word at 6.0s, removal at 0.6s
        p.removals[0].kind = "boring".into();
        let joined = validate(&p, &t).unwrap_err().join("\n");
        assert!(joined.contains("not near"));
        assert!(joined.contains("unknown kind"));
    }

    #[test]
    fn request_body_has_no_paths_and_uses_schema() {
        let t = transcript();
        let body = build_request(&t, 1920, 1080, 30.0, &PlanSettings::default(), DEFAULT_MODEL);
        let s = body.to_string();
        assert_eq!(body["model"], "claude-opus-5-5");
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(body["output_config"]["effort"], "medium");
        assert_eq!(body["fallbacks"], "default");
        assert!(s.contains("3 3.00 3.50 dali") || s.contains("2 3.00 3.50 Sobrang"));
        assert!(!s.contains('/') || !s.contains(".mp4")); // no file paths
        assert!(!s.to_lowercase().contains("api_key"));
        // Schema is closed and parses into EditPlan.
        assert_eq!(plan_schema()["additionalProperties"], false);
        let _: EditPlan = serde_json::from_value(serde_json::to_value(base_plan()).unwrap()).unwrap();
    }
}
