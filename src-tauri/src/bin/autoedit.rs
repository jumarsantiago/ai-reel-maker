//! Command-line driver for the auto-edit pipeline (local steps only).
//!
//!   autoedit prepare <video> [--lang auto|tl|en] [--root projects] [--model ID] [--min S] [--max S]
//!   autoedit render  <project_dir> <plan.json>

use ai_reel_maker_lib::{autoedit, plan};
use std::path::Path;
use std::process::ExitCode;

fn opt(args: &[String], name: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone())
}

fn main() -> ExitCode {
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
        Some("render") if args.len() >= 4 => autoedit::render_plan(Path::new(&args[2]), Path::new(&args[3]), |f| {
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
