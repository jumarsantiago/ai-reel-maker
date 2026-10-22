//! Caption rendering without libass/freetype in FFmpeg: each caption is rasterized here into a
//! transparent PNG (white text, black outline, word-wrapped) and later overlaid by FFmpeg.
//! Also writes a standard .srt sidecar.

use crate::plan::ValidCaption;
use ab_glyph::{point, Font, FontVec, PxScale, ScaleFont};
use std::path::{Path, PathBuf};

pub const DEFAULT_FONT: &str = "/System/Library/Fonts/Supplemental/Arial Bold.ttf";

pub struct CaptionStyle {
    pub font_px: f32,
    pub max_width: u32,
    pub outline_px: u32,
    pub padding: u32,
}

pub const HIGHLIGHT_RGB: [u8; 3] = [255, 212, 0];

impl CaptionStyle {
    /// Bigger, bolder "Reels" captions (1-3 words) for a vertical frame.
    pub fn reels(frame_width: u32) -> Self {
        let s = frame_width as f32 / 1080.0;
        Self {
            font_px: 96.0 * s,
            max_width: (frame_width as f32 * 0.88) as u32,
            outline_px: (9.0 * s).round().max(2.0) as u32,
            padding: (12.0 * s) as u32,
        }
    }

    /// Sized for a 1080-wide vertical frame.
    pub fn for_width(frame_width: u32) -> Self {
        let s = frame_width as f32 / 1080.0;
        Self {
            font_px: 76.0 * s,
            max_width: (frame_width as f32 * 0.86) as u32,
            outline_px: (7.0 * s).round().max(2.0) as u32,
            padding: (12.0 * s) as u32,
        }
    }
}

pub fn load_font() -> Result<FontVec, String> {
    let path = std::env::var_os("AIRM_CAPTION_FONT").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(DEFAULT_FONT));
    let bytes = std::fs::read(&path).map_err(|e| format!("Cannot read caption font {}: {e}", path.display()))?;
    FontVec::try_from_vec(bytes).map_err(|_| format!("Invalid font file {}", path.display()))
}

fn text_width(font: &FontVec, scale: PxScale, text: &str) -> f32 {
    let sf = font.as_scaled(scale);
    let mut w = 0.0;
    let mut prev = None;
    for c in text.chars() {
        let id = sf.glyph_id(c);
        if let Some(p) = prev {
            w += sf.kern(p, id);
        }
        w += sf.h_advance(id);
        prev = Some(id);
    }
    w
}

/// Greedy word wrap to `max` pixels; a single over-long word gets its own line.
pub fn wrap(font: &FontVec, scale: PxScale, text: &str, max: f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let candidate = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if !cur.is_empty() && text_width(font, scale, &candidate) > max {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
        } else {
            cur = candidate;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Separable square max-filter: grows the glyph mask to make the outline.
fn dilate(mask: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut tmp = vec![0.0f32; mask.len()];
    for y in 0..h {
        for x in 0..w {
            let (a, b) = (x.saturating_sub(r), (x + r).min(w - 1));
            tmp[y * w + x] = (a..=b).map(|i| mask[y * w + i]).fold(0.0, f32::max);
        }
    }
    let mut out = vec![0.0f32; mask.len()];
    for y in 0..h {
        let (a, b) = (y.saturating_sub(r), (y + r).min(h - 1));
        for x in 0..w {
            out[y * w + x] = (a..=b).map(|j| tmp[j * w + x]).fold(0.0, f32::max);
        }
    }
    out
}

/// Renders one caption to an RGBA PNG; returns its pixel size.
pub fn render_png(font: &FontVec, style: &CaptionStyle, text: &str, out: &Path) -> Result<(u32, u32), String> {
    render_words_png(font, style, text, None, out)
}

/// Renders a caption with word `highlight` (0-based, by whitespace-separated word) in yellow.
pub fn render_words_png(font: &FontVec, style: &CaptionStyle, text: &str, highlight: Option<usize>, out: &Path) -> Result<(u32, u32), String> {
    let scale = PxScale::from(style.font_px);
    let sf = font.as_scaled(scale);
    let inner = style.max_width.saturating_sub(2 * (style.padding + style.outline_px)) as f32;
    let lines = wrap(font, scale, text, inner);
    if lines.is_empty() {
        return Err("Caption text is empty.".into());
    }
    let line_h = (sf.ascent() - sf.descent() + sf.line_gap()).ceil();
    let margin = (style.padding + style.outline_px) as f32;
    let widest = lines.iter().map(|l| text_width(font, scale, l)).fold(0.0, f32::max);
    let w = (widest + 2.0 * margin).ceil().max(1.0) as usize;
    let h = (line_h * lines.len() as f32 + 2.0 * margin).ceil() as usize;

    // Two coverage masks: normal (white) glyphs and highlighted (yellow) glyphs.
    let mut white = vec![0.0f32; w * h];
    let mut yellow = vec![0.0f32; w * h];
    let mut word_index = 0usize;
    for (i, line) in lines.iter().enumerate() {
        let lw = text_width(font, scale, line);
        let mut x = (w as f32 - lw) / 2.0;
        let baseline = margin + sf.ascent() + line_h * i as f32;
        let mut prev = None;
        let mut in_word = false;
        for c in line.chars() {
            if c.is_whitespace() {
                if in_word {
                    word_index += 1;
                }
                in_word = false;
            } else {
                in_word = true;
            }
            let target = if highlight == Some(word_index) && in_word { &mut yellow } else { &mut white };
            let id = sf.glyph_id(c);
            if let Some(p) = prev {
                x += sf.kern(p, id);
            }
            let glyph = id.with_scale_and_position(scale, point(x, baseline));
            if let Some(og) = font.outline_glyph(glyph) {
                let b = og.px_bounds();
                og.draw(|gx, gy, cov| {
                    let px = b.min.x as i32 + gx as i32;
                    let py = b.min.y as i32 + gy as i32;
                    if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                        let m = &mut target[py as usize * w + px as usize];
                        *m = m.max(cov);
                    }
                });
            }
            x += sf.h_advance(id);
            prev = Some(id);
        }
        if in_word {
            word_index += 1;
        }
    }
    let mask: Vec<f32> = white.iter().zip(&yellow).map(|(a, b)| a.max(*b)).collect();
    let outline = dilate(&mask, w, h, style.outline_px as usize);
    let mut img = image::RgbaImage::new(w as u32, h as u32);
    for (i, px) in img.pixels_mut().enumerate() {
        let a = outline[i].max(mask[i]);
        if a <= 0.0 {
            *px = image::Rgba([0, 0, 0, 0]);
            continue;
        }
        // Straight alpha: glyph colour where the glyph is, black outline around it.
        let (wf, yf) = (white[i] / a, yellow[i] / a);
        let ch = |k: usize| ((255.0 * wf + HIGHLIGHT_RGB[k] as f32 * yf).min(255.0)).round() as u8;
        *px = image::Rgba([ch(0), ch(1), ch(2), (a * 255.0).round() as u8]);
    }
    img.save(out).map_err(|e| format!("Cannot write caption image: {e}"))?;
    Ok((w as u32, h as u32))
}

fn srt_time(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

pub fn to_srt(captions: &[ValidCaption]) -> String {
    captions
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{}\n{} --> {}\n{}\n", i + 1, srt_time(c.out_start), srt_time(c.out_end), c.text))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srt_format() {
        let caps = vec![
            ValidCaption { text: "Hi everyone".into(), out_start: 0.0, out_end: 1.25 },
            ValidCaption { text: "Salamat!".into(), out_start: 61.5, out_end: 62.0 },
        ];
        assert_eq!(to_srt(&caps), "1\n00:00:00,000 --> 00:00:01,250\nHi everyone\n\n2\n00:01:01,500 --> 00:01:02,000\nSalamat!\n");
    }

    #[test]
    fn renders_wrapped_png_with_alpha() {
        let Ok(font) = load_font() else {
            eprintln!("skipped: caption font missing");
            return;
        };
        let style = CaptionStyle::for_width(1080);
        let scale = PxScale::from(style.font_px);
        let lines = wrap(&font, scale, "Sobrang dali lang nito, promise talaga mga ka-reels!", 600.0);
        assert!(lines.len() >= 2, "{lines:?}");

        let dir = std::env::temp_dir().join(format!("arm-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("c.png");
        let (w, h) = render_png(&font, &style, "Sobrang dali lang nito, promise!", &p).unwrap();
        assert!(w <= style.max_width && h > 0);
        let img = image::open(&p).unwrap().to_rgba8();
        let alphas: Vec<u8> = img.pixels().map(|p| p[3]).collect();
        assert!(alphas.iter().any(|&a| a == 255), "has opaque text/outline");
        assert!(alphas.iter().any(|&a| a == 0), "has transparent background");
        assert!(img.pixels().any(|p| p[0] == 255 && p[3] == 255), "has white fill");

        // Highlighting word 1 ("dali") adds yellow pixels; no highlight has none.
        let q = dir.join("h.png");
        render_words_png(&font, &CaptionStyle::reels(1080), "Sobrang dali", Some(1), &q).unwrap();
        let img = image::open(&q).unwrap().to_rgba8();
        let yellowish = |p: &image::Rgba<u8>| p[3] == 255 && p[0] > 240 && p[1] > 190 && p[2] < 40;
        assert!(img.pixels().any(yellowish), "highlighted word is yellow");
        let left_half_yellow = img.enumerate_pixels().filter(|(x, _, p)| *x < img.width() / 3 && yellowish(p)).count();
        assert_eq!(left_half_yellow, 0, "first word stays white");
        render_words_png(&font, &CaptionStyle::reels(1080), "Sobrang dali", None, &q).unwrap();
        assert!(!image::open(&q).unwrap().to_rgba8().pixels().any(yellowish));
        std::fs::remove_dir_all(&dir).ok();
    }
}
