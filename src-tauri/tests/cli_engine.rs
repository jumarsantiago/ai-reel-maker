//! The command-line tool obeys the same saved "Use Claude AI" switch as the app. Real binary, real prefs file
//! (in a temporary HOME), and a local stand-in for the Claude API, so nothing reaches the internet.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

fn temp_home(tag: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("arm-cli-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("Library/Application Support/ai-reel-maker")).unwrap();
    home
}

fn save_prefs(home: &Path, use_claude: bool) {
    let p = home.join("Library/Application Support/ai-reel-maker/prefs.json");
    std::fs::write(p, format!("{{\"last\": {{\"useKit\": true, \"useClaude\": {use_claude}}}}}")).unwrap();
}

fn project(home: &Path) -> PathBuf {
    let dir = home.join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("request.json"), r#"{"model":"claude-opus-5-5","max_tokens":16000,"messages":[]}"#).unwrap();
    dir
}

fn run(home: &Path, dir: &Path, api: Option<&str>) -> (bool, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_autoedit"));
    cmd.args(["count", &dir.to_string_lossy()]).env("HOME", home).env_remove("ANTHROPIC_API_KEY").env_remove("AIRM_API_BASE");
    if let Some(base) = api {
        cmd.env("AIRM_API_BASE", base).env("ANTHROPIC_API_KEY", "sk-ant-test-key-not-real");
    }
    let out = cmd.output().unwrap();
    (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn the_cli_follows_the_saved_claude_switch() {
    let home = temp_home("claude");
    let dir = project(&home);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            // Read the headers and then the whole body, so the client is never cut off mid-request.
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
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
            let _ = tx.send(String::from_utf8_lossy(&buf).lines().next().unwrap_or("").to_string());
            let body = "{\"input_tokens\": 4321}";
            let _ = write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
        }
    });

    // Saved switch OFF: the tool refuses, and no request is made.
    save_prefs(&home, false);
    let (ok, text) = run(&home, &dir, Some(&base));
    assert!(!ok && text.contains("Use Claude AI"), "{text}");
    assert!(rx.try_recv().is_err(), "nothing was sent while the switch was off");

    // Nothing saved at all: the default is OFF too.
    std::fs::remove_file(home.join("Library/Application Support/ai-reel-maker/prefs.json")).unwrap();
    let (ok, text) = run(&home, &dir, Some(&base));
    assert!(!ok && text.contains("Use Claude AI"), "default is off: {text}");

    // Saved switch ON: the same command now reaches the (local stand-in) API and prints the count.
    save_prefs(&home, true);
    let (ok, text) = run(&home, &dir, Some(&base));
    assert!(ok && text.contains("4321"), "{text}");
    assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), "POST /v1/messages/count_tokens HTTP/1.1");
    let _ = std::fs::remove_dir_all(&home);
}

