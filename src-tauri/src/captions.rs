//! Caption rendering without libass/freetype in FFmpeg: each caption is rasterized here into a
//! transparent PNG (white text, black outline, word-wrapped) and later overlaid by FFmpeg.
//! Also writes a standard .srt sidecar.

use crate::plan::ValidCaption;
use ab_glyph::{point, Font, FontVec, PxScale, ScaleFont};
use std::path::{Path, PathBuf};

pub const DEFAULT_FONT: &str = "/System/Library/Fonts/Supplemental/Arial Bold.ttf";
/// Rounded sans used by the soft look (ships with macOS).
pub const SOFT_FONT: &str = "/System/Library/Fonts/Supplemental/Arial Rounded Bold.ttf";

/// Rounded background behind a caption (the soft, aesthetic look).
#[derive(Clone, Copy)]
pub struct Pill {
    pub bg: [u8; 4],
    pub border: [u8; 4],
    pub border_px: f32,
    pub radius: f32,
}

pub struct CaptionStyle {
    pub font_px: f32,
    pub max_width: u32,
    pub outline_px: u32,
    pub padding: u32,
    pub text_rgb: [u8; 3],
    pub highlight_rgb: [u8; 3],
    pub pill: Option<Pill>,
}

pub const HIGHLIGHT_RGB: [u8; 3] = [255, 212, 0];

/// Ready-made caption looks the user can pick from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptionTemplate {
    /// Cream text on a translucent plum pill, peach-rose highlight.
    #[default]
    Soft,
    /// No background: cream text with a thin dark outline.
    Clean,
    /// Light blush pill with plum text and a rose highlight.
    Blush,
    /// Black translucent pill, white text, warm yellow highlight.
    Dark,
    /// White text with a heavy black outline and a yellow highlight (the original look).
    Classic,
}

impl CaptionTemplate {
    pub fn style(self, frame_width: u32) -> CaptionStyle {
        let s = frame_width as f32 / 1080.0;
        match self {
            CaptionTemplate::Soft => CaptionStyle::reels_soft(frame_width),
            CaptionTemplate::Classic => CaptionStyle::reels(frame_width),
            CaptionTemplate::Clean => CaptionStyle {
                font_px: 92.0 * s,
                max_width: (frame_width as f32 * 0.88) as u32,
                outline_px: (7.0 * s).round().max(2.0) as u32,
                padding: (12.0 * s) as u32,
                text_rgb: [255, 250, 245],
                highlight_rgb: [255, 205, 160],
                pill: None,
            },
            CaptionTemplate::Blush => CaptionStyle {
                font_px: 80.0 * s,
                max_width: (frame_width as f32 * 0.86) as u32,
                outline_px: 0,
                padding: (30.0 * s) as u32,
                text_rgb: [88, 52, 68],
                highlight_rgb: [190, 60, 100],
                pill: Some(Pill { bg: [255, 238, 240, 238], border: [214, 140, 160, 230], border_px: 3.0 * s, radius: 46.0 * s }),
            },
            CaptionTemplate::Dark => CaptionStyle {
                font_px: 82.0 * s,
                max_width: (frame_width as f32 * 0.86) as u32,
                outline_px: 0,
                padding: (28.0 * s) as u32,
                text_rgb: [255, 255, 255],
                highlight_rgb: [255, 214, 120],
                pill: Some(Pill { bg: [0, 0, 0, 160], border: [255, 255, 255, 50], border_px: 2.0 * s, radius: 30.0 * s }),
            },
        }
    }
}

impl CaptionStyle {
    /// Bigger, bolder "Reels" captions (1-3 words) for a vertical frame.
    pub fn reels(frame_width: u32) -> Self {
        let s = frame_width as f32 / 1080.0;
        Self {
            font_px: 96.0 * s,
            max_width: (frame_width as f32 * 0.88) as u32,
            outline_px: (9.0 * s).round().max(2.0) as u32,
            padding: (12.0 * s) as u32,
            text_rgb: [255, 255, 255],
            highlight_rgb: HIGHLIGHT_RGB,
            pill: None,
        }
    }

    /// Soft, aesthetic captions: cream text on a translucent plum pill with a thin cream edge and a
    /// peach-rose highlight. Readable on light and dark footage, without a heavy outline.
    pub fn reels_soft(frame_width: u32) -> Self {
        let s = frame_width as f32 / 1080.0;
        Self {
            font_px: 88.0 * s,
            max_width: (frame_width as f32 * 0.86) as u32,
            outline_px: 0,
            padding: (30.0 * s) as u32,
            text_rgb: [255, 248, 242],
            highlight_rgb: [255, 190, 165],
            pill: Some(Pill { bg: [88, 52, 68, 208], border: [255, 240, 235, 150], border_px: 2.5 * s, radius: 44.0 * s }),
        }
    }

    /// Big hook text for the first seconds: white on a deep rose pill.
    pub fn hook(frame_width: u32) -> Self {
        let s = frame_width as f32 / 1080.0;
        Self {
            font_px: 70.0 * s,
            max_width: (frame_width as f32 * 0.88) as u32,
            outline_px: 0,
            padding: (32.0 * s) as u32,
            text_rgb: [255, 255, 255],
            highlight_rgb: [255, 255, 255],
            pill: Some(Pill { bg: [176, 68, 98, 235], border: [255, 240, 235, 190], border_px: 3.0 * s, radius: 46.0 * s }),
        }
    }

    /// Small feature tag: plum text on a cream pill.
    pub fn tag(frame_width: u32) -> Self {
        let s = frame_width as f32 / 1080.0;
        Self {
            font_px: 46.0 * s,
            max_width: (frame_width as f32 * 0.86) as u32,
            outline_px: 0,
            padding: (22.0 * s) as u32,
            text_rgb: [88, 52, 68],
            highlight_rgb: [88, 52, 68],
            pill: Some(Pill { bg: [255, 244, 236, 238], border: [214, 160, 150, 220], border_px: 2.5 * s, radius: 36.0 * s }),
        }
    }

    /// Closing call to action: bigger plum text on a cream pill with a rose edge.
    pub fn cta(frame_width: u32) -> Self {
        let s = frame_width as f32 / 1080.0;
        Self {
            font_px: 58.0 * s,
            max_width: (frame_width as f32 * 0.86) as u32,
            outline_px: 0,
            padding: (30.0 * s) as u32,
            text_rgb: [88, 52, 68],
            highlight_rgb: [88, 52, 68],
            pill: Some(Pill { bg: [255, 244, 236, 245], border: [176, 68, 98, 255], border_px: 4.0 * s, radius: 44.0 * s }),
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
            text_rgb: [255, 255, 255],
            highlight_rgb: HIGHLIGHT_RGB,
            pill: None,
        }
    }
}

pub fn load_font() -> Result<FontVec, String> {
    load_font_for(DEFAULT_FONT)
}

/// Folders a caption font may be picked from (system and user fonts only, so a bad path cannot read other files).
pub fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/System/Library/Fonts"), PathBuf::from("/Library/Fonts")];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join("Library/Fonts"));
    }
    dirs
}

/// Accepts only an absolute font file (ttf/otf/ttc/otc) inside one of [`font_dirs`].
pub fn check_font_path(path: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(path);
    let ext = p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    if !p.is_absolute() || !["ttf", "otf", "ttc", "otc"].contains(&ext.as_str()) {
        return Err("Pick a font file (.ttf, .otf or .ttc) from the font list.".into());
    }
    let real = std::fs::canonicalize(&p).map_err(|e| format!("Font not found: {path} ({e})"))?;
    if !font_dirs().iter().filter_map(|d| std::fs::canonicalize(d).ok()).any(|d| real.starts_with(d)) {
        return Err("Fonts must come from the system or user Fonts folders.".into());
    }
    Ok(real)
}

/// The font chosen in the app (if any), else `AIRM_CAPTION_FONT`, else `default_path`.
pub fn load_font_choice(choice: Option<&str>, default_path: &str) -> Result<FontVec, String> {
    match choice.map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => {
            let path = check_font_path(c)?;
            let bytes = std::fs::read(&path).map_err(|e| format!("Cannot read font {}: {e}", path.display()))?;
            FontVec::try_from_vec(bytes).map_err(|_| format!("Cannot use font {}", path.display()))
        }
        None => load_font_for(default_path),
    }
}

/// `AIRM_CAPTION_FONT` always wins; otherwise `default_path`.
pub fn load_font_for(default_path: &str) -> Result<FontVec, String> {
    let path = std::env::var_os("AIRM_CAPTION_FONT").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(default_path));
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

/// Colour and alpha (0..1) of the rounded pill at pixel centre (x, y) of a w x h image.
fn pill_pixel(p: &Pill, x: f32, y: f32, w: f32, h: f32) -> (f32, f32, f32, f32) {
    let (hx, hy) = (w / 2.0, h / 2.0);
    let r = p.radius.min(hx).min(hy);
    let (qx, qy) = ((x - hx).abs() - (hx - r), (y - hy).abs() - (hy - r));
    let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
    let cover = (0.5 - d).clamp(0.0, 1.0);
    let ring = ((d + p.border_px + 0.5).clamp(0.0, 1.0)) * cover;
    let fa = p.bg[3] as f32 / 255.0 * cover;
    let ba = p.border[3] as f32 / 255.0 * ring;
    let oa = ba + fa * (1.0 - ba);
    if oa <= 0.0 {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let c = |k: usize| (p.border[k] as f32 * ba + p.bg[k] as f32 * fa * (1.0 - ba)) / oa;
    (c(0), c(1), c(2), oa)
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
        // Glyph colour/coverage: text colour where white glyphs are, highlight colour where highlighted.
        let ga = mask[i];
        let (wf, yf) = if ga > 0.0 { (white[i] / ga, yellow[i] / ga) } else { (0.0, 0.0) };
        let gc = |k: usize| style.text_rgb[k] as f32 * wf + style.highlight_rgb[k] as f32 * yf;
        let (br, bg, bb, ba) = match &style.pill {
            Some(p) => pill_pixel(p, (i % w) as f32 + 0.5, (i / w) as f32 + 0.5, w as f32, h as f32),
            None => (0.0, 0.0, 0.0, 0.0),
        };
        if style.pill.is_some() {
            let oa = ga + ba * (1.0 - ga);
            if oa <= 0.0 {
                *px = image::Rgba([0, 0, 0, 0]);
            } else {
                let mix = |g: f32, b: f32| ((g * ga + b * ba * (1.0 - ga)) / oa).clamp(0.0, 255.0).round() as u8;
                *px = image::Rgba([mix(gc(0), br), mix(gc(1), bg), mix(gc(2), bb), (oa * 255.0).round() as u8]);
            }
            continue;
        }
        let a = outline[i].max(ga);
        if a <= 0.0 {
            *px = image::Rgba([0, 0, 0, 0]);
            continue;
        }
        // Straight alpha: glyph colour where the glyph is, black outline around it.
        let ch = |k: usize| ((style.text_rgb[k] as f32 * (white[i] / a) + style.highlight_rgb[k] as f32 * (yellow[i] / a)).min(255.0)).round() as u8;
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
    fn every_template_renders_and_only_clean_and_classic_have_no_pill() {
        let Ok(font) = load_font() else {
            eprintln!("skipped: caption font missing");
            return;
        };
        let dir = std::env::temp_dir().join(format!("arm-templates-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for t in [CaptionTemplate::Soft, CaptionTemplate::Clean, CaptionTemplate::Blush, CaptionTemplate::Dark, CaptionTemplate::Classic] {
            let style = t.style(1080);
            assert_eq!(style.pill.is_none(), matches!(t, CaptionTemplate::Clean | CaptionTemplate::Classic), "{t:?}");
            let p = dir.join(format!("{t:?}.png"));
            let (w, h) = render_words_png(&font, &style, "Sobrang dali", Some(1), &p).unwrap();
            let img = image::open(&p).unwrap().to_rgba8();
            assert_eq!((img.width(), img.height()), (w, h));
            // The very corner is see-through in every look (rounded pill, or no box at all).
            assert_eq!(img.get_pixel(0, 0).0[3], 0, "{t:?}");
            // A pill look paints the middle-left edge; a no-background look leaves most of the frame clear.
            let opaque = img.pixels().filter(|px| px.0[3] > 100).count() as f32 / (w * h) as f32;
            if style.pill.is_some() {
                assert!(opaque > 0.5, "{t:?} {opaque}");
            } else {
                assert!(opaque < 0.5, "{t:?} {opaque}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
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
