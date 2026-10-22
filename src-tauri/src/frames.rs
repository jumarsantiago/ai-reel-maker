//! Small grayscale frame samples from a video (via FFmpeg) and simple picture measurements, used to pick a
//! sharp, steady stretch of b-roll and the best frame for a thumbnail.

use crate::media;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

/// Grayscale frames sampled at `fps` from `start` for `len` seconds, scaled to `width` pixels wide.
pub struct Samples {
    pub width: usize,
    pub height: usize,
    pub fps: f64,
    pub start: f64,
    pub frames: Vec<Vec<u8>>,
}

pub fn sample_gray(video: &Path, start: f64, len: f64, fps: f64, width: u32, src_w: u32, src_h: u32) -> Result<Samples, String> {
    let ffmpeg = media::find_tool("ffmpeg").ok_or("FFmpeg not found.")?;
    let w = (width.max(16) / 2 * 2) as usize;
    let h = if src_w == 0 { w * 9 / 16 } else { ((w as f64 * src_h as f64 / src_w as f64).round() as usize).max(2) / 2 * 2 };
    let mut child = Command::new(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-v", "error", "-ss", &format!("{start:.3}"), "-t", &format!("{len:.3}"), "-i"])
        .arg(video)
        .args(["-an", "-vf", &format!("fps={fps:.4},scale={w}:{h},format=gray"), "-f", "rawvideo", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to start FFmpeg: {e}"))?;
    let mut buf = Vec::new();
    child.stdout.take().expect("piped").read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let _ = child.wait();
    let frames: Vec<Vec<u8>> = buf.chunks_exact(w * h).map(|c| c.to_vec()).collect();
    if frames.is_empty() {
        return Err("Could not read any frames from the video.".into());
    }
    Ok(Samples { width: w, height: h, fps, start, frames })
}

/// Mean absolute Laplacian: higher = sharper (blurry or out-of-focus frames score low).
pub fn sharpness(f: &[u8], w: usize, h: usize) -> f64 {
    if w < 3 || h < 3 {
        return 0.0;
    }
    let mut sum = 0u64;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let c = f[y * w + x] as i32 * 4;
            let n = f[(y - 1) * w + x] as i32 + f[(y + 1) * w + x] as i32 + f[y * w + x - 1] as i32 + f[y * w + x + 1] as i32;
            sum += (c - n).unsigned_abs() as u64;
        }
    }
    sum as f64 / ((w - 2) * (h - 2)) as f64
}

/// Mean absolute difference between two frames: higher = more movement (shake, fast pans).
pub fn motion(a: &[u8], b: &[u8]) -> f64 {
    let n = a.len().min(b.len()).max(1);
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as u64).sum::<u64>() as f64 / n as f64
}

/// Mean brightness, 0-255.
pub fn brightness(f: &[u8]) -> f64 {
    f.iter().map(|&v| v as u64).sum::<u64>() as f64 / f.len().max(1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurements_rank_sharp_still_and_bright_frames() {
        let (w, h) = (8, 8);
        let flat = vec![128u8; w * h];
        let checker: Vec<u8> = (0..w * h).map(|i| if (i / w + i % w) % 2 == 0 { 0 } else { 255 }).collect();
        assert_eq!(sharpness(&flat, w, h), 0.0);
        assert!(sharpness(&checker, w, h) > 500.0);
        assert_eq!(motion(&flat, &flat), 0.0);
        assert!(motion(&flat, &checker) > 100.0);
        assert_eq!(brightness(&flat), 128.0);
    }
}
