//! Minimal Anthropic Messages API client over HTTPS (Rust has no official SDK).
//! - The API key comes from ANTHROPIC_API_KEY or the macOS Keychain item "ANTHROPIC_API_KEY",
//!   is read only at call time, and is never logged, saved, or sent anywhere but api.anthropic.com.
//! - Paid calls are gated by a persistent ledger (call count + worst-case dollars) that refuses
//!   any call that could exceed the owner-approved budget.

use crate::plan::{EditPlan, FALLBACK_BETA, MAX_TOKENS};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const API_BASE: &str = "https://api.anthropic.com";

/// The API address. Only a loopback address in `AIRM_API_BASE` (a local test server) can replace the real one,
/// so the API key can never be redirected to another machine.
fn api_base() -> String {
    let Ok(b) = std::env::var("AIRM_API_BASE") else { return API_BASE.to_string() };
    // Exactly "http://127.0.0.1:<digits>" or "http://localhost:<digits>", with at most a trailing slash.
    // Anything else (a path, "user@host" tricks, another host) is ignored.
    let rest = b.strip_prefix("http://127.0.0.1:").or_else(|| b.strip_prefix("http://localhost:"));
    match rest.map(|r| r.strip_suffix('/').unwrap_or(r)) {
        Some(port) if !port.is_empty() && port.len() <= 5 && port.bytes().all(|c| c.is_ascii_digit()) => b.trim_end_matches('/').to_string(),
        _ => API_BASE.to_string(),
    }
}
const API_VERSION: &str = "2023-06-01";

/// USD per million tokens (input, output), standard first-party pricing checked 2026-10-22.
pub fn price_per_mtok(model: &str) -> Option<(f64, f64)> {
    match model {
        "claude-opus-5-5" => Some((4.0, 20.0)),
        "claude-sonnet-5-5" => Some((2.0, 10.0)),
        "claude-haiku-5-5" => Some((0.10, 0.50)),
        "claude-fable-5-1" => Some((10.0, 50.0)),
        "claude-opus-5" | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6" => Some((5.0, 25.0)),
        _ => None,
    }
}

pub fn cost_usd(model: &str, input_tokens: u64, output_tokens: u64) -> Option<f64> {
    price_per_mtok(model).map(|(i, o)| (input_tokens as f64 * i + output_tokens as f64 * o) / 1e6)
}

pub const KEYCHAIN_SERVICE: &str = "ANTHROPIC_API_KEY";

/// macOS short user name (the Keychain account). Apps launched from Finder may lack $USER.
fn current_user() -> Result<String, String> {
    let name = match std::env::var("USER") {
        Ok(u) if !u.is_empty() => u,
        _ => {
            let out = Command::new("/usr/bin/id").arg("-un").output().map_err(|e| format!("Cannot determine user: {e}"))?;
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
    };
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
        return Err("Unexpected macOS user name.".into());
    }
    Ok(name)
}

/// Accepts only the documented key shape so it can be passed to `security` safely.
pub fn validate_key_format(raw: &str) -> Result<String, String> {
    let k = raw.trim();
    if !k.starts_with("sk-ant-") {
        return Err("That doesn't look like an Anthropic API key (it should start with \"sk-ant-\").".into());
    }
    if !(20..=300).contains(&k.len()) || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("The key contains unexpected characters or has an unexpected length.".into());
    }
    Ok(k.to_string())
}

fn keychain_has_key(user: &str) -> bool {
    Command::new("/usr/bin/security")
        .args(["find-generic-password", "-a", user, "-s", KEYCHAIN_SERVICE])
        .output()
        .is_ok_and(|o| o.status.success())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    /// "env", "keychain" or "none". Never includes the key itself.
    pub source: String,
    pub account: String,
}

pub fn key_status() -> KeyStatus {
    let account = current_user().unwrap_or_default();
    let source = if std::env::var("ANTHROPIC_API_KEY").is_ok_and(|k| !k.trim().is_empty()) {
        "env"
    } else if !account.is_empty() && keychain_has_key(&account) {
        "keychain"
    } else {
        "none"
    };
    KeyStatus { source: source.into(), account }
}

/// Saves (or replaces) the key in the login keychain. The key goes to `security` on stdin,
/// never on its command line, so it doesn't appear in process listings.
pub fn save_key(raw: &str) -> Result<KeyStatus, String> {
    use std::io::Write as _;
    let key = validate_key_format(raw)?;
    let user = current_user()?;
    let mut child = Command::new("/usr/bin/security")
        .arg("-i")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start the Keychain tool: {e}"))?;
    {
        let stdin = child.stdin.as_mut().ok_or("Keychain tool has no input.")?;
        // Both values were validated to contain no spaces or quotes.
        writeln!(stdin, "add-generic-password -U -a {user} -s {KEYCHAIN_SERVICE} -l {KEYCHAIN_SERVICE} -w {key}")
            .map_err(|e| format!("Could not talk to the Keychain tool: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("Keychain tool failed: {e}"))?;
    if !keychain_has_key(&user) {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("The Keychain did not save the key. {}", err.trim()));
    }
    Ok(key_status())
}

pub fn remove_key() -> Result<KeyStatus, String> {
    let user = current_user()?;
    if keychain_has_key(&user) {
        let out = Command::new("/usr/bin/security")
            .args(["delete-generic-password", "-a", &user, "-s", KEYCHAIN_SERVICE])
            .output()
            .map_err(|e| format!("Could not run the Keychain tool: {e}"))?;
        if !out.status.success() {
            return Err("The Keychain refused to remove the key.".into());
        }
    }
    Ok(key_status())
}

/// Resolves the API key without printing it. Environment first, then Keychain.
pub fn api_key() -> Result<String, String> {
    // With "Use Claude AI" off the key is not even looked up (no environment read, no Keychain query).
    crate::engine::require_claude()?;
    if let Ok(k) = std::env::var("ANTHROPIC_API_KEY") {
        if !k.trim().is_empty() {
            return Ok(k.trim().to_string());
        }
    }
    let user = current_user()?;
    let out = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-a", &user, "-s", KEYCHAIN_SERVICE, "-w"])
        .output()
        .map_err(|e| format!("Could not query the Keychain: {e}"))?;
    let key = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || key.is_empty() {
        return Err("No API key found. Add it in Settings (saved to the macOS Keychain).".into());
    }
    Ok(key)
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(Duration::from_secs(600)).build()
}

/// Posts JSON; returns (status, body, request-id). Non-2xx bodies are returned, not raised.
fn post(path: &str, key: &str, body: &Value, beta: Option<&str>) -> Result<(u16, Value, String), String> {
    // The one place every request to Claude leaves the app: refuse when "Use Claude AI" is off.
    crate::engine::require_claude()?;
    let mut req = agent()
        .post(&format!("{}{path}", api_base()))
        .set("x-api-key", key)
        .set("anthropic-version", API_VERSION)
        .set("content-type", "application/json");
    if let Some(b) = beta {
        req = req.set("anthropic-beta", b);
    }
    let resp = match req.send_json(body) {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(format!("Network error contacting the API: {e}")),
    };
    let status = resp.status();
    let request_id = resp.header("request-id").unwrap_or("").to_string();
    let json: Value = resp.into_json().map_err(|e| format!("Unreadable API response ({status}): {e}"))?;
    Ok((status, json, request_id))
}

fn api_error(status: u16, body: &Value, request_id: &str) -> String {
    let kind = body["error"]["type"].as_str().unwrap_or("error");
    let msg = body["error"]["message"].as_str().unwrap_or("");
    let hint = match status {
        401 => " (check the API key)",
        402 | 403 => " (check Console billing/permissions)",
        429 => " (rate limited or out of credit)",
        _ => "",
    };
    format!("API {status} {kind}: {msg}{hint} [request-id {request_id}]")
}

/// Free token count for the exact request body (fields count_tokens doesn't take are dropped).
pub fn count_tokens(request: &Value) -> Result<u64, String> {
    crate::engine::require_claude()?;
    let key = api_key()?;
    let mut body = request.clone();
    if let Some(obj) = body.as_object_mut() {
        obj.remove("max_tokens");
        obj.remove("fallbacks");
    }
    let (status, json, rid) = post("/v1/messages/count_tokens", &key, &body, None)?;
    if status != 200 {
        return Err(api_error(status, &json, &rid));
    }
    json["input_tokens"].as_u64().ok_or_else(|| "count_tokens response had no input_tokens".into())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Ledger {
    pub max_calls: u32,
    pub max_usd: f64,
    pub calls: Vec<LedgerEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub unix_time: u64,
    pub project: String,
    pub model: String,
    pub request_id: String,
    pub status: u16,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

impl Ledger {
    pub fn load(path: &Path, max_calls: u32, max_usd: f64) -> Self {
        let mut l: Ledger = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        l.max_calls = max_calls;
        l.max_usd = max_usd;
        l
    }
    pub fn spent(&self) -> f64 {
        self.calls.iter().map(|c| c.cost_usd).sum()
    }
    /// Refuses unless one more call fits both the call cap and the dollar cap at its worst case.
    pub fn check(&self, worst_case_usd: f64) -> Result<(), String> {
        if self.calls.len() as u32 >= self.max_calls {
            return Err(format!("Budget: {} of {} approved paid calls already used.", self.calls.len(), self.max_calls));
        }
        if self.spent() + worst_case_usd > self.max_usd + 1e-9 {
            return Err(format!(
                "Budget: ${:.4} spent + worst case ${worst_case_usd:.4} would exceed the ${:.2} cap.",
                self.spent(),
                self.max_usd
            ));
        }
        Ok(())
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text + "\n").map_err(|e| format!("Cannot write ledger: {e}"))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanCallResult {
    pub request_id: String,
    pub model_requested: String,
    pub model_served: String,
    pub stop_reason: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub fallback_used: bool,
    pub seconds: f64,
}

/// Extracts the structured-output JSON text from a Messages API response.
pub fn plan_from_response(resp: &Value) -> Result<EditPlan, String> {
    match resp["stop_reason"].as_str() {
        Some("end_turn") => {}
        Some("refusal") => {
            return Err(format!("Model declined: {}", resp["stop_details"]["explanation"].as_str().unwrap_or("no details")))
        }
        Some("max_tokens") => return Err("Response hit max_tokens; plan is incomplete.".into()),
        other => return Err(format!("Unexpected stop_reason {other:?}.")),
    }
    let text: String = resp["content"]
        .as_array()
        .map(|blocks| blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect())
        .unwrap_or_default();
    if text.trim().is_empty() {
        return Err("Response contained no text block.".into());
    }
    serde_json::from_str(&text).map_err(|e| format!("Plan JSON did not match the schema: {e}"))
}

/// ONE paid planning call, gated by the ledger. Writes the raw response even on failure.
pub fn request_plan(request: &Value, project: &Path, ledger_path: &Path, ledger: &mut Ledger, input_tokens_est: u64) -> Result<(EditPlan, PlanCallResult), String> {
    let model = request["model"].as_str().unwrap_or_default().to_string();
    let worst = cost_usd(&model, input_tokens_est, MAX_TOKENS as u64)
        .ok_or_else(|| format!("No known price for model {model}; refusing to call."))?;
    crate::engine::require_claude()?;
    ledger.check(worst)?;
    let key = api_key()?;

    let started = std::time::Instant::now();
    let (status, json, rid) = post("/v1/messages", &key, request, Some(FALLBACK_BETA))?;
    let seconds = started.elapsed().as_secs_f64();
    std::fs::write(project.join("response.json"), serde_json::to_string_pretty(&json).unwrap_or_default() + "\n")
        .map_err(|e| format!("Cannot write response.json: {e}"))?;

    let input_tokens = json["usage"]["input_tokens"].as_u64().unwrap_or(0)
        + json["usage"]["cache_creation_input_tokens"].as_u64().unwrap_or(0)
        + json["usage"]["cache_read_input_tokens"].as_u64().unwrap_or(0);
    let output_tokens = json["usage"]["output_tokens"].as_u64().unwrap_or(0);
    let served = json["model"].as_str().unwrap_or(&model).to_string();
    // Billed as a call if the API accepted it; failed requests are recorded with their usage (often 0).
    let cost = cost_usd(&served, input_tokens, output_tokens).or_else(|| cost_usd(&model, input_tokens, output_tokens)).unwrap_or(worst);
    ledger.calls.push(LedgerEntry {
        unix_time: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        project: project.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        model: served.clone(),
        request_id: rid.clone(),
        status,
        input_tokens,
        output_tokens,
        cost_usd: if status == 200 { cost } else { 0.0 },
    });
    ledger.save(ledger_path)?;
    if status != 200 {
        return Err(api_error(status, &json, &rid));
    }
    let fallback_used = json["content"].as_array().is_some_and(|b| b.iter().any(|x| x["type"] == "fallback"))
        || json["usage"]["iterations"].as_array().is_some_and(|it| it.iter().any(|x| x["type"] == "fallback_message"));
    let result = PlanCallResult {
        request_id: rid,
        model_requested: model,
        model_served: served,
        stop_reason: json["stop_reason"].as_str().unwrap_or("").into(),
        input_tokens,
        output_tokens,
        cost_usd: cost,
        fallback_used,
        seconds,
    };
    let plan = plan_from_response(&json)?;
    Ok((plan, result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_format_validation() {
        assert!(validate_key_format("  sk-ant-TESTONLY_not-a-real-key-000  ").is_ok());
        assert!(validate_key_format("sk-proj-123456789012345678").is_err());
        assert!(validate_key_format("sk-ant-short").is_err());
        assert!(validate_key_format("sk-ant-api03-abc def 1234567890").is_err());
        assert!(validate_key_format("sk-ant-api03-abc\"; rm -rf ~ 123456").is_err());
        assert!(validate_key_format("sk-ant-api03-abcdefghij\nadd-generic-password").is_err());
    }

    #[test]
    fn prices_and_costs() {
        assert_eq!(price_per_mtok("claude-opus-5-5"), Some((4.0, 20.0)));
        let c = cost_usd("claude-opus-5-5", 4000, 16000).unwrap();
        assert!((c - 0.336).abs() < 1e-9);
        assert_eq!(cost_usd("unknown-model", 1, 1), None);
    }

    #[test]
    fn ledger_enforces_calls_and_dollars() {
        let mut l = Ledger { max_calls: 3, max_usd: 1.0, calls: vec![] };
        assert!(l.check(0.34).is_ok());
        let entry = |c| LedgerEntry {
            unix_time: 0, project: "p".into(), model: "m".into(), request_id: "r".into(),
            status: 200, input_tokens: 0, output_tokens: 0, cost_usd: c,
        };
        l.calls.push(entry(0.70));
        assert!(l.check(0.34).unwrap_err().contains("exceed"));
        assert!(l.check(0.20).is_ok());
        l.calls.push(entry(0.0));
        l.calls.push(entry(0.0));
        assert!(l.check(0.01).unwrap_err().contains("3 of 3"));
    }

    #[test]
    fn extracts_plan_and_rejects_bad_stops() {
        let plan = json!({"summary": {"title": "t", "overall_rationale": "r", "overall_confidence": "high"},
                          "segments": [], "removals": [], "captions": []});
        let ok = json!({"stop_reason": "end_turn", "content": [
            {"type": "thinking", "thinking": ""}, {"type": "text", "text": plan.to_string()}]});
        assert_eq!(plan_from_response(&ok).unwrap().summary.title, "t");
        assert!(plan_from_response(&json!({"stop_reason": "refusal", "stop_details": {"explanation": "x"}})).unwrap_err().contains("declined"));
        assert!(plan_from_response(&json!({"stop_reason": "max_tokens", "content": []})).unwrap_err().contains("max_tokens"));
        let bad = json!({"stop_reason": "end_turn", "content": [{"type": "text", "text": "{\"nope\": 1}"}]});
        assert!(plan_from_response(&bad).unwrap_err().contains("schema"));
    }

    /// Only a loopback address in AIRM_API_BASE can replace the real API, so the key cannot be redirected.
    #[test]
    fn api_base_accepts_only_loopback_overrides() {
        let _g = crate::engine::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for (value, want) in [
            ("http://127.0.0.1:8080", "http://127.0.0.1:8080"),
            ("http://localhost:9/", "http://localhost:9"),
            ("https://evil.example.com", API_BASE),
            ("http://127.0.0.1.evil.com:80", API_BASE),
            ("http://10.0.0.5:80", API_BASE),
            ("http://127.0.0.1:80@evil.com", API_BASE),
            ("http://localhost:80@evil.com/", API_BASE),
            ("http://127.0.0.1:80/v1/steal", API_BASE),
            ("http://127.0.0.1:", API_BASE),
            ("", API_BASE),
        ] {
            unsafe { std::env::set_var("AIRM_API_BASE", value) };
            assert_eq!(api_base(), want, "{value}");
        }
        unsafe { std::env::remove_var("AIRM_API_BASE") };
        assert_eq!(api_base(), API_BASE);
    }
}
