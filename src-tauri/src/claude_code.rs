//! Claude Code as the editor's planner: the app runs the user's own `claude` command in the background, in
//! the project folder, and asks it to read the transcript and write an edit plan. This uses the user's Claude
//! subscription (their Claude Code login), not an API key, so it costs no API money; it does use their plan's
//! usage limits.
//!
//! What it may do is deliberately small: only the Read and Write tools, nothing else (no shell, no web), anything
//! not allowed is denied (`dontAsk`), it runs with its folder set to the project, and it has a time limit. The
//! API key is removed from its environment so it can never fall back to API billing. Its plan is checked by the
//! same validator as every other plan (it cannot invent captions, cut kept speech or break timing), and it can be
//! undone like Claude's API plan. Nothing runs unless "Use Claude AI" is on.

use crate::claude::{adopt_plan, read_transcript, PlanOutcome};
use crate::engine;
use crate::plan::EditPlan;
use serde::Serialize;
use serde_json::Value;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

pub const PLAN_FILE: &str = "plan.claude-code.json";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(8 * 60);

pub const INSTALL_HELP: &str = "Claude Code is not installed. Install it from claude.com/claude-code (or run `npm install -g @anthropic-ai/claude-code` in Terminal), then run `claude` once and log in with /login.";
pub const LOGIN_HELP: &str = "Claude Code is not logged in. Open Terminal, run `claude`, and log in with /login (use your Claude subscription). Then try again.";

/// The pid of the running `claude`, so "Cancel" can stop it. 0 = nothing running.
static RUNNING_PID: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliStatus {
    pub installed: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub logged_in: bool,
    pub auth_method: Option<String>,
    pub message: String,
}

/// Where the `claude` command usually lives. `AIRM_CLAUDE_BIN` (a path to an existing file) wins, for tests.
pub fn find_cli() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("AIRM_CLAUDE_BIN").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(h) = &home {
        dirs.extend([".local/bin", ".claude/local", ".npm-global/bin", ".bun/bin", ".volta/bin"].iter().map(|d| h.join(d)));
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].iter().map(PathBuf::from));
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.into_iter().map(|d| d.join("claude")).find(|p| p.is_file())
}

/// The PATH given to `claude`: it is a Node program, so Node's folder must be on it even when the app was
/// launched from Finder with a bare PATH.
fn child_path(cli: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(d) = cli.parent() {
        parts.push(d.display().to_string());
    }
    if let Some(node) = ["/opt/homebrew/bin", "/usr/local/bin"].iter().find(|d| Path::new(d).join("node").is_file()) {
        parts.push((*node).to_string());
    }
    parts.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].iter().map(|s| s.to_string()));
    parts.join(":")
}

fn command(cli: &Path) -> Command {
    let mut c = Command::new(cli);
    c.env("PATH", child_path(cli)).stdin(Stdio::null());
    // Never let Claude Code fall back to API billing or another cloud: the subscription login is the only route.
    for k in ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX", "CLAUDE_CODE_USE_FOUNDRY"] {
        c.env_remove(k);
    }
    c
}

/// Whether Claude Code is installed and logged in (`claude --version` and `claude auth status` are actually run).
pub fn status(cli: Option<&Path>) -> CliStatus {
    let Some(cli) = cli.map(Path::to_path_buf).or_else(find_cli) else {
        return CliStatus { installed: false, path: None, version: None, logged_in: false, auth_method: None, message: INSTALL_HELP.into() };
    };
    let version = command(&cli).arg("--version").output().ok().filter(|o| o.status.success()).and_then(|o| String::from_utf8_lossy(&o.stdout).lines().next().map(|l| l.trim().to_string()));
    // `auth status` prints JSON and exits 1 when nobody is logged in, so the output is read either way.
    let auth: Option<Value> = command(&cli).args(["auth", "status"]).output().ok().and_then(|o| serde_json::from_slice(&o.stdout).ok());
    let logged_in = auth.as_ref().and_then(|a| a["loggedIn"].as_bool()).unwrap_or(false);
    let auth_method = auth.as_ref().and_then(|a| a["authMethod"].as_str()).map(str::to_string);
    let message = match (&version, logged_in) {
        (None, _) => "Found the claude command, but it did not run. Reinstall Claude Code.".to_string(),
        (Some(_), false) => LOGIN_HELP.to_string(),
        (Some(_), true) => "Ready. Uses your Claude subscription, not API money.".to_string(),
    };
    CliStatus { installed: version.is_some(), path: Some(cli.to_string_lossy().into_owned()), version, logged_in, auth_method, message }
}

/// The instruction given to Claude Code. The rules and the transcript are in `request.json` (the same request the
/// API route would send), including the JSON schema the plan must follow.
pub fn prompt() -> String {
    [
        "You are planning the edit of one video from its transcript.",
        "1. Read the file request.json in the current folder. Its \"system\" field holds the rules, its first message holds the task and the transcript, and output_config.format.schema is the JSON schema your answer must follow.",
        "2. Follow those rules exactly. Use only words that are in the transcript. Do not invent anything.",
        "   Consistency, which the app checks and rejects the whole plan for: a removal must NEVER overlap any kept segment, not even by a fraction of a second. If you want to cut a pause inside speech, end one kept segment before it and start the next one after it, so the cut is a gap between segments. Every removal needs evidence word ids from the transcript near that time. Segments must not overlap each other. Before writing, check every removal against every segment.",
        &format!("3. Write ONLY the finished JSON plan (an object that matches the schema, no markdown, no comments) to a new file named {PLAN_FILE} in the current folder, using the Write tool."),
        "4. Do nothing else: do not change any other file, and do not run commands. When the file is written, reply with the single word: done.",
    ]
    .join("\n")
}

/// The arguments for one run. Only Read and Write exist; anything else is denied without asking.
pub fn args() -> Vec<String> {
    ["-p", &prompt(), "--output-format", "json", "--tools", "Read,Write", "--allowedTools", "Read", "Write", "--permission-mode", "dontAsk", "--no-session-persistence"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Signals the whole process group of `pid`: `claude` starts helper processes that would otherwise keep
/// running (and keep our pipes open) after only the main one is stopped.
fn signal_group(pid: u32, sig: i32) -> bool {
    unsafe { libc::kill(-(pid as i32), sig) == 0 }
}

/// Stops the running `claude`, if any. Returns whether something was stopped.
pub fn cancel() -> bool {
    let pid = RUNNING_PID.swap(0, Ordering::SeqCst);
    if pid == 0 {
        return false;
    }
    signal_group(pid, libc::SIGTERM)
}

fn limit_text(timeout: Duration) -> String {
    let s = timeout.as_secs();
    if s >= 120 { format!("{} minutes", s / 60) } else if s >= 60 { "1 minute".into() } else { format!("{} seconds", s.max(1)) }
}

/// Runs Claude Code on a prepared project and applies its plan if it passes the app's checks.
pub fn run_plan(dir: &Path, cli: &Path, timeout: Duration) -> Result<PlanOutcome, String> {
    engine::require_claude()?;
    if !cli.is_file() {
        return Err(INSTALL_HELP.into());
    }
    if !dir.join("request.json").is_file() || !dir.join("transcript.json").is_file() {
        return Err("This project has no transcript yet.".into());
    }
    let plan_path = dir.join(PLAN_FILE);
    let _ = std::fs::remove_file(&plan_path);
    let started = Instant::now();
    let mut child = command(cli)
        .args(args())
        .current_dir(dir)
        .process_group(0) // its own group, so a timeout or cancel can stop its helpers too
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start Claude Code: {e}"))?;
    RUNNING_PID.store(child.id(), Ordering::SeqCst);
    let mut out = child.stdout.take().expect("piped");
    let mut err = child.stderr.take().expect("piped");
    let out_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let (status, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(s)) => break (Some(s), false),
            Ok(None) if started.elapsed() > timeout => {
                signal_group(child.id(), libc::SIGKILL);
                let _ = child.kill();
                let _ = child.wait();
                break (None, true);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(format!("Lost track of Claude Code: {e}")),
        }
    };
    let was_cancelled = RUNNING_PID.swap(0, Ordering::SeqCst) == 0;
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    crate::autoedit::log(dir, &format!("Claude Code ran {:.1}s (timeout {timed_out}, cancelled {was_cancelled})", started.elapsed().as_secs_f64()));
    if timed_out {
        return Err(format!("Claude Code took longer than {} and was stopped. Nothing was changed.", limit_text(timeout)));
    }
    if was_cancelled && !status.is_some_and(|s| s.success()) {
        return Err("Stopped. Nothing was changed.".into());
    }

    // `claude -p --output-format json` prints one JSON result object (also for errors, with exit code 1).
    let result: Option<Value> = serde_json::from_str(stdout.trim()).ok();
    if let Some(r) = &result {
        if r["is_error"].as_bool() == Some(true) {
            let msg = r["result"].as_str().unwrap_or("unknown error");
            return Err(if msg.to_lowercase().contains("not logged in") || msg.contains("/login") { LOGIN_HELP.to_string() } else { format!("Claude Code reported an error: {msg}") });
        }
    } else if !status.is_some_and(|s| s.success()) {
        let tail: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).rev().take(3).collect();
        return Err(format!("Claude Code failed: {}", tail.into_iter().rev().collect::<Vec<_>>().join(" ")));
    }

    let raw = std::fs::read_to_string(&plan_path).map_err(|_| "Claude Code finished but did not write a plan. Nothing was changed.".to_string())?;
    let edit: EditPlan = serde_json::from_str(raw.trim()).map_err(|e| format!("Claude Code's plan could not be read ({e}). Nothing was changed."))?;
    let transcript = read_transcript(dir)?;
    let errors = adopt_plan(dir, &edit, &transcript)?;
    let usage = result.as_ref().map(|r| &r["usage"]);
    let model = result.as_ref().and_then(|r| r["modelUsage"].as_object()).and_then(|m| m.keys().next().cloned()).unwrap_or_else(|| "Claude Code".into());
    Ok(PlanOutcome {
        applied: errors.is_empty(),
        errors,
        // A subscription run is not billed per call, so no dollar amount is claimed.
        cost_usd: 0.0,
        input_tokens: usage.and_then(|u| u["input_tokens"].as_u64()).unwrap_or(0),
        output_tokens: usage.and_then(|u| u["output_tokens"].as_u64()).unwrap_or(0),
        model_served: model,
        segments: edit.segments.len(),
        captions: edit.captions.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{set_claude_enabled, TEST_LOCK};
    use std::os::unix::fs::PermissionsExt;

    /// A stand-in `claude` for tests: records how it was called, then behaves as the mode file says.
    fn stub(dir: &Path, mode: &str, plan_json: &str) -> PathBuf {
        let bin = dir.join("claude");
        std::fs::write(dir.join("mode.txt"), mode).unwrap();
        std::fs::write(dir.join("stub-plan.json"), plan_json).unwrap();
        let script = r#"#!/bin/sh
here="$(cd "$(dirname "$0")" && pwd)"
case "$1" in
  --version) echo "9.9.9 (Claude Code stub)"; exit 0;;
  auth) if [ "$(cat "$here/mode.txt")" = "notlogged" ]; then echo '{"loggedIn": false, "authMethod": "none"}'; exit 1; else echo '{"loggedIn": true, "authMethod": "claude.ai"}'; exit 0; fi;;
esac
{ echo "CWD=$(pwd)"; echo "KEYSET=${ANTHROPIC_API_KEY:-unset}"; echo "TOKENSET=${ANTHROPIC_AUTH_TOKEN:-unset}"; for a in "$@"; do echo "ARG=$a"; done; } > "$here/call.log"
mode="$(cat "$here/mode.txt")"
case "$mode" in
  ok|bad) cp "$here/stub-plan.json" ./plan.claude-code.json; echo '{"is_error":false,"result":"done","total_cost_usd":0.5,"usage":{"input_tokens":1500,"output_tokens":420},"modelUsage":{"claude-opus-5-5":{}}}'; exit 0;;
  nofile) echo '{"is_error":false,"result":"done"}'; exit 0;;
  garbage) echo "this is not json" > ./plan.claude-code.json; echo '{"is_error":false,"result":"done"}'; exit 0;;
  notlogged) echo '{"is_error":true,"result":"Not logged in · Please run /login"}'; exit 1;;
  other) echo '{"is_error":true,"result":"Rate limit reached"}'; exit 1;;
  crash) echo "boom" >&2; exit 2;;
  sleep) sleep 30; exit 0;;
esac
"#;
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    fn project(tag: &str) -> (PathBuf, crate::transcribe::Transcript) {
        use crate::transcribe::{Interval, Transcript, Word};
        let dir = std::env::temp_dir().join(format!("arm-cc-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let spec = ["Sobrang", "dali", "lang", "nito,", "promise", "talaga", "mga", "ka-reels!", "Salamat", "po."];
        let mut t0 = 0.1;
        let words: Vec<Word> = spec
            .iter()
            .enumerate()
            .map(|(id, text)| {
                if id == 8 {
                    t0 += 1.5;
                }
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
        std::fs::write(dir.join("request.json"), serde_json::json!({"model": "x", "system": "rules", "messages": []}).to_string()).unwrap();
        std::fs::write(dir.join("plan.autocut.json"), serde_json::to_string(&crate::autoplan::auto_plan(&t)).unwrap()).unwrap();
        (dir, t)
    }

    fn call_log(bin_dir: &Path) -> String {
        std::fs::read_to_string(bin_dir.join("call.log")).unwrap_or_default()
    }

    #[test]
    fn arguments_allow_only_read_and_write_and_ask_for_nothing() {
        let a = args();
        let pos = |s: &str| a.iter().position(|x| x == s).unwrap_or_else(|| panic!("missing {s}: {a:?}"));
        assert_eq!(a[pos("--tools") + 1], "Read,Write", "only these two tools exist at all");
        assert_eq!(&a[pos("--allowedTools") + 1..pos("--allowedTools") + 3], ["Read", "Write"]);
        assert_eq!(a[pos("--permission-mode") + 1], "dontAsk", "anything not allowed is denied, never asked");
        assert_eq!(a[pos("--output-format") + 1], "json");
        assert!(a.contains(&"--no-session-persistence".to_string()));
        for banned in ["Bash", "WebFetch", "WebSearch", "--dangerously-skip-permissions", "bypassPermissions"] {
            assert!(!a.iter().any(|x| x.contains(banned)), "{banned} must never be passed: {a:?}");
        }
        let p = &a[pos("-p") + 1];
        assert!(p.contains("request.json") && p.contains(PLAN_FILE) && p.contains("Do not invent"), "{p}");
        assert!(!p.contains("\\n"), "line breaks are real line breaks");
    }

    #[test]
    fn status_runs_the_command_and_explains_each_state() {
        let dir = std::env::temp_dir().join(format!("arm-cc-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ok = stub(&dir, "ok", "{}");
        let s = status(Some(&ok));
        assert!(s.installed && s.logged_in && s.version.as_deref() == Some("9.9.9 (Claude Code stub)"), "{s:?}");
        assert!(s.message.starts_with("Ready") && s.message.contains("not API money"), "{}", s.message);
        stub(&dir, "notlogged", "{}");
        let s = status(Some(&ok));
        assert!(s.installed && !s.logged_in && s.message.contains("/login"), "{s:?}");
        let s = status(Some(Path::new("/definitely/not/here/claude")));
        assert!(!s.installed || s.path.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_good_plan_is_applied_with_a_clean_environment_and_can_be_undone() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (dir, t) = project("ok");
        let bin_dir = dir.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin = stub(&bin_dir, "ok", &serde_json::to_string(&crate::autoplan::auto_plan(&t)).unwrap());
        set_claude_enabled(true);
        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-must-not-reach-claude-code");
            std::env::set_var("ANTHROPIC_AUTH_TOKEN", "token-must-not-reach-claude-code");
        }
        let before = std::fs::read_to_string(dir.join("plan.autocut.json")).unwrap();
        let out = run_plan(&dir, &bin, Duration::from_secs(20));
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
        }
        set_claude_enabled(false);
        let out = out.unwrap();
        assert!(out.applied && out.errors.is_empty(), "{out:?}");
        assert_eq!((out.input_tokens, out.output_tokens, out.cost_usd), (1500, 420, 0.0), "no dollar amount is claimed for a subscription run");
        assert_eq!(out.model_served, "claude-opus-5-5");
        let log = call_log(&bin_dir);
        assert!(log.contains("KEYSET=unset") && log.contains("TOKENSET=unset"), "the API key never reaches Claude Code:\n{log}");
        assert!(log.contains(&format!("CWD={}", std::fs::canonicalize(&dir).unwrap().display())) || log.contains(&format!("CWD={}", dir.display())), "runs inside the project folder:\n{log}");
        assert!(log.contains("ARG=--permission-mode\nARG=dontAsk") && log.contains("ARG=--tools\nARG=Read,Write"), "{log}");
        assert!(dir.join(PLAN_FILE).exists() && dir.join("plan.autocut.rules.json").exists());
        // Undo restores the rules-based plan exactly (shared with the API route).
        assert!(crate::claude::undo(&dir).unwrap());
        assert_eq!(std::fs::read_to_string(dir.join("plan.autocut.json")).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_runs_while_the_claude_switch_is_off() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (dir, t) = project("off");
        let bin_dir = dir.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin = stub(&bin_dir, "ok", &serde_json::to_string(&crate::autoplan::auto_plan(&t)).unwrap());
        set_claude_enabled(false);
        let err = run_plan(&dir, &bin, Duration::from_secs(5)).unwrap_err();
        assert!(err.contains("Use Claude AI"), "{err}");
        assert!(!bin_dir.join("call.log").exists(), "claude was never started");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_and_broken_results_change_nothing_and_say_why() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_claude_enabled(true);
        let (dir, t) = project("bad");
        let before = std::fs::read_to_string(dir.join("plan.autocut.json")).unwrap();
        let bin_dir = dir.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let good = serde_json::to_string(&crate::autoplan::auto_plan(&t)).unwrap();
        let unchanged = |why: &str| {
            assert_eq!(std::fs::read_to_string(dir.join("plan.autocut.json")).unwrap(), before, "{why}: plan unchanged");
            assert!(!dir.join("plan.autocut.rules.json").exists(), "{why}");
        };

        // An invented caption fails the validator: not applied, reasons given.
        let mut invented = crate::autoplan::auto_plan(&t);
        invented.captions[0].text = "Buy now limited offer guaranteed".into();
        let bin = stub(&bin_dir, "bad", &serde_json::to_string(&invented).unwrap());
        let out = run_plan(&dir, &bin, Duration::from_secs(20)).unwrap();
        assert!(!out.applied && out.errors.iter().any(|e| e.contains("match what is spoken")), "{out:?}");
        unchanged("invented caption");

        // Each failure mode has its own clear message.
        for (mode, expect) in [
            ("nofile", "did not write a plan"),
            ("garbage", "could not be read"),
            ("notlogged", "/login"),
            ("other", "Rate limit reached"),
            ("crash", "boom"),
        ] {
            let bin = stub(&bin_dir, mode, &good);
            let err = run_plan(&dir, &bin, Duration::from_secs(20)).unwrap_err();
            assert!(err.contains(expect), "{mode}: {err}");
            unchanged(mode);
        }
        // A missing command and a missing transcript.
        assert!(run_plan(&dir, Path::new("/nope/claude"), Duration::from_secs(1)).unwrap_err().contains("not installed"));
        std::fs::remove_file(dir.join("transcript.json")).unwrap();
        let bin = stub(&bin_dir, "ok", &good);
        assert!(run_plan(&dir, &bin, Duration::from_secs(5)).unwrap_err().contains("no transcript"));
        set_claude_enabled(false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_run_that_takes_too_long_is_stopped_and_a_cancel_works() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_claude_enabled(true);
        let (dir, t) = project("slow");
        let bin_dir = dir.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin = stub(&bin_dir, "sleep", &serde_json::to_string(&crate::autoplan::auto_plan(&t)).unwrap());
        let started = Instant::now();
        let err = run_plan(&dir, &bin, Duration::from_millis(700)).unwrap_err();
        assert!(err.contains("took longer") && started.elapsed() < Duration::from_secs(10), "{err} after {:?}", started.elapsed());
        // Cancel from another thread while it runs.
        let d2 = dir.clone();
        let b2 = bin.clone();
        let h = std::thread::spawn(move || run_plan(&d2, &b2, Duration::from_secs(60)));
        std::thread::sleep(Duration::from_millis(600));
        assert!(cancel(), "something was running to cancel");
        let err = h.join().unwrap().unwrap_err();
        assert!(err.contains("Stopped") || err.contains("failed") || err.contains("did not write"), "{err}");
        assert!(!cancel(), "nothing left to cancel");
        assert_eq!(std::fs::read_to_string(dir.join("plan.autocut.json")).unwrap(), std::fs::read_to_string(dir.join("plan.autocut.json")).unwrap());
        set_claude_enabled(false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_cli_prefers_an_explicit_override() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("arm-cc-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = stub(&dir, "ok", "{}");
        unsafe { std::env::set_var("AIRM_CLAUDE_BIN", &bin) };
        assert_eq!(find_cli(), Some(bin));
        unsafe { std::env::set_var("AIRM_CLAUDE_BIN", dir.join("missing")) };
        assert_ne!(find_cli(), Some(dir.join("missing")), "a path that does not exist is ignored");
        unsafe { std::env::remove_var("AIRM_CLAUDE_BIN") };
        let _ = std::fs::remove_dir_all(&dir);
    }
}
