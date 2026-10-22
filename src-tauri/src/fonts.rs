//! Caption font choices: lists the system and user fonts that can draw Tagalog/English captions, and renders a
//! real preview of a caption in a chosen font (the same code that draws the captions in the video).

use crate::captions::{self, CaptionTemplate};
use ab_glyph::{Font, FontVec};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Fonts bigger than this (CJK collections) are skipped; they are slow to load and not made for Latin captions.
const MAX_FONT_BYTES: u64 = 12 * 1024 * 1024;
const SAMPLE: &str = "Sobrang ganda ng byahe!";

#[derive(Debug, Clone, Serialize)]
pub struct FontInfo {
    pub name: String,
    pub path: String,
}

fn is_font_file(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| ["ttf", "otf", "ttc", "otc"].contains(&e.to_ascii_lowercase().as_str()))
}

/// True if the font draws the letters, digits and punctuation captions need (including "ñ").
fn covers_captions(font: &FontVec) -> bool {
    "AaGgÑñ09!?,.'".chars().all(|c| font.glyph_id(c).0 != 0)
}

fn collect(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() && depth < 2 {
            collect(&p, depth + 1, out);
        } else if is_font_file(&p) && std::fs::metadata(&p).map(|m| m.len() <= MAX_FONT_BYTES).unwrap_or(false) {
            out.push(p);
        }
    }
}

/// Usable caption fonts, sorted by name. A name that appears in several folders is listed once.
pub fn list_fonts() -> Vec<FontInfo> {
    let mut files = Vec::new();
    for d in captions::font_dirs() {
        collect(&d, 0, &mut files);
    }
    let mut out: Vec<FontInfo> = Vec::new();
    for p in files {
        let Ok(bytes) = std::fs::read(&p) else { continue };
        let Ok(font) = FontVec::try_from_vec(bytes) else { continue };
        if !covers_captions(&font) {
            continue;
        }
        let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        if name.is_empty() || out.iter().any(|f| f.name.eq_ignore_ascii_case(&name)) {
            continue;
        }
        out.push(FontInfo { name, path: p.to_string_lossy().into_owned() });
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

/// A real caption in the chosen template and `font` (None = the built-in rounded font), as a PNG data URL for the UI.
pub fn preview_data_url(font: Option<&str>, text: Option<&str>, template: CaptionTemplate) -> Result<String, String> {
    let font = captions::load_font_choice(font, captions::SOFT_FONT)?;
    let text = text.map(str::trim).filter(|t| !t.is_empty()).unwrap_or(SAMPLE);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("airm-font-preview-{}-{nanos}.png", std::process::id()));
    let style = template.style(1080);
    let res = captions::render_words_png(&font, &style, text, Some(1), &tmp);
    let bytes = std::fs::read(&tmp);
    let _ = std::fs::remove_file(&tmp);
    res?;
    let bytes = bytes.map_err(|e| format!("Cannot read preview: {e}"))?;
    Ok(format!("data:image/png;base64,{}", base64(&bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_values() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn default_preview_is_a_png_data_url() {
        if captions::load_font_for(captions::SOFT_FONT).is_err() {
            return; // no system fonts here
        }
        let url = preview_data_url(None, None, CaptionTemplate::Soft).unwrap();
        assert!(url.starts_with("data:image/png;base64,iVBOR"), "{}", &url[..40.min(url.len())]);
    }

    #[test]
    fn lists_usable_fonts_and_previews_each_kind() {
        let fonts = list_fonts();
        if fonts.is_empty() {
            return; // CI without fonts
        }
        assert!(fonts.windows(2).all(|w| w[0].name.to_lowercase() <= w[1].name.to_lowercase()));
        // Every listed font must actually be usable for a preview.
        for f in fonts.iter().take(8) {
            preview_data_url(Some(&f.path), Some("Kumusta! Ñandú 123"), CaptionTemplate::Clean).unwrap_or_else(|e| panic!("{}: {e}", f.name));
        }
    }

    #[test]
    fn rejects_fonts_outside_system_folders_and_wrong_types() {
        assert!(captions::check_font_path("/etc/passwd").is_err());
        assert!(captions::check_font_path("relative/font.ttf").is_err());
        let tmp = std::env::temp_dir().join("evil.ttf");
        let _ = std::fs::write(&tmp, b"x");
        assert!(captions::check_font_path(&tmp.to_string_lossy()).is_err());
        let _ = std::fs::remove_file(&tmp);
    }
}
