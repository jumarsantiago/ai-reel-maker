//! Cover image for a reel: the sharpest, well-lit frame of the finished video with the hook text on it.

use crate::captions::{self, CaptionStyle};
use crate::frames;
use crate::media;
use std::path::{Path, PathBuf};

/// Time (seconds) of the best-looking frame: sharp and neither too dark nor too bright, avoiding the very
/// start and end (fades, titles).
pub fn best_frame_time(video: &Path) -> Result<f64, String> {
    let info = media::probe(&video.to_string_lossy())?;
    let d = info.duration_sec;
    if d <= 1.0 {
        return Ok(d / 2.0);
    }
    let (start, len) = (d * 0.1, d * 0.8);
    let fps = (24.0 / len).clamp(0.05, 4.0);
    let s = frames::sample_gray(video, start, len, fps, 192, info.width, info.height)?;
    let sharp: Vec<f64> = s.frames.iter().map(|f| frames::sharpness(f, s.width, s.height)).collect();
    let max_sharp = sharp.iter().cloned().fold(1e-9, f64::max);
    let best = s
        .frames
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let light = 1.0 - ((frames::brightness(f) - 128.0).abs() / 128.0);
            (i, sharp[i] / max_sharp * 0.7 + light * 0.3)
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
        .unwrap_or(0);
    Ok(start + best as f64 / fps)
}

/// The next free `thumbnail.png` / `thumbnail-2.png` / ... in `dir`.
pub fn next_thumbnail_path(dir: &Path) -> PathBuf {
    (1..)
        .map(|n| if n == 1 { dir.join("thumbnail.png") } else { dir.join(format!("thumbnail-{n}.png")) })
        .find(|p| !p.exists())
        .expect("unbounded")
}

/// Saves the frame of `video` at `t` seconds as a PNG.
pub fn grab_frame(video: &Path, t: f64, out: &Path) -> Result<(), String> {
    let ffmpeg = media::find_tool("ffmpeg").ok_or("FFmpeg not found.")?;
    let ok = std::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-v", "error", "-ss", &format!("{t:.3}"), "-i"])
        .arg(video)
        .args(["-frames:v", "1", "-y"])
        .arg(out)
        .status()
        .map_err(|e| e.to_string())?
        .success();
    if ok && out.exists() { Ok(()) } else { Err("Could not grab a frame for the thumbnail.".into()) }
}

/// Draws `text` as a big title on the image at `png` (in place).
pub fn add_title(png: &Path, text: &str, font: Option<&str>) -> Result<(), String> {
    let mut frame = image::open(png).map_err(|e| e.to_string())?.to_rgba8();
    let (w, h) = frame.dimensions();
    let mut style = CaptionStyle::hook(w.min(h));
    style.font_px *= 1.45;
    style.max_width = (w as f32 * 0.9) as u32;
    let font = captions::load_font_choice(font, captions::SOFT_FONT)?;
    let tmp = png.with_file_name(".thumbnail-title.png");
    let res = captions::render_png(&font, &style, text, &tmp);
    let title = image::open(&tmp).map(|i| i.to_rgba8());
    let _ = std::fs::remove_file(&tmp);
    res?;
    let title = title.map_err(|e| e.to_string())?;
    let x = (w as i64 - title.width() as i64) / 2;
    let y_center = if h > w { 0.74 } else { 0.70 };
    let y = (h as f64 * y_center) as i64 - title.height() as i64 / 2;
    image::imageops::overlay(&mut frame, &title, x, y.max(0));
    frame.save(png).map_err(|e| format!("Cannot save thumbnail: {e}"))
}

/// Writes a thumbnail next to `video` from its best frame, with `text` (if any) as a big title.
pub fn make_thumbnail(video: &Path, text: Option<&str>, font: Option<&str>) -> Result<PathBuf, String> {
    let dir = video.parent().ok_or("Video has no folder")?;
    let out = next_thumbnail_path(dir);
    grab_frame(video, best_frame_time(video)?, &out)?;
    if let Some(text) = text.map(str::trim).filter(|t| !t.is_empty()) {
        add_title(&out, text, font)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A video that is blurry for the first half and sharp for the second: the cover comes from the sharp part.
    #[test]
    fn picks_a_sharp_frame_and_draws_the_title() {
        let Some(ffmpeg) = media::find_tool("ffmpeg") else { return };
        let dir = std::env::temp_dir().join(format!("arm-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let v = dir.join("v.mp4");
        assert!(std::process::Command::new(&ffmpeg)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25:duration=10",
                   "-vf", "boxblur=enable='lt(t,5)':luma_radius=20:luma_power=3", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&v)
            .status()
            .unwrap()
            .success());
        let t = best_frame_time(&v).unwrap();
        assert!(t > 5.0, "picked {t}s, inside the blurred half");
        let out = make_thumbnail(&v, Some("SIARGAO DAY 1"), None).unwrap();
        let img = image::open(&out).unwrap();
        assert_eq!((img.width(), img.height()), (640, 360));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
