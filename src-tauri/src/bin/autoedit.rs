//! Command-line driver for the auto-edit pipeline (local steps only).
//!
//!   autoedit prepare <video> [--lang auto|tl|en] [--root projects] [--model ID] [--min S] [--max S]
//!   autoedit render  <project_dir> <plan.json>
//!   autoedit init | inbox | publish <project_dir> [--final]   (inbox/outbox folder workflow)
//!   autoedit count   <project_dir>                 (free token count; sends request.json)
//!   autoedit plan    <project_dir>                 (ONE paid call, budget-gated)
//!   autoedit autocut <video> [--lang auto]         (free, no AI: rules-based cut + render)
//!   style flags for render/autocut: --plain --no-zoom --no-sfx --no-highlight --no-loudness --music FILE

use ai_reel_maker_lib::{api, autoedit, inbox, plan};
use std::path::Path;
use std::process::ExitCode;

fn opt(args: &[String], name: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone())
}

/// Reel style flags: default is the full Auto Edit look; --plain turns everything off.
fn style_from(args: &[String]) -> autoedit::EditStyle {
    let mut st = if args.iter().any(|a| a == "--plain") { autoedit::EditStyle::plain() } else { autoedit::EditStyle::default() };
    let has = |f: &str| args.iter().any(|a| a == f);
    if has("--no-zoom") { st.zoom = false; }
    if has("--no-sfx") { st.sound_effects = false; }
    if has("--no-highlight") { st.highlight_words = false; }
    if has("--no-loudness") { st.loudness = false; }
    st.music = opt(args, "--music").and_then(|m| std::fs::canonicalize(m).ok()).map(|p| p.to_string_lossy().into_owned());
    st
}

const LEDGER: &str = "projects/api_ledger.json";
const MAX_PAID_CALLS: u32 = 3;
const MAX_USD: f64 = 1.00;

fn read_request(dir: &str) -> Result<serde_json::Value, String> {
    let p = Path::new(dir).join("request.json");
    let t = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    serde_json::from_str(&t).map_err(|e| format!("{}: {e}", p.display()))
}

fn paid_plan(dir: &Path) -> Result<(), String> {
    let req = read_request(&dir.to_string_lossy())?;
    let ledger_path = Path::new(LEDGER);
    let mut ledger = api::Ledger::load(ledger_path, MAX_PAID_CALLS, MAX_USD);
    let n = api::count_tokens(&req)?;
    let model = req["model"].as_str().unwrap_or_default().to_string();
    let worst = api::cost_usd(&model, n, plan::MAX_TOKENS as u64).unwrap_or(f64::INFINITY);
    println!("input tokens {n}; worst case ${worst:.4}; spent so far ${:.4} over {} call(s)", ledger.spent(), ledger.calls.len());
    ledger.check(worst)?;
    println!("calling {model} (one request)...");
    let (edit, r) = api::request_plan(&req, dir, ledger_path, &mut ledger, n)?;
    let raw = serde_json::to_string_pretty(&edit).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("plan.raw.json"), raw + "\n").map_err(|e| e.to_string())?;
    let usage = serde_json::to_string_pretty(&r).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("api_usage.json"), usage + "\n").map_err(|e| e.to_string())?;
    autoedit::log(dir, &format!("API plan call: {} served by {} in {:.1}s, {} in / {} out tokens, ${:.4}, request-id {}",
        r.model_requested, r.model_served, r.seconds, r.input_tokens, r.output_tokens, r.cost_usd, r.request_id));
    println!("served by {} in {:.1}s | stop {} | fallback {}", r.model_served, r.seconds, r.stop_reason, r.fallback_used);
    println!("usage: {} input / {} output tokens -> ${:.4} | total spent ${:.4} over {} call(s)",
        r.input_tokens, r.output_tokens, r.cost_usd, ledger.spent(), ledger.calls.len());
    println!("plan: {} segments, {} removals, {} captions -> plan.raw.json", edit.segments.len(), edit.removals.len(), edit.captions.len());
    Ok(())
}

fn main() -> ExitCode {
    // The command-line tool obeys the same saved "Use Claude AI" switch as the app (OFF unless turned on there).
    ai_reel_maker_lib::engine::apply_saved();
    let args: Vec<String> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("prepare") if args.len() >= 3 => {
            let mut s = plan::PlanSettings::default();
            if let Some(v) = opt(&args, "--min").and_then(|v| v.parse().ok()) {
                s.target_min_sec = v;
            }
            if let Some(v) = opt(&args, "--max").and_then(|v| v.parse().ok()) {
                s.target_max_sec = v;
            }
            let lang = opt(&args, "--lang").unwrap_or_else(|| "auto".into());
            let root = opt(&args, "--root").unwrap_or_else(|| "projects".into());
            let model = opt(&args, "--model").unwrap_or_else(|| plan::DEFAULT_MODEL.into());
            let video = std::fs::canonicalize(&args[2]).map_err(|e| format!("{}: {e}", args[2]));
            let root_dir = std::fs::create_dir_all(&root).and_then(|_| std::fs::canonicalize(&root)).map_err(|e| format!("{root}: {e}"));
            video.and_then(|v| root_dir.map(|r| (v, r))).and_then(|(v, r)| autoedit::prepare(&v.to_string_lossy(), &r, &lang, &s, &model)).map(|dir| {
                println!("project: {}", dir.display());
                println!("request.json written for review - nothing was sent.");
            })
        }
        Some("render") if args.len() >= 4 => autoedit::render_plan(Path::new(&args[2]), Path::new(&args[3]), &style_from(&args), |f| {
            eprint!("\rrendering {:>3.0}%", f * 100.0);
        })
        .map(|r| {
            eprintln!();
            println!("output: {} ({:.2}s, {}x{}, audio {})", r.output.output_path, r.output.duration_sec, r.output.width, r.output.height, r.output.has_audio);
            println!("srt: {}", r.srt);
            println!("segments {} | captions {} | planned {:.2}s", r.segments, r.captions, r.planned_duration_sec);
            println!("kept-words match {:.0}% | fillers in output: {:?}", r.kept_words_match * 100.0, r.filler_words_in_output);
            for w in &r.warnings {
                println!("warning: {w}");
            }
        }),
        Some("init") => inbox::base_dir().and_then(|b| inbox::init(&b).map(|c| (b, c))).map(|(b, c)| {
            println!("folder: {}", b.display());
            println!("{}", if c.is_empty() { "already set up (nothing overwritten)".to_string() } else { format!("created: {}", c.join(", ")) });
        }),
        Some("inbox") => inbox::base_dir().and_then(|b| {
            let items = inbox::list_inbox(&b.join("inbox"), &b.join("outbox"))?;
            if items.is_empty() {
                println!("inbox is empty: {}", b.join("inbox").display());
            }
            for i in &items {
                println!("{}\t{}\t{}", i.status(), i.file, i.path);
            }
            Ok(())
        }),
        Some("publish") if args.len() >= 3 => inbox::base_dir().and_then(|b| {
            let dir = std::fs::canonicalize(&args[2]).map_err(|e| format!("{}: {e}", args[2]))?;
            let out = inbox::publish(&dir, &b.join("outbox"), args.iter().any(|a| a == "--final"))?;
            println!("published: {}", out.display());
            Ok(())
        }),
        Some("count") if args.len() >= 3 => read_request(&args[2]).and_then(|req| api::count_tokens(&req)).map(|n| {
            println!("input tokens (count_tokens, free): {n}");
            if let Some((i, o)) = api::price_per_mtok(plan::DEFAULT_MODEL) {
                println!("input cost at ${i}/MTok: ${:.4} | worst case with {} output tokens at ${o}/MTok: ${:.4}",
                    n as f64 * i / 1e6, plan::MAX_TOKENS, api::cost_usd(plan::DEFAULT_MODEL, n, plan::MAX_TOKENS as u64).unwrap_or(0.0));
            }
        }),
        Some("plan") if args.len() >= 3 => paid_plan(Path::new(&args[2])),
        Some("autocut") if args.len() >= 3 => {
            let lang = opt(&args, "--lang").unwrap_or_else(|| "auto".into());
            let root = opt(&args, "--root").unwrap_or_else(|| "projects".into());
            std::fs::canonicalize(&args[2])
                .map_err(|e| format!("{}: {e}", args[2]))
                .and_then(|v| {
                    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
                    let r = std::fs::canonicalize(&root).map_err(|e| e.to_string())?;
                    autoedit::auto_cut(&v.to_string_lossy(), &r, &lang, &style_from(&args), |s| eprintln!("{s}"), |f| eprint!("\rrendering {:>3.0}%", f * 100.0))
                })
                .map(|(dir, r)| {
                    eprintln!();
                    println!("project: {}", dir.display());
                    println!("output: {} ({:.2}s, {}x{})", r.output.output_path, r.output.duration_sec, r.output.width, r.output.height);
                    println!("segments {} | captions {} | kept-words match {:.0}% | fillers in output: {:?}", r.segments, r.captions, r.kept_words_match * 100.0, r.filler_words_in_output);
                })
        }
        _ => Err("usage:\n  autoedit prepare <video> [--lang auto] [--root projects] [--model ID] [--min S] [--max S]\n  autoedit render <project_dir> <plan.json>".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
