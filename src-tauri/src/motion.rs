//! Animated motion graphics (cards, stickers, arrows, lower thirds) drawn by HyperFrames.
//!
//! Each item is a small HTML + GSAP composition with a transparent background. HyperFrames renders it to a short
//! transparent WebM, and FFmpeg lays that over the reel at the chosen time. Items are cached by their content, so
//! the same card in a later render costs nothing. HyperFrames lives in the user's `kit/` folder (Node + Chrome).

use crate::extras;
use crate::plan::ValidCaption;
use crate::render::Orientation;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Bump when the templates change, so cached renders are redone.
const TEMPLATE_VERSION: u32 = 5;
const DEFAULT_SEC: f64 = 3.0;
const MIN_SEC: f64 = 1.0;
const MAX_SEC: f64 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Template {
    /// A rounded card with a title (and optional line below) that pops in.
    #[default]
    Card,
    /// A badge: check mark, star, or a text badge such as NEW / SALE.
    Sticker,
    /// A bouncing arrow that points at something, with an optional label.
    Arrow,
    /// A name/label bar that slides in from the left.
    LowerThird,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MotionSpec {
    pub template: Template,
    /// Main text (card title, badge text, arrow label, lower-third name).
    pub text: String,
    /// Second line (card subtitle, lower-third role).
    pub sub: Option<String>,
    /// Sticker: "check", "star" or "badge". Arrow: "down", "up", "left" or "right".
    pub variant: Option<String>,
    /// "top", "middle", "bottom", "left" or "right". Each template has its own default.
    pub position: Option<String>,
    /// Colour: "rose" (default), "plum", "gold", "teal" or "white".
    pub accent: Option<String>,
    /// Show at this many seconds on the finished reel.
    pub at: Option<f64>,
    /// Or: show when this phrase is spoken (talking videos).
    pub phrase: Option<String>,
    /// How long it stays (seconds).
    pub dur: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlacedMotion {
    pub spec: MotionSpec,
    pub start: f64,
    pub dur: f64,
}

/// Decides when each item is shown: a fixed time, else the first caption containing its phrase, else it is
/// spread out. Items are clamped to the reel; those with no room (or no way to place them) are reported.
pub fn place(specs: &[MotionSpec], captions: &[ValidCaption], total: f64) -> (Vec<PlacedMotion>, Vec<String>) {
    let mut out = Vec::new();
    let mut notes = Vec::new();
    let unanchored = specs.iter().filter(|s| s.at.is_none() && s.phrase.as_deref().map_or(true, |p| p.trim().is_empty())).count();
    let mut free_i = 0usize;
    for s in specs {
        if s.text.trim().is_empty() && s.template != Template::Arrow && !(s.template == Template::Sticker && s.variant.as_deref() != Some("badge")) {
            notes.push(format!("{:?} skipped: no text", s.template));
            continue;
        }
        let want = match (s.at, s.phrase.as_deref().map(str::trim).filter(|p| !p.is_empty())) {
            (Some(t), _) => Some(t),
            (None, Some(p)) => extras::find_phrase(captions, p),
            (None, None) => {
                free_i += 1;
                Some((total * free_i as f64 / (unanchored + 1) as f64 - DEFAULT_SEC / 2.0).max(0.0))
            }
        };
        let Some(start) = want.filter(|t| t.is_finite() && *t >= 0.0) else {
            notes.push(format!("\"{}\" skipped: the phrase was not found", s.text));
            continue;
        };
        let dur = s.dur.unwrap_or(DEFAULT_SEC).clamp(MIN_SEC, MAX_SEC).min(total - start);
        if dur < MIN_SEC - 1e-9 {
            notes.push(format!("\"{}\" skipped: no room at {start:.1}s", s.text));
            continue;
        }
        out.push(PlacedMotion { spec: s.clone(), start, dur });
    }
    (out, notes)
}

fn esc(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&#39;".to_string(),
            c => c.to_string(),
        })
        .collect()
}

/// (gradient start, gradient end, text colour) for an accent name.
fn colours(accent: Option<&str>) -> (&'static str, &'static str, &'static str) {
    match accent {
        Some("plum") => ("#7a3050", "#4d1f35", "#ffffff"),
        Some("gold") => ("#f7d36b", "#e9a93a", "#3b2a06"),
        Some("teal") => ("#2dd4bf", "#0f9d8e", "#06322d"),
        Some("white") => ("#ffffff", "#e9e6f0", "#3a2540"),
        _ => ("#d9577a", "#a83a5c", "#ffffff"),
    }
}

/// Top-left anchor of an item as a fraction of the frame, from a position name.
fn anchor(position: Option<&str>, default: &str, vertical: bool) -> (f64, f64) {
    let p = position.unwrap_or(default);
    match (p, vertical) {
        ("top", true) => (0.5, 0.16),
        ("top", false) => (0.5, 0.18),
        ("middle", _) => (0.5, 0.5),
        ("bottom", true) => (0.5, 0.78),
        ("bottom", false) => (0.5, 0.82),
        ("left", true) => (0.2, 0.42),
        ("left", false) => (0.18, 0.5),
        ("right", true) => (0.8, 0.42),
        ("right", false) => (0.82, 0.5),
        _ => (0.5, 0.5),
    }
}

const BASE_CSS: &str = "*{box-sizing:border-box}html,body{margin:0;overflow:hidden;background:transparent;font-family:-apple-system,'Helvetica Neue',Arial,sans-serif}\
#main{position:relative;overflow:hidden;background:transparent}\
.el{position:absolute;transform:translate(-50%,-50%);will-change:transform,opacity}";

/// The full HTML composition for one item, sized for `orientation` and lasting `dur` seconds.
pub fn html(spec: &MotionSpec, orientation: Orientation, dur: f64) -> String {
    let (w, h) = orientation.size();
    let vertical = orientation == Orientation::Vertical;
    let (c1, c2, ink) = colours(spec.accent.as_deref());
    let s = if vertical { 1.0 } else { 0.9 };
    let exit = (dur - 0.4).max(0.6);
    let text = esc(spec.text.trim());
    let sub = spec.sub.as_deref().map(str::trim).filter(|x| !x.is_empty()).map(esc);

    let (body, css, js) = match spec.template {
        Template::Card => {
            let (ax, ay) = anchor(spec.position.as_deref(), "top", vertical);
            let sub_html = sub.as_ref().map(|t| format!("<div class=\"sub\">{t}</div>")).unwrap_or_default();
            (
                format!("<div id=\"el\" class=\"el clip\" data-start=\"0\" data-duration=\"{dur:.2}\" data-track-index=\"1\" style=\"left:{:.1}%;top:{:.1}%\"><div class=\"card\"><div class=\"shine\"></div><div class=\"t\">{text}</div>{sub_html}</div></div>", ax * 100.0, ay * 100.0),
                format!(
                    ".card{{position:relative;overflow:hidden;width:max-content;min-width:{mw:.0}px;max-width:{xw:.0}px;padding:{py:.0}px {px:.0}px;border-radius:{r:.0}px;background:linear-gradient(135deg,{c1},{c2});color:{ink};text-align:center;border:4px solid rgba(255,255,255,.75);box-shadow:0 18px 50px rgba(0,0,0,.4)}}\
                     .t{{font-weight:900;font-size:{fs:.0}px;line-height:1.1;letter-spacing:-1px}}.sub{{margin-top:10px;font-weight:600;font-size:{fs2:.0}px;opacity:.9}}\
                     .shine{{position:absolute;top:0;left:0;width:40%;height:100%;background:linear-gradient(100deg,transparent,rgba(255,255,255,.55),transparent)}}",
                    mw = 520.0 * s, xw = w as f64 * 0.86, py = 34.0 * s, px = 44.0 * s, r = 38.0 * s, fs = 78.0 * s, fs2 = 40.0 * s
                ),
                format!(
                    "tl.from('#el',{{y:140,scale:.55,opacity:0,duration:.55,ease:'back.out(1.8)'}},0);\
                     tl.fromTo('.shine',{{xPercent:-150,skewX:-20}},{{xPercent:350,skewX:-20,duration:.9,ease:'power2.inOut'}},.45);\
                     tl.to('#el',{{y:-70,opacity:0,scale:.9,duration:.4,ease:'power2.in'}},{exit:.2});"
                ),
            )
        }
        Template::Sticker => {
            let (ax, ay) = anchor(spec.position.as_deref(), "right", vertical);
            let variant = spec.variant.as_deref().unwrap_or("badge");
            let size = 250.0 * s;
            let inner = match variant {
                "check" => format!("<svg viewBox=\"0 0 100 100\" width=\"{size:.0}\" height=\"{size:.0}\"><circle cx=\"50\" cy=\"50\" r=\"46\" fill=\"url(#g)\" stroke=\"#fff\" stroke-width=\"4\"/><defs><linearGradient id=\"g\" x1=\"0\" y1=\"0\" x2=\"1\" y2=\"1\"><stop offset=\"0\" stop-color=\"#4ade80\"/><stop offset=\"1\" stop-color=\"#16a34a\"/></linearGradient></defs><path id=\"tick\" d=\"M28 52 L44 68 L74 34\" fill=\"none\" stroke=\"#fff\" stroke-width=\"11\" stroke-linecap=\"round\" stroke-linejoin=\"round\" pathLength=\"1\" stroke-dasharray=\"1\"/></svg>"),
                "star" => format!("<svg viewBox=\"0 0 100 100\" width=\"{size:.0}\" height=\"{size:.0}\"><polygon points=\"50,6 61,38 95,38 67,58 78,92 50,72 22,92 33,58 5,38 39,38\" fill=\"url(#g)\" stroke=\"#fff\" stroke-width=\"4\" stroke-linejoin=\"round\"/><defs><linearGradient id=\"g\" x1=\"0\" y1=\"0\" x2=\"1\" y2=\"1\"><stop offset=\"0\" stop-color=\"#ffe27a\"/><stop offset=\"1\" stop-color=\"#f5a623\"/></linearGradient></defs></svg>"),
                _ => format!("<div class=\"badge\">{}</div>", if text.is_empty() { "NEW".to_string() } else { text.clone() }),
            };
            let tick = if variant == "check" { "tl.fromTo('#tick',{strokeDashoffset:1},{strokeDashoffset:0,duration:.45,ease:'power2.out'},.35);" } else { "" };
            (
                format!("<div id=\"el\" class=\"el clip\" data-start=\"0\" data-duration=\"{dur:.2}\" data-track-index=\"1\" style=\"left:{:.1}%;top:{:.1}%\">{inner}</div>", ax * 100.0, ay * 100.0),
                format!(
                    ".badge{{padding:{py:.0}px {px:.0}px;border-radius:{r:.0}px;background:linear-gradient(135deg,{c1},{c2});color:{ink};font-weight:900;font-size:{fs:.0}px;letter-spacing:1px;border:5px solid #fff;box-shadow:0 14px 36px rgba(0,0,0,.45);text-align:center;white-space:nowrap}}",
                    py = 22.0 * s, px = 40.0 * s, r = 30.0 * s, fs = 80.0 * s
                ),
                format!(
                    "tl.from('#el',{{scale:0,rotation:-35,opacity:0,duration:.6,ease:'back.out(2.4)'}},0);{tick}\
                     tl.to('#el',{{rotation:6,duration:.35,yoyo:true,repeat:{reps},ease:'sine.inOut'}},.7);\
                     tl.to('#el',{{scale:0,rotation:25,opacity:0,duration:.35,ease:'back.in(2)'}},{exit:.2});",
                    reps = ((exit - 0.7) / 0.35).floor().max(1.0) as i64 - 1
                ),
            )
        }
        Template::Arrow => {
            let dir = spec.variant.as_deref().unwrap_or("down");
            let (ax, mut ay) = anchor(spec.position.as_deref(), if dir == "down" { "bottom" } else { "middle" }, vertical);
            // The default bottom spot sits clear of the captions (which are centred at about 70% / 82%).
            if spec.position.is_none() && dir == "down" {
                ay = if vertical { 0.86 } else { 0.62 };
            }
            let (rot, dx, dy) = match dir {
                "up" => (180, 0.0, -1.0),
                "left" => (90, -1.0, 0.0),
                "right" => (-90, 1.0, 0.0),
                _ => (0, 0.0, 1.0),
            };
            let size = 230.0 * s;
            let label = if text.is_empty() { String::new() } else { format!("<div class=\"lbl\">{text}</div>") };
            (
                format!("<div id=\"el\" class=\"el clip\" data-start=\"0\" data-duration=\"{dur:.2}\" data-track-index=\"1\" style=\"left:{:.1}%;top:{:.1}%\">{label}<div id=\"arr\" style=\"transform:rotate({rot}deg)\"><svg viewBox=\"0 0 100 120\" width=\"{size:.0}\" height=\"{:.0}\"><defs><linearGradient id=\"g\" x1=\"0\" y1=\"0\" x2=\"0\" y2=\"1\"><stop offset=\"0\" stop-color=\"{c1}\"/><stop offset=\"1\" stop-color=\"{c2}\"/></linearGradient></defs><path d=\"M36 4 H64 V58 H86 L50 112 L14 58 H36 Z\" fill=\"url(#g)\" stroke=\"#fff\" stroke-width=\"5\" stroke-linejoin=\"round\"/></svg></div></div>", ax * 100.0, ay * 100.0, size * 1.2),
                format!(
                    "#el{{display:flex;flex-direction:column;align-items:center;gap:8px}}#arr{{filter:drop-shadow(0 10px 18px rgba(0,0,0,.45))}}\
                     .lbl{{padding:{py:.0}px {px:.0}px;border-radius:{r:.0}px;background:linear-gradient(135deg,{c1},{c2});color:{ink};font-weight:900;font-size:{fs:.0}px;border:4px solid #fff;box-shadow:0 12px 30px rgba(0,0,0,.4);white-space:nowrap}}",
                    py = 14.0 * s, px = 30.0 * s, r = 24.0 * s, fs = 52.0 * s
                ),
                format!(
                    "tl.from('#el',{{opacity:0,scale:.4,duration:.45,ease:'back.out(2)'}},0);\
                     tl.to('#arr',{{x:{:.0},y:{:.0},duration:.4,yoyo:true,repeat:{reps},ease:'sine.inOut'}},.5);\
                     tl.to('#el',{{opacity:0,scale:.6,duration:.35,ease:'power2.in'}},{exit:.2});",
                    dx * 42.0 * s, dy * 42.0 * s, reps = ((exit - 0.5) / 0.4).floor().max(1.0) as i64 - 1
                ),
            )
        }
        Template::LowerThird => {
            let (_, ay) = anchor(spec.position.as_deref(), "bottom", vertical);
            let sub_html = sub.as_ref().map(|t| format!("<div class=\"sub\">{t}</div>")).unwrap_or_default();
            (
                format!("<div id=\"el\" class=\"clip\" data-start=\"0\" data-duration=\"{dur:.2}\" data-track-index=\"1\" style=\"position:absolute;left:{:.0}px;top:{:.1}%\"><div class=\"bar\"></div><div class=\"txt\"><div class=\"name\">{text}</div>{sub_html}</div></div>", w as f64 * 0.06, ay * 100.0),
                format!(
                    "#el{{display:flex;align-items:stretch;gap:0}}.bar{{width:{bw:.0}px;border-radius:99px;background:linear-gradient(180deg,{c1},{c2});transform-origin:top}}\
                     .txt{{padding:{py:.0}px {px:.0}px;background:rgba(14,12,20,.78);border-radius:0 {r:.0}px {r:.0}px 0;color:#fff;backdrop-filter:blur(4px)}}\
                     .name{{font-weight:800;font-size:{fs:.0}px;letter-spacing:.5px}}.sub{{margin-top:6px;font-weight:500;font-size:{fs2:.0}px;color:#d9d3e6}}",
                    bw = 12.0 * s, py = 18.0 * s, px = 30.0 * s, r = 22.0 * s, fs = 58.0 * s, fs2 = 34.0 * s
                ),
                format!(
                    "tl.set('#el',{{yPercent:-50}},0);tl.from('.bar',{{scaleY:0,duration:.4,ease:'power3.out'}},0);\
                     tl.from('.txt',{{x:-60,opacity:0,duration:.5,ease:'power3.out'}},.2);\
                     tl.to('#el',{{x:-80,opacity:0,duration:.4,ease:'power2.in'}},{exit:.2});"
                ),
            )
        }
    };

    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><title>motion</title>\n<script src=\"assets/gsap.min.js\"></script>\n<style>{BASE_CSS}html,body,#main{{width:{w}px;height:{h}px}}{css}</style></head><body>\n\
         <div id=\"main\" data-composition-id=\"main\" data-start=\"0\" data-duration=\"{dur:.2}\" data-width=\"{w}\" data-height=\"{h}\">\n{body}\n</div>\n\
         <script>\nconst tl=gsap.timeline({{paused:true}});\n{js}\nwindow.__timelines=window.__timelines||{{}};window.__timelines['main']=tl;\n</script></body></html>\n"
    )
}

/// A HyperFrames install (the user's `kit/` folder) and the Node that runs it.
#[derive(Debug, Clone)]
pub struct Kit {
    pub dir: PathBuf,
    pub node: PathBuf,
    pub cli: PathBuf,
    pub gsap: PathBuf,
}

fn find_node() -> Option<PathBuf> {
    ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].iter().map(|d| Path::new(d).join("node")).find(|p| p.is_file())
}

/// Folders where the kit is usually found, tried in order when the user has not chosen one.
fn default_kit_dirs() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return Vec::new() };
    vec![home.join("Downloads/my-free247editor/kit"), home.join("my-free247editor/kit"), home.join("Projects/my-free247editor/kit")]
}

impl Kit {
    /// The kit in `dir` (or the usual places), checked for HyperFrames, GSAP and Node.
    pub fn find(dir: Option<&str>) -> Result<Kit, String> {
        let candidates: Vec<PathBuf> = match dir.map(str::trim).filter(|d| !d.is_empty()) {
            Some(d) => vec![PathBuf::from(d)],
            None => default_kit_dirs(),
        };
        let mut why = String::from("No HyperFrames folder is set.");
        for c in &candidates {
            let cli = c.join("node_modules/hyperframes/bin/hyperframes.mjs");
            let gsap = c.join("node_modules/gsap/dist/gsap.min.js");
            if !cli.is_file() || !gsap.is_file() {
                why = format!("{} does not look like the free247editor kit folder (HyperFrames is missing).", c.display());
                continue;
            }
            let node = find_node().ok_or("Node.js was not found. Install it from nodejs.org (version 22 or newer).")?;
            return Ok(Kit { dir: c.clone(), node, cli, gsap });
        }
        Err(format!("{why} Choose the kit folder in Settings."))
    }
}

fn cache_key(spec: &MotionSpec, orientation: Orientation, dur: f64) -> String {
    let json = serde_json::to_string(&(spec, orientation, (dur * 100.0).round() as i64, TEMPLATE_VERSION)).unwrap_or_default();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in json.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// Renders one item to a transparent WebM (cached by content) and returns its path.
pub fn render(kit: &Kit, spec: &MotionSpec, orientation: Orientation, dur: f64, cache_dir: &Path) -> Result<PathBuf, String> {
    let key = cache_key(spec, orientation, dur);
    let out = cache_dir.join(format!("{key}.webm"));
    if out.metadata().map(|m| m.len() > 1000).unwrap_or(false) {
        return Ok(out);
    }
    let work = cache_dir.join(format!("work-{key}"));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(work.join("assets")).map_err(|e| format!("Cannot create {}: {e}", work.display()))?;
    std::fs::copy(&kit.gsap, work.join("assets/gsap.min.js")).map_err(|e| format!("Cannot copy GSAP: {e}"))?;
    let (w, h) = orientation.size();
    std::fs::write(work.join("index.html"), html(spec, orientation, dur)).map_err(|e| e.to_string())?;
    std::fs::write(work.join("meta.json"), format!("{{\"id\":\"main\",\"name\":\"motion\",\"width\":{w},\"height\":{h},\"fps\":30}}")).map_err(|e| e.to_string())?;
    std::fs::write(
        work.join("hyperframes.json"),
        "{\"$schema\":\"https://hyperframes.heygen.com/schema/hyperframes.json\",\"paths\":{\"blocks\":\"compositions\",\"components\":\"compositions/components\",\"assets\":\"assets\"}}",
    )
    .map_err(|e| e.to_string())?;
    let tmp = work.join("out.webm");
    // The app may be launched from Finder with a bare PATH, so give HyperFrames the folders it needs.
    let path = format!("{}:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin", kit.node.parent().map(|p| p.display().to_string()).unwrap_or_default());
    // The kit's own checker first: a broken composition is reported by name instead of failing inside the renderer.
    let lint = Command::new(&kit.node).arg(&kit.cli).arg("lint").current_dir(&work).env("PATH", &path).stdin(Stdio::null()).output().map_err(|e| format!("Could not run HyperFrames: {e}"))?;
    if !lint.status.success() {
        let text = format!("{}{}", String::from_utf8_lossy(&lint.stdout), String::from_utf8_lossy(&lint.stderr));
        let problems: Vec<&str> = text.lines().filter(|l| l.contains('\u{2717}')).map(str::trim).take(4).collect();
        let _ = std::fs::remove_dir_all(&work);
        return Err(format!("HyperFrames lint rejected the \"{}\" graphic:\n{}", spec.text, problems.join("\n")));
    }
    let output = Command::new(&kit.node)
        .arg(&kit.cli)
        .args(["render", "--format", "webm", "--output"])
        .arg(&tmp)
        .current_dir(&work)
        .env("PATH", path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Could not run HyperFrames: {e}"))?;
    if !output.status.success() || !tmp.is_file() {
        let err = String::from_utf8_lossy(&output.stderr);
        let log = String::from_utf8_lossy(&output.stdout);
        let tail: Vec<&str> = err.lines().chain(log.lines()).filter(|l| !l.trim().is_empty()).rev().take(6).collect();
        let _ = std::fs::remove_dir_all(&work);
        return Err(format!("HyperFrames could not render \"{}\":\n{}", spec.text, tail.into_iter().rev().collect::<Vec<_>>().join("\n")));
    }
    std::fs::rename(&tmp, &out).map_err(|e| format!("Cannot save the animation: {e}"))?;
    let _ = std::fs::remove_dir_all(&work);
    Ok(out)
}

/// Stops with a clear message when `herk-edit-kit` is switched off.
pub fn ensure_kit_enabled(use_kit: Option<bool>) -> Result<(), String> {
    crate::engine::require_kit(use_kit).map_err(|e| format!("Animated graphics need herk-edit-kit. {e}"))
}

/// Renders every placed item (reusing cached ones, a few at a time) and returns the video overlays for the renderer.
pub fn overlays(kit: &Kit, placed: &[PlacedMotion], orientation: Orientation, cache_dir: &Path) -> Result<Vec<crate::render::VideoOverlay>, String> {
    const PARALLEL: usize = 3;
    std::fs::create_dir_all(cache_dir).map_err(|e| format!("Cannot create {}: {e}", cache_dir.display()))?;
    let mut paths: Vec<Option<Result<PathBuf, String>>> = (0..placed.len()).map(|_| None).collect();
    for (chunk_no, chunk) in placed.chunks(PARALLEL).enumerate() {
        let results: Vec<Result<PathBuf, String>> = std::thread::scope(|s| {
            let handles: Vec<_> = chunk.iter().map(|p| s.spawn(move || render(kit, &p.spec, orientation, p.dur, cache_dir))).collect();
            handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("A graphic failed to render.".into()))).collect()
        });
        for (i, r) in results.into_iter().enumerate() {
            paths[chunk_no * PARALLEL + i] = Some(r);
        }
    }
    placed
        .iter()
        .zip(paths)
        .map(|(p, r)| Ok(crate::render::VideoOverlay { path: r.expect("rendered")?, start: p.start, end: p.start + p.dur }))
        .collect()
}

/// A short MP4 of one graphic playing over `background` (a video; its first frames) or, without one, a neutral
/// backdrop, so the user can see exactly how it moves. Cached like the graphic itself.
pub fn preview(kit: &Kit, spec: &MotionSpec, orientation: Orientation, background: Option<&Path>, cache_dir: &Path) -> Result<PathBuf, String> {
    let ffmpeg = crate::media::find_tool("ffmpeg").ok_or("FFmpeg not found.")?;
    let dur = spec.dur.unwrap_or(DEFAULT_SEC).clamp(MIN_SEC, MAX_SEC);
    std::fs::create_dir_all(cache_dir).map_err(|e| format!("Cannot create {}: {e}", cache_dir.display()))?;
    let webm = render(kit, spec, orientation, dur, cache_dir)?;
    let (w, h) = orientation.size();
    let bg_key = background.map(|b| b.to_string_lossy().into_owned()).unwrap_or_default();
    let out = cache_dir.join(format!("preview-{}-{:08x}.mp4", cache_key(spec, orientation, dur), bg_key.bytes().fold(5381u32, |a, b| a.wrapping_mul(33) ^ b as u32)));
    if out.metadata().map(|m| m.len() > 1000).unwrap_or(false) {
        return Ok(out);
    }
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-y", "-v", "error"]);
    match background {
        Some(b) => {
            cmd.args(["-ss", "0", "-t", &format!("{dur:.2}"), "-i"]).arg(b);
        }
        None => {
            cmd.args(["-f", "lavfi", "-i", &format!("color=c=0x5d6b73:s={w}x{h}:r=30:d={dur:.2}")]);
        }
    }
    let status = cmd
        .args(["-c:v", "libvpx-vp9", "-i"])
        .arg(&webm)
        .args([
            "-filter_complex",
            &format!("[0:v]scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h},setsar=1,fps=30,format=yuv420p[b];[1:v]format=rgba[o];[b][o]overlay=0:0:eof_action=pass:format=auto,format=yuv420p"),
            "-t", &format!("{dur:.2}"), "-an", "-c:v", "libx264", "-preset", "veryfast", "-crf", "22", "-movflags", "+faststart",
        ])
        .arg(&out)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("Failed to start FFmpeg: {e}"))?;
    if !status.success() || !out.is_file() {
        let _ = std::fs::remove_file(&out);
        return Err("Could not make the preview video.".into());
    }
    Ok(out)
}

/// `~/Library/Application Support/ai-reel-maker/motion-cache`
pub fn cache_dir() -> Result<PathBuf, String> {
    Ok(crate::prefs::app_dir()?.join("motion-cache"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(t: Template, text: &str) -> MotionSpec {
        MotionSpec { template: t, text: text.into(), ..Default::default() }
    }

    fn cap(t: &str, a: f64, b: f64) -> ValidCaption {
        ValidCaption { text: t.into(), out_start: a, out_end: b }
    }

    #[test]
    fn html_is_a_valid_composition_and_escapes_text() {
        for t in [Template::Card, Template::Sticker, Template::Arrow, Template::LowerThird] {
            let h = html(&spec(t, "A <b>&\"quoted\"</b>"), Orientation::Vertical, 3.0);
            assert!(h.contains("data-composition-id=\"main\"") && h.contains("data-duration=\"3.00\""), "{t:?}");
            assert!(h.contains("window.__timelines['main']=tl"), "{t:?}");
            assert!(h.contains("A &lt;b&gt;&amp;&quot;quoted&quot;&lt;/b&gt;") || t == Template::Sticker, "{t:?} must escape");
            assert!(!h.contains("<b>"), "{t:?} must not inject markup");
            assert!(h.contains("class=\"el clip\"") || h.contains("class=\"clip\""), "{t:?}");
        }
        let h = html(&spec(Template::Card, "X"), Orientation::Horizontal, 3.0);
        assert!(h.contains("width:1920px;height:1080px") && h.contains("data-width=\"1920\""));
    }

    #[test]
    fn sticker_and_arrow_variants() {
        let mut s = spec(Template::Sticker, "SALE");
        assert!(html(&s, Orientation::Vertical, 3.0).contains(">SALE</div>"));
        s.variant = Some("check".into());
        assert!(html(&s, Orientation::Vertical, 3.0).contains("id=\"tick\""));
        s.variant = Some("star".into());
        assert!(html(&s, Orientation::Vertical, 3.0).contains("<polygon"));
        let mut a = spec(Template::Arrow, "");
        a.variant = Some("up".into());
        assert!(html(&a, Orientation::Vertical, 3.0).contains("rotate(180deg)"));
    }

    #[test]
    fn placement_by_time_phrase_and_spread() {
        let caps = vec![cap("sobrang", 0.0, 2.0), cap("ergonomic niya", 8.0, 9.0)];
        let mut by_phrase = spec(Template::Card, "ERGONOMIC");
        by_phrase.phrase = Some("ergonomic".into());
        let mut by_time = spec(Template::Sticker, "NEW");
        by_time.at = Some(1.0);
        by_time.dur = Some(2.0);
        let lost = MotionSpec { phrase: Some("nope".into()), ..spec(Template::Card, "LOST") };
        let free1 = spec(Template::Card, "A");
        let free2 = spec(Template::Card, "B");
        let (p, notes) = place(&[by_phrase, by_time, lost, free1, free2], &caps, 20.0);
        assert_eq!(p.len(), 4);
        assert_eq!(p[0].start, 8.0);
        assert_eq!((p[1].start, p[1].dur), (1.0, 2.0));
        assert!(p[2].start < p[3].start, "unanchored items are spread out");
        assert_eq!(notes.len(), 1);
        // The last seconds of the reel: duration shrinks, and an item with no room is dropped.
        let mut late = spec(Template::Card, "LATE");
        late.at = Some(19.5);
        let (p, n) = place(&[late], &caps, 20.0);
        assert!(p.is_empty() && n.len() == 1, "{p:?} {n:?}");
        let mut end = spec(Template::Card, "END");
        end.at = Some(17.5);
        let (p, _) = place(&[end], &caps, 20.0);
        assert_eq!(p[0].dur, 2.5);
    }

    #[test]
    fn cache_key_changes_with_the_content() {
        let a = spec(Template::Card, "A");
        let b = spec(Template::Card, "B");
        assert_eq!(cache_key(&a, Orientation::Vertical, 3.0), cache_key(&a, Orientation::Vertical, 3.0));
        assert_ne!(cache_key(&a, Orientation::Vertical, 3.0), cache_key(&b, Orientation::Vertical, 3.0));
        assert_ne!(cache_key(&a, Orientation::Vertical, 3.0), cache_key(&a, Orientation::Horizontal, 3.0));
        assert_ne!(cache_key(&a, Orientation::Vertical, 3.0), cache_key(&a, Orientation::Vertical, 4.0));
    }

    #[test]
    fn arrow_default_sits_below_the_captions() {
        let a = MotionSpec { template: Template::Arrow, ..Default::default() };
        assert!(html(&a, Orientation::Vertical, 3.0).contains("top:86.0%"));
        let placed = MotionSpec { position: Some("middle".into()), ..a };
        assert!(html(&placed, Orientation::Vertical, 3.0).contains("top:50.0%"), "a chosen place is respected");
    }

    #[test]
    fn kit_errors_are_explained() {
        let e = Kit::find(Some("/definitely/not/here")).unwrap_err();
        assert!(e.contains("does not look like") && e.contains("Settings"), "{e}");
    }

    /// With the real kit installed: every template renders to a transparent WebM that FFmpeg can lay over video.
    #[test]
    fn real_templates_render_with_transparency() {
        let Ok(kit) = Kit::find(None) else {
            eprintln!("skipped: no HyperFrames kit on this computer");
            return;
        };
        let Some(ffmpeg) = crate::media::find_tool("ffmpeg") else { return };
        let dir = std::env::temp_dir().join(format!("arm-motion-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let items = [
            MotionSpec { text: "1.5 YEARS NA".into(), sub: Some("MATIBAY PA RIN".into()), ..Default::default() },
            MotionSpec { template: Template::Sticker, text: "NEW".into(), ..Default::default() },
            MotionSpec { template: Template::Sticker, variant: Some("check".into()), ..Default::default() },
            MotionSpec { template: Template::Arrow, text: "YELLOW BASKET".into(), ..Default::default() },
            MotionSpec { template: Template::LowerThird, text: "SIARGAO".into(), sub: Some("Day 1".into()), ..Default::default() },
        ];
        for spec in &items {
            let webm = render(&kit, spec, Orientation::Vertical, 2.5, &dir).unwrap_or_else(|e| panic!("{:?}: {e}", spec.template));
            // A second call is served from the cache without rendering again.
            let t = std::time::Instant::now();
            assert_eq!(render(&kit, spec, Orientation::Vertical, 2.5, &dir).unwrap(), webm);
            assert!(t.elapsed().as_millis() < 200, "cache miss for {:?}", spec.template);
            // Lay it over solid red and check: corner untouched (transparent), something drawn in the middle.
            let png = dir.join(format!("{:?}.png", spec.template));
            let ok = Command::new(&ffmpeg)
                .args(["-y", "-v", "error", "-f", "lavfi", "-i", "color=c=0xff0000:s=1080x1920:r=30:d=3", "-c:v", "libvpx-vp9", "-i"])
                .arg(&webm)
                .args(["-filter_complex", "[1:v]format=rgba[o];[0:v][o]overlay=0:0", "-ss", "1.2", "-frames:v", "1"])
                .arg(&png)
                .status()
                .unwrap()
                .success();
            assert!(ok, "{:?} could not be overlaid", spec.template);
            let img = image::open(&png).unwrap().to_rgb8();
            let corner = img.get_pixel(2, 2).0;
            assert!(corner[0] > 240 && corner[1] < 15 && corner[2] < 15, "{:?}: corner should stay red, got {corner:?}", spec.template);
            let changed = img.pixels().filter(|p| p.0[1] > 40 || p.0[0] < 200).count();
            assert!(changed > 2000, "{:?}: nothing was drawn ({changed} px)", spec.template);
        }
        // The preview over a plain backdrop and over a real video.
        let p1 = preview(&kit, &items[0], Orientation::Vertical, None, &dir).unwrap();
        let info = crate::media::probe(&p1.to_string_lossy()).unwrap();
        assert_eq!((info.width, info.height), (1080, 1920));
        assert!((info.duration_sec - 2.5).abs() < 0.4 || (info.duration_sec - 3.0).abs() < 0.4, "{}", info.duration_sec);
        let bg = dir.join("bg.mp4");
        assert!(Command::new(&ffmpeg)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25:duration=5", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&bg)
            .status()
            .unwrap()
            .success());
        let p2 = preview(&kit, &items[0], Orientation::Horizontal, Some(&bg), &dir).unwrap();
        assert_eq!(crate::media::probe(&p2.to_string_lossy()).unwrap().width, 1920);
        assert_eq!(preview(&kit, &items[0], Orientation::Horizontal, Some(&bg), &dir).unwrap(), p2, "second call is cached");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
