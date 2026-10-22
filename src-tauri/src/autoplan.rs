//! "Auto-cut": a free, offline, rules-based edit plan - no AI. It produces the same EditPlan the
//! Claude planner would, so it goes through the same validator, renderer and checks.
//! Rules (all driven by the local transcript):
//!   - dead air: pauses longer than MAX_GAP_SEC between spoken words are cut
//!   - fillers: um/uh/ah/er/hmm are cut (Tagalog "ano"/"parang" are left alone - too often real words)
//!   - repeats and false starts: an immediately repeated word ("the the") or a short phrase that is
//!     restarted soon after ("how to make, how to make"; "strong coffee, strong cough, strong coffee")
//!     keeps only the last attempt
//!   - captions: kept words grouped into short lines, breaking at punctuation

use crate::plan::{normalize, Confidence, EditPlan, PlanCaption, PlanRemoval, PlanSegment, PlanSummary};
use crate::transcribe::{Transcript, Word};

pub const MAX_GAP_SEC: f64 = 0.4;
/// Must match the validator's minimum segment length (plan.rs), with a hair of margin for rounding.
const MIN_SEGMENT_SEC: f64 = 0.32;
const PAD_SEC: f64 = 0.08;
const MAX_PHRASE: usize = 4;
/// How many words may sit between a phrase and its restart for it to count as a false start.
const RESTART_WINDOW: usize = 3;
const CAPTION_MAX_WORDS: usize = 3;
const CAPTION_MAX_CHARS: usize = 20;
pub const FILLERS: &[&str] = &["um", "uh", "uhm", "umm", "ah", "er", "erm", "hmm", "mm"];

#[derive(Debug, Clone, Copy, PartialEq)]
enum Why {
    Keep,
    Filler,
    Repeat,
}

/// Marks each word keep / filler / repeat.
fn classify(words: &[Word]) -> Vec<Why> {
    let norm: Vec<String> = words.iter().map(|w| normalize(&w.text)).collect();
    let mut why: Vec<Why> = norm.iter().map(|n| if FILLERS.contains(&n.as_str()) { Why::Filler } else { Why::Keep }).collect();
    // Indices of words that still count as speech, so fillers between attempts don't hide a restart.
    let speech: Vec<usize> = (0..words.len()).filter(|&i| why[i] == Why::Keep && !norm[i].is_empty()).collect();
    let mut k = 0;
    while k < speech.len() {
        let mut cut_until = None;
        // Prefer the longest phrase whose restart begins soonest.
        'search: for n in (1..=MAX_PHRASE).rev() {
            if k + 2 * n > speech.len() {
                continue;
            }
            let max_gap = if n == 1 { 0 } else { RESTART_WINDOW };
            for gap in 0..=max_gap {
                let j = k + n + gap;
                if j + n > speech.len() {
                    break;
                }
                let same = (0..n).all(|t| norm[speech[k + t]] == norm[speech[j + t]]);
                // Single repeated words need at least 2 letters ("I I" is real stutter, "a a" too noisy).
                if same && (n > 1 || norm[speech[k]].len() >= 2) {
                    cut_until = Some(j);
                    break 'search;
                }
            }
        }
        match cut_until {
            Some(j) => {
                for &i in &speech[k..j] {
                    why[i] = Why::Repeat;
                }
                k = j;
            }
            None => k += 1,
        }
    }
    why
}

pub fn auto_plan(t: &Transcript) -> EditPlan {
    let w = &t.words;
    let mut why = classify(w);
    let dur = t.duration_sec;

    // Group kept words into segments, splitting where a removed word or a long pause sits between.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for i in 0..w.len() {
        if why[i] != Why::Keep {
            continue;
        }
        let continues = groups.last().is_some_and(|g| {
            let last = *g.last().expect("non-empty");
            (last + 1..i).all(|m| why[m] == Why::Keep) && w[i].start - w[last].end <= MAX_GAP_SEC
        });
        if continues {
            groups.last_mut().expect("non-empty").push(i);
        } else {
            groups.push(vec![i]);
        }
    }

    // Segment edges: pad into the silence, but never past halfway to a neighbouring word. A group that is
    // still shorter than the validator's minimum segment is widened into the silence around it; if there is
    // no room (a stray "uh-uh" squeezed between words), it is dropped and its words count as removed, so the
    // plan always validates and caption segment indexes stay aligned.
    let mut segments = Vec::new();
    let mut bounds: Vec<(f64, f64)> = Vec::new();
    let mut kept_groups: Vec<Vec<usize>> = Vec::new();
    for g in groups {
        let (a, b) = (g[0], *g.last().expect("non-empty"));
        let prev_end = if a > 0 { w[a - 1].end } else { 0.0 };
        let next_start = if b + 1 < w.len() { w[b + 1].start } else { dur };
        let lo = ((prev_end + w[a].start) / 2.0).max(0.0);
        let hi = ((w[b].end + next_start) / 2.0).min(dur);
        let mut start = (w[a].start - PAD_SEC).max(lo);
        let mut end = (w[b].end + PAD_SEC).min(hi);
        if end - start < MIN_SEGMENT_SEC {
            let grow = (MIN_SEGMENT_SEC - (end - start)) / 2.0;
            start = (start - grow).max(lo);
            end = (end + grow).min(hi);
            // One side may have hit its limit; give the leftover to the other.
            if end - start < MIN_SEGMENT_SEC {
                start = (end - MIN_SEGMENT_SEC).max(lo);
                end = (start + MIN_SEGMENT_SEC).min(hi);
            }
        }
        if round2(end) - round2(start) < MIN_SEGMENT_SEC {
            for &i in &g {
                why[i] = Why::Filler;
            }
            continue;
        }
        let text: Vec<&str> = g.iter().map(|&i| w[i].text.as_str()).collect();
        segments.push(PlanSegment {
            source_id: t.source_id.clone(),
            start: round2(start),
            end: round2(end),
            purpose: "speech".into(),
            rationale: format!("Kept: \"{}\"", text.join(" ")),
            confidence: Confidence::High,
        });
        bounds.push((round2(start), round2(end)));
        kept_groups.push(g);
    }
    let groups = kept_groups;

    // Removals = everything between kept segments that contains removed words or a long pause.
    let mut removals = Vec::new();
    let mut edges = vec![(0.0, 0.0)];
    edges.extend(bounds.iter().copied());
    edges.push((dur, dur));
    for k in 0..edges.len() - 1 {
        let (gap_start, gap_end) = (edges[k].1, edges[k + 1].0);
        if gap_end - gap_start < 0.05 {
            continue;
        }
        let inside: Vec<usize> = (0..w.len()).filter(|&i| why[i] != Why::Keep && w[i].start >= gap_start - 0.05 && w[i].end <= gap_end + 0.05).collect();
        if inside.is_empty() {
            // Pure pause: cite the words on either side (if any).
            let before = w.iter().rposition(|x| x.end <= gap_start + 0.15);
            let after = w.iter().position(|x| x.start >= gap_end - 0.15);
            let ev: Vec<usize> = before.into_iter().chain(after).collect();
            if !ev.is_empty() && gap_end - gap_start >= MAX_GAP_SEC {
                removals.push(PlanRemoval {
                    start: round2(gap_start),
                    end: round2(gap_end),
                    kind: "dead_air".into(),
                    evidence_word_ids: ev.iter().map(|&i| w[i].id as i64).collect(),
                    rationale: format!("Pause of {:.1}s", gap_end - gap_start),
                    confidence: Confidence::High,
                });
            }
            continue;
        }
        // Split the gap into runs of the same kind (filler vs repeat), cutting between runs halfway.
        let mut runs: Vec<(Why, Vec<usize>)> = Vec::new();
        for &i in &inside {
            match runs.last_mut() {
                Some((k, v)) if *k == why[i] => v.push(i),
                _ => runs.push((why[i], vec![i])),
            }
        }
        let mut run_start = gap_start;
        for (r, (kind, ids)) in runs.iter().enumerate() {
            let run_end = match runs.get(r + 1) {
                Some((_, next)) => (w[*ids.last().expect("non-empty")].end + w[next[0]].start) / 2.0,
                None => gap_end,
            };
            let txt: Vec<&str> = ids.iter().map(|&i| w[i].text.as_str()).collect();
            let (k, rationale, c) = match kind {
                Why::Repeat => ("repeat", format!("Repeated / restarted: \"{}\"", txt.join(" ")), Confidence::Medium),
                _ => ("filler", format!("Filler: \"{}\"", txt.join(" ")), Confidence::High),
            };
            removals.push(PlanRemoval {
                start: round2(run_start),
                end: round2(run_end),
                kind: k.into(),
                evidence_word_ids: ids.iter().map(|&i| w[i].id as i64).collect(),
                rationale,
                confidence: c,
            });
            run_start = run_end;
        }
    }

    // Captions: short lines inside each segment, breaking after punctuation.
    let mut captions = Vec::new();
    for (si, g) in groups.iter().enumerate() {
        let (seg_start, seg_end) = bounds[si];
        let mut line: Vec<usize> = Vec::new();
        let flush = |line: &mut Vec<usize>, captions: &mut Vec<PlanCaption>, next: Option<usize>| {
            if line.is_empty() {
                return;
            }
            let first = line[0];
            let last = *line.last().expect("non-empty");
            let start = if captions.last().is_some_and(|c: &PlanCaption| c.segment_index == si as i64) { w[first].start } else { seg_start };
            let end = next.map(|n| w[n].start).unwrap_or(seg_end).max(w[last].end);
            let text: Vec<&str> = line.iter().map(|&i| w[i].text.as_str()).collect();
            captions.push(PlanCaption { segment_index: si as i64, start: round2(start.max(seg_start)), end: round2(end.min(seg_end)), text: text.join(" ") });
            line.clear();
        };
        for (pos, &i) in g.iter().enumerate() {
            line.push(i);
            let chars: usize = line.iter().map(|&x| w[x].text.len() + 1).sum();
            let ends_clause = w[i].text.ends_with([',', '.', '!', '?', ';', ':']);
            let next = g.get(pos + 1).copied();
            let next_len = next.map(|n| w[n].text.len() + 1).unwrap_or(0);
            if ends_clause || line.len() >= CAPTION_MAX_WORDS || chars + next_len > CAPTION_MAX_CHARS || next.is_none() {
                flush(&mut line, &mut captions, next);
            }
        }
    }

    let captions = merge_short_captions(captions);

    let kept: f64 = bounds.iter().map(|(a, b)| b - a).sum();
    EditPlan {
        summary: PlanSummary {
            title: "Auto-cut (rules, no AI)".into(),
            overall_rationale: format!(
                "Removed pauses over {MAX_GAP_SEC}s, fillers and repeated/restarted phrases; kept {:.1}s of {:.1}s in original order.",
                kept, dur
            ),
            overall_confidence: Confidence::Medium,
        },
        segments,
        removals,
        captions,
    }
}

/// Captions shorter than this flash by too fast to read; they join a neighbour in the same segment.
const MIN_CAPTION_READ_SEC: f64 = 0.35;

fn merge_short_captions(caps: Vec<PlanCaption>) -> Vec<PlanCaption> {
    let mut out: Vec<PlanCaption> = Vec::new();
    let mut pending: Option<PlanCaption> = None; // a short first caption waiting to join the next one
    for mut c in caps {
        if let Some(p) = pending.take() {
            if p.segment_index == c.segment_index {
                c.text = format!("{} {}", p.text, c.text);
                c.start = p.start;
            } else {
                out.push(p);
            }
        }
        if c.end - c.start >= MIN_CAPTION_READ_SEC {
            out.push(c);
            continue;
        }
        match out.last_mut() {
            Some(prev) if prev.segment_index == c.segment_index => {
                prev.text = format!("{} {}", prev.text, c.text);
                prev.end = c.end;
            }
            _ => pending = Some(c),
        }
    }
    out.extend(pending);
    out
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::validate;
    use crate::transcribe::Interval;

    /// Builds words laid out one after another (0.3s each, 0.05s apart) with optional extra pauses.
    fn transcript(spec: &[(&str, f64)]) -> Transcript {
        let mut t = 0.1;
        let words = spec
            .iter()
            .enumerate()
            .map(|(id, (text, pause_before))| {
                t += pause_before;
                let w = Word { id, text: text.to_string(), start: t, end: t + 0.3 };
                t += 0.35;
                w
            })
            .collect::<Vec<_>>();
        Transcript {
            source_id: "clip_1".into(),
            duration_sec: t + 0.5,
            language_requested: "auto".into(),
            languages_detected: vec!["en".into()],
            words,
            silences: Vec::<Interval>::new(),
            speech_chunks: vec![],
            whisper_model: "test".into(),
            timing_method: "test".into(),
        }
    }

    fn kept_text(t: &Transcript, p: &EditPlan) -> String {
        let v = validate(p, t).expect("auto plan must validate");
        v.expected_words.join(" ")
    }

    #[test]
    fn removes_fillers_repeats_false_starts_and_pauses() {
        let t = transcript(&[
            ("Today", 0.0), ("I'm", 0.0), ("going", 0.0), ("to", 0.0), ("show", 0.0), ("you,", 0.0),
            ("um,", 0.0), ("how", 0.0), ("to", 0.0), ("make,", 0.0), ("how", 0.0), ("to", 0.0), ("make", 0.0),
            ("iced", 0.0), ("coffee.", 0.0),
            ("First,", 1.5), ("strong", 0.0), ("coffee,", 0.0), ("strong", 0.0), ("cough.", 0.0),
            ("strong", 0.3), ("coffee,", 0.0), ("and", 0.0), ("ice.", 0.0),
        ]);
        let p = auto_plan(&t);
        assert_eq!(kept_text(&t, &p), "today im going to show you how to make iced coffee first strong coffee and ice");
        let kinds: Vec<&str> = p.removals.iter().map(|r| r.kind.as_str()).collect();
        assert!(kinds.contains(&"filler") && kinds.contains(&"repeat") && kinds.contains(&"dead_air"), "{kinds:?}");
    }

    #[test]
    fn keeps_normal_speech_and_short_words_alone() {
        let t = transcript(&[("Sobrang", 0.0), ("dali", 0.0), ("lang", 0.0), ("nito,", 0.0), ("ano", 0.0), ("ba,", 0.0), ("a", 0.0), ("a", 0.0), ("promise!", 0.0)]);
        let p = auto_plan(&t);
        assert_eq!(p.segments.len(), 1);
        assert_eq!(kept_text(&t, &p), "sobrang dali lang nito ano ba a a promise");
        // Only the trailing silence may be cut - never words.
        assert!(p.removals.iter().all(|r| r.kind == "dead_air"), "{:?}", p.removals);
    }

    #[test]
    fn lone_tiny_word_between_pauses_is_widened_and_the_plan_validates() {
        // A burst of speech shorter than the minimum segment ("Uh-uh") used to be rejected by validation,
        // which shifted every later caption onto the wrong segment.
        let mut t = transcript(&[("Hello", 0.0), ("there.", 0.0), ("Uh-uh.", 1.5), ("Okay", 1.5), ("so", 0.0), ("yes.", 0.0), ("Hmm-mm.", 1.5), ("Bye", 1.5), ("now.", 0.0)]);
        t.words[2].end = t.words[2].start + 0.05;
        t.words[6].end = t.words[6].start + 0.1;
        let p = auto_plan(&t);
        let v = validate(&p, &t).unwrap_or_else(|e| panic!("plan rejected: {e:?}"));
        assert_eq!(v.expected_words.join(" "), "hello there uhuh okay so yes hmmmm bye now");
        assert_eq!(p.captions.len(), v.captions.len());
    }

    #[test]
    fn tiny_word_squeezed_between_fillers_is_dropped_and_the_plan_validates() {
        let mut t = transcript(&[("Hello", 0.0), ("there.", 0.0), ("um,", 0.0), ("yes.", 0.0), ("um,", 0.0), ("Okay", 0.0), ("so", 0.0), ("yes.", 0.0)]);
        t.words[3].end = t.words[3].start + 0.05;
        let p = auto_plan(&t);
        let v = validate(&p, &t).unwrap_or_else(|e| panic!("plan rejected: {e:?}"));
        assert_eq!(v.expected_words.join(" "), "hello there okay so yes");
        assert_eq!(p.captions.len(), v.captions.len());
        assert!(p.segments.iter().all(|s| s.end - s.start >= 0.3));
    }

    #[test]
    fn random_transcripts_always_produce_a_valid_plan() {
        // Tiny words, fillers, repeats and pauses in every combination must never make validation reject the plan.
        let mut seed = 0x2545F4914F6CDD1Du64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let pool = ["yes.", "um,", "Uh-uh.", "the", "the", "ano", "kasi", "hmm", "okay", "so", "bye", "how", "to", "make,", "ito"];
        for case in 0..300 {
            let n = 5 + (rnd() * 60.0) as usize;
            let mut clock = 0.0;
            let mut words = Vec::new();
            for id in 0..n {
                clock += if rnd() < 0.3 { rnd() * 2.0 } else { rnd() * 0.12 };
                let len = if rnd() < 0.3 { 0.03 + rnd() * 0.2 } else { 0.15 + rnd() * 0.6 };
                let text = pool[(rnd() * pool.len() as f64) as usize % pool.len()].to_string();
                words.push(Word { id, text, start: clock, end: clock + len });
                clock += len;
            }
            let t = Transcript {
                source_id: "clip_1".into(),
                duration_sec: clock + 0.3,
                language_requested: "tl".into(),
                languages_detected: vec![],
                words,
                silences: Vec::<Interval>::new(),
                speech_chunks: vec![],
                whisper_model: "test".into(),
                timing_method: "test".into(),
            };
            let p = auto_plan(&t);
            if p.segments.is_empty() {
                continue;
            }
            match validate(&p, &t) {
                Ok(v) => assert_eq!(v.captions.len(), p.captions.len(), "case {case}: captions dropped"),
                // A reel that is only a fraction of a second long is rightly refused; that is not a plan bug.
                Err(e) if e.len() == 1 && e[0].contains("would be only") => {}
                Err(e) => panic!("case {case}: plan rejected: {:?}", &e[..e.len().min(4)]),
            }
        }
    }

    #[test]
    fn stutter_single_word_is_cut() {
        let t = transcript(&[("the", 0.0), ("the", 0.0), ("best", 0.0), ("coffee", 0.0)]);
        assert_eq!(kept_text(&t, &auto_plan(&t)), "the best coffee");
    }

    #[test]
    fn short_captions_merge_into_neighbours() {
        let c = |seg: i64, start: f64, end: f64, text: &str| PlanCaption { segment_index: seg, start, end, text: text.into() };
        let merged = merge_short_captions(vec![
            c(0, 0.0, 1.0, "and lots of"), c(0, 1.0, 1.1, "ice."),   // short tail joins previous
            c(1, 2.0, 2.1, "Oh"), c(1, 2.1, 3.0, "hello there"),       // short head joins next
            c(2, 4.0, 4.2, "Hi"),                                      // alone in its segment: kept
        ]);
        let texts: Vec<&str> = merged.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(texts, ["and lots of ice.", "Oh hello there", "Hi"]);
        assert_eq!((merged[0].start, merged[0].end), (0.0, 1.1));
        assert_eq!((merged[1].start, merged[1].end), (2.0, 3.0));
    }

    #[test]
    fn captions_are_short_and_inside_segments() {
        let t = transcript(&[("Pour", 0.0), ("the", 0.0), ("milk,", 0.0), ("add", 0.0), ("the", 0.0), ("syrup,", 0.0), ("and", 0.0), ("stir", 0.0), ("it", 0.0), ("very", 0.0), ("slowly.", 0.0)]);
        let p = auto_plan(&t);
        let texts: Vec<&str> = p.captions.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["Pour the milk,", "add the syrup,", "and stir it", "very slowly."]);
        let v = validate(&p, &t).unwrap();
        assert_eq!(v.captions.len(), 4);
    }
}
