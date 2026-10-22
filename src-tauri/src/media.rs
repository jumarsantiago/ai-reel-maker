//! Local media inspection via FFprobe.
//! Never goes through a shell: ffprobe runs with discrete arguments on a validated path.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const ALLOWED_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "webm", "mkv"];

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    pub path: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub duration_sec: f64,
    pub width: u32,
    pub height: u32,
    pub video_codec: String,
    pub fps: f64,
    pub has_audio: bool,
}

/// Finds a binary in the usual Homebrew locations first, because apps launched
/// from Finder do not inherit the shell PATH.
pub fn find_tool(name: &str) -> Option<PathBuf> {
    for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        let p = Path::new(dir).join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

pub fn validate_media_path(path: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(path);
    if !p.is_absolute() {
        return Err("Path must be absolute.".into());
    }
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !ALLOWED_EXTENSIONS.contains(&ext.as_str()) {
        return Err(format!(
            "Unsupported file type '.{ext}'. Allowed: {}.",
            ALLOWED_EXTENSIONS.join(", ")
        ));
    }
    if !p.is_file() {
        return Err(format!("File not found: {path}"));
    }
    Ok(p)
}

#[derive(Deserialize)]
struct ProbeOutput {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Deserialize)]
struct ProbeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
}

#[derive(Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
    size: Option<String>,
}

fn parse_rate(rate: &str) -> f64 {
    match rate.split_once('/') {
        Some((n, d)) => {
            let (n, d) = (n.parse::<f64>().unwrap_or(0.0), d.parse::<f64>().unwrap_or(0.0));
            if d > 0.0 { n / d } else { 0.0 }
        }
        None => rate.parse().unwrap_or(0.0),
    }
}

/// Parses `ffprobe -print_format json -show_format -show_streams` output.
pub fn parse_probe_json(path: &Path, json: &str) -> Result<MediaInfo, String> {
    let out: ProbeOutput =
        serde_json::from_str(json).map_err(|e| format!("Could not read ffprobe output: {e}"))?;
    let video = out
        .streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("video"))
        .ok_or("File has no video stream.")?;
    let format = out.format.as_ref();
    let duration_sec = format
        .and_then(|f| f.duration.as_deref())
        .and_then(|d| d.parse::<f64>().ok())
        .unwrap_or(0.0);
    if duration_sec <= 0.0 {
        return Err("Could not determine video duration.".into());
    }
    Ok(MediaInfo {
        path: path.to_string_lossy().into_owned(),
        file_name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        size_bytes: format
            .and_then(|f| f.size.as_deref())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        duration_sec,
        width: video.width.unwrap_or(0),
        height: video.height.unwrap_or(0),
        video_codec: video.codec_name.clone().unwrap_or_default(),
        fps: video.avg_frame_rate.as_deref().map(parse_rate).unwrap_or(0.0),
        has_audio: out
            .streams
            .iter()
            .any(|s| s.codec_type.as_deref() == Some("audio")),
    })
}

pub fn probe(path: &str) -> Result<MediaInfo, String> {
    let p = validate_media_path(path)?;
    let ffprobe = find_tool("ffprobe")
        .ok_or("FFprobe not found. Install FFmpeg (e.g. `brew install ffmpeg`).")?;
    let output = Command::new(ffprobe)
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(&p)
        .output()
        .map_err(|e| format!("Failed to run ffprobe: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffprobe could not read this file: {}", err.trim()));
    }
    parse_probe_json(&p, &String::from_utf8_lossy(&output.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "streams": [
        {"codec_type": "video", "codec_name": "h264", "width": 1920, "height": 1080, "avg_frame_rate": "30000/1001"},
        {"codec_type": "audio", "codec_name": "aac"}
      ],
      "format": {"duration": "12.500000", "size": "1048576"}
    }"#;

    #[test]
    fn parses_video_and_audio() {
        let info = parse_probe_json(Path::new("/tmp/a.mp4"), SAMPLE).unwrap();
        assert_eq!(info.file_name, "a.mp4");
        assert_eq!((info.width, info.height), (1920, 1080));
        assert_eq!(info.video_codec, "h264");
        assert!((info.fps - 29.97).abs() < 0.01);
        assert!((info.duration_sec - 12.5).abs() < 1e-9);
        assert_eq!(info.size_bytes, 1_048_576);
        assert!(info.has_audio);
    }

    #[test]
    fn rejects_audio_only() {
        let json = r#"{"streams":[{"codec_type":"audio"}],"format":{"duration":"3"}}"#;
        assert!(parse_probe_json(Path::new("/tmp/a.mp4"), json).is_err());
    }

    #[test]
    fn rejects_zero_duration() {
        let json = r#"{"streams":[{"codec_type":"video","width":2,"height":2}],"format":{}}"#;
        assert!(parse_probe_json(Path::new("/tmp/a.mp4"), json).is_err());
    }

    #[test]
    fn validates_paths() {
        assert!(validate_media_path("relative.mp4").is_err());
        assert!(validate_media_path("/tmp/notes.txt").is_err());
        assert!(validate_media_path("/definitely/missing/file.mp4").is_err());
    }

    /// Integration: generates a real 2s clip with local FFmpeg and probes it.
    #[test]
    fn probes_real_generated_clip() {
        let Some(ffmpeg) = find_tool("ffmpeg") else {
            eprintln!("skipped: ffmpeg not installed");
            return;
        };
        let dir = std::env::temp_dir().join(format!("arm-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("probe-test.mp4");
        let status = Command::new(ffmpeg)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25:duration=2"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .unwrap();
        assert!(status.success());
        let info = probe(clip.to_str().unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!((info.width, info.height), (320, 240));
        assert!((info.duration_sec - 2.0).abs() < 0.1);
        assert!(!info.has_audio);
        assert_eq!(info.video_codec, "h264");
    }

    #[test]
    fn parses_rates() {
        assert_eq!(parse_rate("25/1"), 25.0);
        assert_eq!(parse_rate("0/0"), 0.0);
        assert_eq!(parse_rate("24"), 24.0);
    }
}
