//! The one Claude-assisted operation in the app: asking Claude to plan the edit of a prepared project.
//!
//! Nothing here runs unless "Use Claude AI" is on (every request is refused in `api::post` otherwise), it needs the
//! user's own API key, it shows the worst-case cost first, and it spends nothing until the user confirms that
//! amount. The call is capped by the same ledger as the command-line tool (3 calls, $1 in total). Claude's plan goes
//! through the same validator as the rules-based plan (it cannot invent captions, cut kept speech, or break timing);
//! if it fails validation, the rules-based plan stays and the reasons are shown. The rules-based plan is kept so the
//! change can be undone.

use crate::api;
use crate::engine;
use crate::plan::{self, EditPlan, MAX_TOKENS};
use crate::transcribe::Transcript;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const MAX_PAID_CALLS: u32 = 3;
pub const MAX_USD: f64 = 1.00;

pub fn ledger_path() -> Result<PathBuf, String> {
    Ok(crate::prefs::app_dir()?.join("api_ledger.json"))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Estimate {
    pub model: String,
    pub input_tokens: u64,
    pub worst_case_usd: f64,
    pub spent_usd: f64,
    pub calls_made: usize,
    pub max_calls: u32,
    pub max_usd: f64,
    /// False when the budget would be exceeded; `blocked_reason` says why.
    pub allowed: bool,
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanOutcome {
    /// True when Claude's plan passed validation and replaced the rules-based plan.
    pub applied: bool,
    pub errors: Vec<String>,
    pub cost_usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model_served: String,
    pub segments: usize,
    pub captions: usize,
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("Invalid JSON in {}: {e}", path.display()))
}

/// The exact request that would be sent, with its price. Counting tokens is free but does contact Claude, so it
/// needs the switch on and a key too.
pub fn estimate(dir: &Path, ledger: &Path) -> Result<Estimate, String> {
    engine::require_claude()?;
    let req = read_json(&dir.join("request.json"))?;
    let model = req["model"].as_str().unwrap_or_default().to_string();
    let n = api::count_tokens(&req)?;
    let worst = api::cost_usd(&model, n, MAX_TOKENS as u64).ok_or_else(|| format!("No known price for model {model}; refusing to call."))?;
    let l = api::Ledger::load(ledger, MAX_PAID_CALLS, MAX_USD);
    let blocked = l.check(worst).err();
    Ok(Estimate {
        model,
        input_tokens: n,
        worst_case_usd: worst,
        spent_usd: l.spent(),
        calls_made: l.calls.len(),
        max_calls: MAX_PAID_CALLS,
        max_usd: MAX_USD,
        allowed: blocked.is_none(),
        blocked_reason: blocked,
    })
}

/// Runs the one paid planning call. `confirmed_usd` is the worst-case cost the user saw and agreed to.
pub fn run_plan(dir: &Path, ledger: &Path, confirmed_usd: f64) -> Result<PlanOutcome, String> {
    engine::require_claude()?;
    let est = estimate(dir, ledger)?;
    if let Some(why) = est.blocked_reason {
        return Err(why);
    }
    if !confirmed_usd.is_finite() || est.worst_case_usd > confirmed_usd + 1e-6 {
        return Err(format!("The cost is now up to ${:.4}, more than the ${:.4} you confirmed. Check the estimate and confirm again.", est.worst_case_usd, confirmed_usd.max(0.0)));
    }
    let req = read_json(&dir.join("request.json"))?;
    let mut l = api::Ledger::load(ledger, MAX_PAID_CALLS, MAX_USD);
    let (edit, r) = api::request_plan(&req, dir, ledger, &mut l, est.input_tokens)?;
    let raw = serde_json::to_string_pretty(&edit).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("plan.claude.json"), raw + "\n").map_err(|e| e.to_string())?;
    std::fs::write(dir.join("api_usage.json"), serde_json::to_string_pretty(&r).map_err(|e| e.to_string())? + "\n").map_err(|e| e.to_string())?;
    crate::autoedit::log(dir, &format!("Claude plan: {} in {:.1}s, {} in / {} out tokens, ${:.4}", r.model_served, r.seconds, r.input_tokens, r.output_tokens, r.cost_usd));

    let transcript: Transcript = serde_json::from_value(read_json(&dir.join("transcript.json"))?).map_err(|e| format!("Invalid transcript.json: {e}"))?;
    let mut outcome = PlanOutcome {
        applied: false,
        errors: Vec::new(),
        cost_usd: r.cost_usd,
        input_tokens: r.input_tokens,
        output_tokens: r.output_tokens,
        model_served: r.model_served,
        segments: edit.segments.len(),
        captions: edit.captions.len(),
    };
    match plan::validate(&edit, &transcript) {
        Err(errors) => outcome.errors = errors,
        Ok(_) => {
            let current = dir.join("plan.autocut.json");
            let keep = dir.join("plan.autocut.rules.json");
            if !keep.exists() {
                std::fs::copy(&current, &keep).map_err(|e| format!("Cannot keep the rules-based plan: {e}"))?;
            }
            write_plan(&current, &edit)?;
            // Caption edits are applied on top of the first plan; the plan just changed, so start that fresh.
            let _ = std::fs::remove_file(dir.join("plan.autocut.original.json"));
            outcome.applied = true;
        }
    }
    Ok(outcome)
}

fn write_plan(path: &Path, edit: &EditPlan) -> Result<(), String> {
    std::fs::write(path, serde_json::to_string_pretty(edit).map_err(|e| e.to_string())? + "\n").map_err(|e| format!("Cannot write {}: {e}", path.display()))
}

/// Puts the rules-based plan back. Returns false when there was nothing to undo.
pub fn undo(dir: &Path) -> Result<bool, String> {
    let keep = dir.join("plan.autocut.rules.json");
    if !keep.exists() {
        return Ok(false);
    }
    std::fs::copy(&keep, dir.join("plan.autocut.json")).map_err(|e| format!("Cannot restore the rules-based plan: {e}"))?;
    let _ = std::fs::remove_file(dir.join("plan.autocut.original.json"));
    let _ = std::fs::remove_file(&keep);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{set_claude_enabled, TEST_LOCK};
    use crate::transcribe::{Interval, Word};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A local stand-in for the Claude API: counts the requests it gets and answers with canned JSON.
    struct FakeApi {
        base: String,
        hits: Arc<Mutex<Vec<String>>>,
    }

    fn fake_api(plan_json: String) -> FakeApi {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let hits = Arc::new(Mutex::new(Vec::new()));
        let h = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                // Read the headers, then as much body as Content-Length says.
                loop {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(split) = text.find("\r\n\r\n") {
                        let len = text[..split].lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
                        if buf.len() >= split + 4 + len {
                            break;
                        }
                    }
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                h.lock().unwrap().push(path.clone());
                let body = if path.ends_with("count_tokens") {
                    serde_json::json!({"input_tokens": 1200}).to_string()
                } else {
                    serde_json::json!({"stop_reason": "end_turn", "model": plan::DEFAULT_MODEL, "content": [{"type": "text", "text": plan_json}], "usage": {"input_tokens": 1200, "output_tokens": 400}}).to_string()
                };
                let _ = write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nrequest-id: req_test\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}", body.len(), body);
            }
        });
        FakeApi { base, hits }
    }

    fn project(tag: &str) -> (PathBuf, Transcript) {
        let dir = std::env::temp_dir().join(format!("arm-claude-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let spec = ["Sobrang", "dali", "lang", "nito,", "promise", "talaga", "mga", "ka-reels!"];
        let mut t0 = 0.1;
        let words: Vec<Word> = spec
            .iter()
            .enumerate()
            .map(|(id, text)| {
                let w = Word { id, text: text.to_string(), start: t0, end: t0 + 0.3 };
                t0 += 0.35;
                w
            })
            .collect();
        let t = Transcript {
            source_id: "clip_1".into(),
            duration_sec: t0 + 0.4,
            language_requested: "tl".into(),
            languages_detected: vec![],
            words,
            silences: Vec::<Interval>::new(),
            speech_chunks: vec![],
            whisper_model: "test".into(),
            timing_method: "test".into(),
        };
        std::fs::write(dir.join("transcript.json"), serde_json::to_string(&t).unwrap()).unwrap();
        std::fs::write(dir.join("request.json"), serde_json::json!({"model": plan::DEFAULT_MODEL, "max_tokens": 16000, "messages": []}).to_string()).unwrap();
        std::fs::write(dir.join("plan.autocut.json"), serde_json::to_string(&crate::autoplan::auto_plan(&t)).unwrap()).unwrap();
        (dir, t)
    }

    fn with_api<R>(api: &FakeApi, f: impl FnOnce() -> R) -> R {
        unsafe {
            std::env::set_var("AIRM_API_BASE", &api.base);
            std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test-key-not-real");
        }
        let r = f();
        unsafe {
            std::env::remove_var("AIRM_API_BASE");
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
        r
    }

    #[test]
    fn nothing_reaches_claude_while_the_switch_is_off() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (dir, t) = project("off");
        let api = fake_api(serde_json::to_string(&crate::autoplan::auto_plan(&t)).unwrap());
        set_claude_enabled(false);
        let ledger = dir.join("ledger.json");
        with_api(&api, || {
            assert!(estimate(&dir, &ledger).unwrap_err().contains("Use Claude AI"));
            assert!(run_plan(&dir, &ledger, 5.0).unwrap_err().contains("Use Claude AI"));
            assert!(api::count_tokens(&serde_json::json!({})).unwrap_err().contains("Use Claude AI"), "the low-level call is gated too");
        });
        assert!(api.hits.lock().unwrap().is_empty(), "no request left the app: {:?}", api.hits.lock().unwrap());
        assert!(!ledger.exists(), "no money was recorded");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn estimate_confirm_apply_and_undo() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (dir, t) = project("on");
        let claude_plan = crate::autoplan::auto_plan(&t);
        let api = fake_api(serde_json::to_string(&claude_plan).unwrap());
        set_claude_enabled(true);
        let ledger = dir.join("ledger.json");
        let before = std::fs::read_to_string(dir.join("plan.autocut.json")).unwrap();
        with_api(&api, || {
            let est = estimate(&dir, &ledger).unwrap();
            assert_eq!((est.input_tokens, est.allowed, est.calls_made), (1200, true, 0));
            assert!(est.worst_case_usd > 0.0 && est.worst_case_usd < MAX_USD, "{est:?}");
            assert_eq!(api.hits.lock().unwrap().as_slice(), ["/v1/messages/count_tokens"], "estimating only counts tokens");
            // Confirming less than the worst case is refused, and still sends nothing paid.
            assert!(run_plan(&dir, &ledger, est.worst_case_usd / 2.0).unwrap_err().contains("confirm again"));
            assert!(!api.hits.lock().unwrap().iter().any(|p| p == "/v1/messages"));
            // Confirmed: one paid call, plan validated and applied, rules plan kept, money recorded.
            let out = run_plan(&dir, &ledger, est.worst_case_usd).unwrap();
            assert!(out.applied && out.errors.is_empty(), "{out:?}");
            assert_eq!(api.hits.lock().unwrap().iter().filter(|p| *p == "/v1/messages").count(), 1);
            assert!(dir.join("plan.claude.json").exists() && dir.join("plan.autocut.rules.json").exists());
            let l = api::Ledger::load(&ledger, MAX_PAID_CALLS, MAX_USD);
            assert_eq!(l.calls.len(), 1);
            assert!(l.spent() > 0.0 && l.spent() < est.worst_case_usd);
            // Undo restores the rules-based plan exactly.
            assert!(undo(&dir).unwrap());
            assert_eq!(std::fs::read_to_string(dir.join("plan.autocut.json")).unwrap(), before);
            assert!(!undo(&dir).unwrap(), "nothing left to undo");
        });
        set_claude_enabled(false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_invalid_claude_plan_is_not_applied() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (dir, t) = project("bad");
        let mut bad = crate::autoplan::auto_plan(&t);
        bad.captions[0].text = "Buy now limited offer guaranteed".into(); // invented words
        let api = fake_api(serde_json::to_string(&bad).unwrap());
        set_claude_enabled(true);
        let ledger = dir.join("ledger.json");
        let before = std::fs::read_to_string(dir.join("plan.autocut.json")).unwrap();
        with_api(&api, || {
            let est = estimate(&dir, &ledger).unwrap();
            let out = run_plan(&dir, &ledger, est.worst_case_usd).unwrap();
            assert!(!out.applied && !out.errors.is_empty(), "{out:?}");
            assert!(out.errors.iter().any(|e| e.contains("match what is spoken")), "{:?}", out.errors);
        });
        assert_eq!(std::fs::read_to_string(dir.join("plan.autocut.json")).unwrap(), before, "the rules plan stays");
        assert!(!dir.join("plan.autocut.rules.json").exists());
        set_claude_enabled(false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_budget_blocks_a_call_before_it_is_sent() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (dir, t) = project("budget");
        let api = fake_api(serde_json::to_string(&crate::autoplan::auto_plan(&t)).unwrap());
        set_claude_enabled(true);
        let ledger = dir.join("ledger.json");
        let mut l = api::Ledger::load(&ledger, MAX_PAID_CALLS, MAX_USD);
        for _ in 0..MAX_PAID_CALLS {
            l.calls.push(api::LedgerEntry { unix_time: 0, project: "x".into(), model: plan::DEFAULT_MODEL.into(), request_id: "r".into(), status: 200, input_tokens: 1, output_tokens: 1, cost_usd: 0.01 });
        }
        l.save(&ledger).unwrap();
        with_api(&api, || {
            let est = estimate(&dir, &ledger).unwrap();
            assert!(!est.allowed && est.blocked_reason.as_deref().is_some_and(|r| r.contains("Budget")), "{est:?}");
            assert!(run_plan(&dir, &ledger, 100.0).unwrap_err().contains("Budget"));
        });
        assert!(!api.hits.lock().unwrap().iter().any(|p| p == "/v1/messages"), "no paid request was sent");
        set_claude_enabled(false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// All four combinations of the two switches, with real renders (travel, preview and plain export) and the
    /// real kit where it is installed. Each switch decides only its own features.
    #[test]
    fn the_four_switch_combinations_work_independently() {
        use crate::extras::Extras;
        use crate::motion::{MotionSpec, Template};
        use crate::render::SegmentRequest;
        use crate::travel::{self, TravelStyle};
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Some(ffmpeg) = crate::media::find_tool("ffmpeg") else { return };
        let kit_installed = crate::motion::Kit::find(None).is_ok();
        let dir = std::env::temp_dir().join(format!("arm-matrix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("clip.mp4");
        assert!(std::process::Command::new(&ffmpeg)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25:duration=4", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .unwrap()
            .success());
        let segs = vec![SegmentRequest { source_path: clip.to_string_lossy().into_owned(), in_point: 0.0, out_point: 4.0 }];
        let style = TravelStyle { clip_sec: 0.0, fade: false, ..Default::default() };
        let (dir_t, plan_t) = project("matrix");
        let api = fake_api(serde_json::to_string(&crate::autoplan::auto_plan(&plan_t)).unwrap());
        let ledger = dir_t.join("ledger.json");
        let card = MotionSpec { template: Template::Card, text: "HELLO".into(), at: Some(1.0), dur: Some(2.0), ..Default::default() };
        let mut seen_hits = 0usize;

        for use_kit in [true, false] {
            for use_claude in [true, false] {
                let combo = format!("kit={use_kit} claude={use_claude}");
                set_claude_enabled(use_claude);
                let root = dir.join(format!("root-{use_kit}-{use_claude}"));
                std::fs::create_dir_all(&root).unwrap();

                // 1. Standalone: a plain edit, its preview and a plain export work in every combination.
                let (_, plain) = travel::travel_edit(&segs, &root, &style, None, |_| {}, |_| {}).unwrap_or_else(|e| panic!("{combo}: plain edit: {e}"));
                assert_eq!((plain.width, plain.height), (1080, 1920), "{combo}");
                travel::travel_preview(&segs, &root, &style, None, 3.0, |_| {}).unwrap_or_else(|e| panic!("{combo}: plain preview: {e}"));
                let export = crate::render::ExportRequest {
                    segments: segs.clone(),
                    output_path: root.join("export.mp4").to_string_lossy().into_owned(),
                    overwrite_confirmed: false,
                };
                assert!(crate::render::export(&export, |_| {}).unwrap().duration_sec > 3.5, "{combo}: export");

                // 2. The kit switch decides animated graphics, in the edit and in its preview, and nothing else.
                let extras = Extras { motion: vec![card.clone()], use_kit: Some(use_kit), ..Default::default() };
                let edit = travel::travel_edit(&segs, &root, &style, Some(&extras), |_| {}, |_| {});
                let prev = travel::travel_preview(&segs, &root, &style, Some(&extras), 3.0, |_| {});
                if use_kit {
                    if kit_installed {
                        assert!(edit.is_ok(), "{combo}: graphics render with the kit on: {:?}", edit.err());
                        assert!(prev.is_ok(), "{combo}: graphics preview with the kit on: {:?}", prev.err());
                    }
                } else {
                    let e = edit.unwrap_err();
                    assert!(e.contains("herk-edit-kit") && e.contains("Use herk-edit-kit"), "{combo}: says which switch: {e}");
                    assert!(prev.unwrap_err().contains("herk-edit-kit"), "{combo}: preview is gated too");
                }

                // 3. The Claude switch decides Claude requests, and nothing else.
                with_api(&api, || {
                    let r = estimate(&dir_t, &ledger);
                    if use_claude {
                        assert!(r.is_ok(), "{combo}: estimate works with Claude on: {:?}", r.err());
                    } else {
                        assert!(r.unwrap_err().contains("Use Claude AI"), "{combo}");
                    }
                });
                let hits = api.hits.lock().unwrap().len();
                assert_eq!(hits - seen_hits, usize::from(use_claude), "{combo}: exactly one request when Claude is on, none when off");
                seen_hits = hits;
            }
        }
        set_claude_enabled(false);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir_t);
    }
}
