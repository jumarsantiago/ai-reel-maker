//! The two independent engine switches, and a status report for each.
//!
//! * **herk-edit-kit** (HyperFrames and the kit's own tools): ON uses the kit for the operations it supports here
//!   (animated graphics, the edit-review page); OFF uses only the app's own engine. It never needs Claude.
//! * **Claude AI**: ON allows the Claude-assisted operations the user has explicitly set up (their own API key,
//!   shown cost, confirmation); OFF blocks every Claude network call. It never needs the kit.
//!
//! Neither switch changes the other, and nothing here switches an engine silently: an operation that needs a
//! switched-off engine stops with a message that says which switch to turn on.

use crate::motion::Kit;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

static CLAUDE_ON: AtomicBool = AtomicBool::new(false);

pub const CLAUDE_OFF_MSG: &str = "Claude AI is turned off. Turn on \"Use Claude AI\" in Settings > Engines to use Claude-assisted features (they use your own API key and cost money).";
pub const KIT_OFF_MSG: &str = "herk-edit-kit is turned off. Turn on \"Use herk-edit-kit\" in Settings > Engines to use this, or remove it from the edit.";

pub fn set_claude_enabled(on: bool) {
    CLAUDE_ON.store(on, Ordering::SeqCst);
}

pub fn claude_enabled() -> bool {
    CLAUDE_ON.load(Ordering::SeqCst)
}

/// Every Claude network call goes through this first.
pub fn require_claude() -> Result<(), String> {
    if claude_enabled() { Ok(()) } else { Err(CLAUDE_OFF_MSG.into()) }
}

/// `use_kit` of `None` (an older request that does not say) means ON, which is how the app behaved before the
/// switch existed.
pub fn kit_enabled(use_kit: Option<bool>) -> bool {
    use_kit.unwrap_or(true)
}

pub fn require_kit(use_kit: Option<bool>) -> Result<(), String> {
    if kit_enabled(use_kit) { Ok(()) } else { Err(KIT_OFF_MSG.into()) }
}

/// The switches as the UI last saved them (`prefs.json`, `last.useKit` / `last.useClaude`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Switches {
    pub use_kit: bool,
    pub use_claude: bool,
}

impl Default for Switches {
    fn default() -> Self {
        Switches { use_kit: true, use_claude: false }
    }
}

pub fn load_switches(app_dir: &Path) -> Switches {
    let prefs = crate::prefs::load(app_dir);
    let last = &prefs["last"];
    Switches { use_kit: last["useKit"].as_bool().unwrap_or(true), use_claude: last["useClaude"].as_bool().unwrap_or(false) }
}

/// Applies the saved switches (at start-up, and for the command-line tool). A missing file means the defaults:
/// kit ON, Claude OFF.
pub fn apply_saved() {
    if let Ok(dir) = crate::prefs::app_dir() {
        set_claude_enabled(load_switches(&dir).use_claude);
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KitTool {
    pub name: String,
    pub what: String,
    pub present: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KitReport {
    pub enabled: bool,
    pub found: bool,
    pub dir: Option<String>,
    pub node_version: Option<String>,
    pub hyperframes_version: Option<String>,
    pub tools: Vec<KitTool>,
    /// "Ready", "Turned off", or the reason it cannot be used.
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeReport {
    pub enabled: bool,
    /// "env", "keychain" or "none". The key itself is never read into this report.
    pub key_source: String,
    pub calls_made: usize,
    pub spent_usd: f64,
    pub max_calls: u32,
    pub max_usd: f64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    pub kit: KitReport,
    pub claude: ClaudeReport,
    /// Claude Code (the user's own `claude` command and login). Checked only while "Use Claude AI" is on.
    pub claude_code: Option<crate::claude_code::CliStatus>,
}

fn run_version(program: &Path, args: &[&std::ffi::OsStr]) -> Option<String> {
    let out = Command::new(program).args(args).stdin(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).lines().map(str::trim).rfind(|l| !l.is_empty()).map(str::to_string)
}

/// What the kit offers this app, with a real check of each: Node and HyperFrames are actually run.
pub fn kit_report(enabled: bool, kit_dir: Option<&str>) -> KitReport {
    let found = Kit::find(kit_dir);
    match found {
        Err(why) => KitReport { enabled, found: false, dir: None, node_version: None, hyperframes_version: None, tools: Vec::new(), message: if enabled { why } else { format!("Turned off. ({why})") } },
        Ok(kit) => {
            let node_version = run_version(&kit.node, &["--version".as_ref()]);
            let hyperframes_version = run_version(&kit.node, &[kit.cli.as_os_str(), "--version".as_ref()]);
            let tool = |name: &str, what: &str, rel: &str| KitTool { name: name.into(), what: what.into(), present: kit.dir.join(rel).is_file() };
            let tools = vec![
                KitTool { name: "HyperFrames".into(), what: "draws animated graphics".into(), present: hyperframes_version.is_some() },
                tool("build-edl-review", "edit-review page (kept vs cut)", "scripts/build-edl-review.mjs"),
                tool("cut-silences", "silence planner (not used by the app)", ".claude/skills/cut-silences/scripts/cut-silences.mjs"),
                tool("preflight", "composition checks", "scripts/preflight.mjs"),
            ];
            let message = match (enabled, hyperframes_version.is_some()) {
                (false, _) => "Turned off. The app's own engine is used.".to_string(),
                (true, true) => "Ready.".to_string(),
                (true, false) => "The kit was found, but HyperFrames did not run. Reinstall it with `npm ci` in the kit folder.".to_string(),
            };
            KitReport { enabled, found: true, dir: Some(kit.dir.to_string_lossy().into_owned()), node_version, hyperframes_version, tools, message }
        }
    }
}

pub fn claude_report(enabled: bool, ledger_path: &Path, max_calls: u32, max_usd: f64) -> ClaudeReport {
    let key = crate::api::key_status();
    let ledger = crate::api::Ledger::load(ledger_path, max_calls, max_usd);
    let message = match (enabled, key.source.as_str()) {
        (false, _) => "Turned off. No request is sent to Claude.".to_string(),
        (true, "none") => "On, but there is no API key. Add one in Settings.".to_string(),
        (true, _) => "On. Claude features ask you to confirm the cost before they run.".to_string(),
    };
    ClaudeReport { enabled, key_source: key.source, calls_made: ledger.calls.len(), spent_usd: ledger.spent(), max_calls, max_usd, message }
}

/// Both reports. `use_kit` and `use_claude` are the switches as shown in the UI.
pub fn status(use_kit: bool, use_claude: bool, kit_dir: Option<&str>, ledger_path: &Path, max_calls: u32, max_usd: f64) -> EngineStatus {
    EngineStatus {
        kit: kit_report(use_kit, kit_dir),
        claude: claude_report(use_claude, ledger_path, max_calls, max_usd),
        claude_code: use_claude.then(|| crate::claude_code::status(None)),
    }
}

/// Only ever used in tests that change the process-wide Claude switch: they must not run at the same time.
#[cfg(test)]
pub static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineRequest {
    pub use_kit: bool,
    pub use_claude: bool,
    pub kit_dir: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_kit_on_claude_off_and_old_requests_mean_kit_on() {
        assert_eq!(Switches::default(), Switches { use_kit: true, use_claude: false });
        assert!(kit_enabled(None) && kit_enabled(Some(true)) && !kit_enabled(Some(false)));
        assert!(require_kit(Some(false)).unwrap_err().contains("Use herk-edit-kit"));
        assert!(require_kit(None).is_ok());
    }

    #[test]
    fn claude_switch_is_process_wide_and_blocks_by_default() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_claude_enabled(false);
        assert!(require_claude().unwrap_err().contains("Use Claude AI"));
        set_claude_enabled(true);
        assert!(require_claude().is_ok());
        set_claude_enabled(false);
    }

    #[test]
    fn saved_switches_are_read_back_and_damaged_files_mean_defaults() {
        let dir = std::env::temp_dir().join(format!("arm-engine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load_switches(&dir), Switches::default(), "no file");
        crate::prefs::save(&dir, &serde_json::json!({"last": {"useKit": false, "useClaude": true}})).unwrap();
        assert_eq!(load_switches(&dir), Switches { use_kit: false, use_claude: true });
        crate::prefs::save(&dir, &serde_json::json!({"last": {"useKit": "yes", "useClaude": 1}})).unwrap();
        assert_eq!(load_switches(&dir), Switches::default(), "wrong types are ignored");
        std::fs::write(dir.join("prefs.json"), "{ broken").unwrap();
        assert_eq!(load_switches(&dir), Switches::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kit_report_runs_the_real_tools_or_explains_why_not() {
        let missing = kit_report(true, Some("/definitely/not/here"));
        assert!(!missing.found && missing.message.contains("Settings"), "{}", missing.message);
        let off = kit_report(false, Some("/definitely/not/here"));
        assert!(off.message.starts_with("Turned off"), "{}", off.message);
        if let Ok(kit) = Kit::find(None) {
            let r = kit_report(true, Some(&kit.dir.to_string_lossy()));
            assert!(r.found && r.message == "Ready.", "{r:?}");
            assert!(r.node_version.as_deref().is_some_and(|v| v.starts_with('v')), "{r:?}");
            assert!(r.hyperframes_version.is_some(), "HyperFrames was actually run: {r:?}");
            assert!(r.tools.iter().any(|t| t.name == "build-edl-review" && t.present));
            let r = kit_report(false, Some(&kit.dir.to_string_lossy()));
            assert!(r.found && !r.enabled && r.message.starts_with("Turned off"), "{r:?}");
        }
    }
}
