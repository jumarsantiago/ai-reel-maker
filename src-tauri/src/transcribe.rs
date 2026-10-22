//! Local transcription: FFmpeg audio extraction + silencedetect, then whisper.cpp on speech chunks.
//! Splitting on real silences keeps Whisper's word timestamps from drifting across long pauses
//! (whisper.cpp otherwise smears pauses into neighbouring words). Everything runs locally.

use crate::media;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Silence must be quieter than this (dBFS) and last at least `SILENCE_MIN_SEC`.
pub const SILENCE_NOISE_DB: i32 = -35;
pub const SILENCE_MIN_SEC: f64 = 0.35;
/// Padding kept around each speech chunk so word onsets/offsets aren't clipped.
const CHUNK_PAD_SEC: f64 = 0.1;
const CHUNK_MIN_SEC: f64 = 0.2;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Word {
    pub id: usize,
    pub text: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Interval {
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub source_id: String,
    pub duration_sec: f64,
    /// Language requested (`auto` or an ISO code) and what Whisper detected per chunk.
    pub language_requested: String,
    pub languages_detected: Vec<String>,
    pub words: Vec<Word>,
    pub silences: Vec<Interval>,
    pub speech_chunks: Vec<Interval>,
    pub whisper_model: String,
    /// How word edges were placed: "dtw+energy" (whisper DTW anchors, cut at audio minima)
    /// or "offsets+energy" (no DTW preset for this model).
    #[serde(default)]
    pub timing_method: String,
}

pub fn default_model_path() -> PathBuf {
    if let Some(p) = std::env::var_os("AIRM_WHISPER_MODEL") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let dir = home.join("Library/Application Support/ai-reel-maker/models");
    // Best installed model first: large-v3 spells Taglish much better than small.
    ["ggml-large-v3.bin", "ggml-large-v3-turbo.bin", "ggml-medium.bin", "ggml-small.bin"]
        .iter()
        .map(|f| dir.join(f))
        .find(|p| p.exists())
        .unwrap_or_else(|| dir.join("ggml-small.bin"))
}

/// Parses FFmpeg `silencedetect` stderr. A trailing open silence runs to `duration`.
pub fn parse_silences(stderr: &str, duration: f64) -> Vec<Interval> {
    let mut out = Vec::new();
    let mut open: Option<f64> = None;
    for line in stderr.lines() {
        let value = |key: &str| {
            line.find(key).and_then(|i| {
                line[i + key.len()..].split_whitespace().next().and_then(|v| v.trim_end_matches('|').parse::<f64>().ok())
            })
        };
        if let Some(s) = value("silence_start: ") {
            open = Some(s.max(0.0));
        } else if let Some(e) = value("silence_end: ") {
            if let Some(s) = open.take() {
                out.push(Interval { start: s, end: e.min(duration) });
            }
        }
    }
    if let Some(s) = open {
        out.push(Interval { start: s, end: duration });
    }
    out
}

/// Speech regions = complement of silences, padded and with tiny fragments dropped.
pub fn speech_chunks(silences: &[Interval], duration: f64) -> Vec<Interval> {
    let mut chunks = Vec::new();
    let mut t = 0.0;
    let ends = silences.iter().copied().chain(std::iter::once(Interval { start: duration, end: duration }));
    for s in ends {
        if s.start - t > CHUNK_MIN_SEC {
            chunks.push(Interval {
                start: (t - CHUNK_PAD_SEC).max(0.0),
                end: (s.start + CHUNK_PAD_SEC).min(duration),
            });
        }
        t = s.end;
    }
    chunks
}

#[derive(Deserialize)]
struct WhisperJson {
    result: Option<WhisperResult>,
    #[serde(default)]
    transcription: Vec<WhisperSegment>,
}
#[derive(Deserialize)]
struct WhisperResult {
    language: Option<String>,
}
#[derive(Deserialize)]
struct WhisperSegment {
    text: String,
    offsets: WhisperOffsets,
    #[serde(default)]
    tokens: Vec<WhisperToken>,
}
#[derive(Deserialize)]
struct WhisperToken {
    text: String,
    #[serde(default = "no_dtw")]
    t_dtw: i64,
}
fn no_dtw() -> i64 {
    -1
}

/// A transcribed word before refinement: whisper's own edges plus DTW anchors (absolute
/// seconds of its first and last non-punctuation token) when available.
#[derive(Debug, Clone, PartialEq)]
pub struct RawWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub anchor_first: Option<f64>,
    pub anchor_last: Option<f64>,
}
#[derive(Deserialize)]
struct WhisperOffsets {
    from: i64,
    to: i64,
}

/// Parses whisper-cli `-ojf -ml 1 -sow [-dtw]` output (one word per segment) for a chunk starting at
/// `chunk.start`, clamping times to the chunk. Returns (language, words).
pub fn parse_whisper_chunk(json: &str, chunk: Interval) -> Result<(String, Vec<RawWord>), String> {
    let parsed: WhisperJson = serde_json::from_str(json).map_err(|e| format!("Bad whisper JSON: {e}"))?;
    let lang = parsed.result.and_then(|r| r.language).unwrap_or_default();
    let mut words = Vec::new();
    for seg in parsed.transcription {
        let text = seg.text.trim();
        if text.is_empty() || text.starts_with('[') {
            continue;
        }
        let start = (chunk.start + seg.offsets.from as f64 / 1000.0).clamp(chunk.start, chunk.end);
        let end = (chunk.start + seg.offsets.to as f64 / 1000.0).clamp(start, chunk.end);
        let anchors: Vec<f64> = seg
            .tokens
            .iter()
            .filter(|t| t.t_dtw >= 0 && !t.text.starts_with('[') && t.text.chars().any(char::is_alphanumeric))
            .map(|t| (chunk.start + t.t_dtw as f64 / 100.0).clamp(chunk.start, chunk.end))
            .collect();
        words.push(RawWord {
            text: text.to_string(),
            start,
            end,
            anchor_first: anchors.first().copied(),
            anchor_last: anchors.last().copied(),
        });
    }
    Ok((lang, words))
}

/// RMS envelope of 16 kHz mono PCM in 10 ms frames, lightly smoothed (3 frames).
pub const FRAME_SEC: f64 = 0.01;
pub fn envelope(samples: &[i16]) -> Vec<f64> {
    let n = (16000.0 * FRAME_SEC) as usize;
    let raw: Vec<f64> = samples
        .chunks(n)
        .map(|c| (c.iter().map(|&x| (x as f64).powi(2)).sum::<f64>() / c.len() as f64).sqrt())
        .collect();
    (0..raw.len())
        .map(|i| {
            let (a, b) = (i.saturating_sub(1), (i + 1).min(raw.len() - 1));
            raw[a..=b].iter().sum::<f64>() / (b - a + 1) as f64
        })
        .collect()
}

/// Amplitude equivalent of SILENCE_NOISE_DB for 16-bit audio.
fn quiet_level() -> f64 {
    32768.0 * 10f64.powf(SILENCE_NOISE_DB as f64 / 20.0)
}

/// Reads the PCM samples from a 16-bit mono WAV written by FFmpeg (walks the RIFF chunks).
pub fn read_wav_samples(path: &Path) -> Result<Vec<i16>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let size = u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
        if id == b"data" {
            let end = (i + 8 + size).min(bytes.len());
            return Ok(bytes[i + 8..end].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect());
        }
        i += 8 + size + (size & 1);
    }
    Err("WAV has no data chunk.".into())
}

/// Places word edges on the audio: each boundary between consecutive words goes to the quietest
/// point between their anchors (a real pause becomes a gap). The first word starts at speech onset
/// and the last ends at speech offset. Without anchors, whisper's own edge midpoints are used.
pub fn refine_words(words: &[RawWord], env: &[f64], chunk: Interval) -> Vec<(String, f64, f64)> {
    if words.is_empty() || env.is_empty() {
        return words.iter().map(|w| (w.text.clone(), w.start, w.end)).collect();
    }
    let quiet = quiet_level();
    let frame = |t: f64| ((t / FRAME_SEC).round().max(0.0) as usize).min(env.len() - 1);
    let time = |f: usize| f as f64 * FRAME_SEC;
    let mid = |w: &RawWord| (w.start + w.end) / 2.0;
    let first_anchor = |w: &RawWord| w.anchor_first.unwrap_or_else(|| mid(w));
    let last_anchor = |w: &RawWord| w.anchor_last.unwrap_or_else(|| mid(w));

    let mut edges: Vec<(f64, f64)> = words.iter().map(|w| (w.start, w.end)).collect();
    // Speech onset/offset of the chunk.
    let (c0, c1) = (frame(chunk.start), frame(chunk.end));
    let onset = (c0..=c1).find(|&f| env[f] > quiet).map(time).unwrap_or(chunk.start);
    let offset = (c0..=c1).rev().find(|&f| env[f] > quiet).map(|f| time(f + 1)).unwrap_or(chunk.end);
    edges[0].0 = onset.min(first_anchor(&words[0]));
    let last = words.len() - 1;
    edges[last].1 = offset.max(last_anchor(&words[last])).min(chunk.end);

    for i in 0..last {
        let (a, b) = (last_anchor(&words[i]), first_anchor(&words[i + 1]));
        let (fa, fb) = (frame(a), frame(b));
        let (end_i, start_next) = if fb > fa + 1 {
            let window = fa + 1..fb;
            // Longest quiet run in the window = the pause; otherwise the single quietest frame.
            let mut best: Option<(usize, usize)> = None;
            let mut run_start = None;
            for f in window.clone() {
                if env[f] <= quiet {
                    let s = *run_start.get_or_insert(f);
                    if best.is_none_or(|(bs, be)| f + 1 - s > be - bs) {
                        best = Some((s, f + 1));
                    }
                } else {
                    run_start = None;
                }
            }
            match best {
                Some((s, e)) => (time(s), time(e)),
                None => {
                    let m = window.min_by(|&x, &y| env[x].total_cmp(&env[y])).unwrap_or(fa);
                    (time(m), time(m))
                }
            }
        } else {
            let m = (a + b) / 2.0;
            (m, m)
        };
        edges[i].1 = end_i;
        edges[i + 1].0 = start_next;
    }
    words
        .iter()
        .zip(edges)
        .map(|(w, (s, e))| (w.text.clone(), s.clamp(chunk.start, chunk.end), e.clamp(s, chunk.end)))
        .collect()
}

/// whisper.cpp DTW presets are named after the model ("small", "base.en", ...).
pub fn dtw_preset(model: &Path) -> Option<String> {
    let name = model.file_name()?.to_string_lossy();
    // whisper.cpp spells the large models "large-v3" in file names but "large.v3" as a DTW preset.
    let preset = name.strip_prefix("ggml-")?.strip_suffix(".bin")?.replace("large-", "large.").replace("-turbo", ".turbo");
    let preset = preset.as_str();
    const KNOWN: &[&str] = &["tiny", "tiny.en", "base", "base.en", "small", "small.en", "medium", "medium.en", "large.v1", "large.v2", "large.v3", "large.v3.turbo"];
    KNOWN.contains(&preset).then(|| preset.to_string())
}

fn run(cmd: &mut Command, what: &str) -> Result<std::process::Output, String> {
    let out = cmd.output().map_err(|e| format!("Failed to run {what}: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(6).collect();
        return Err(format!("{what} failed: {}", tail.into_iter().rev().collect::<Vec<_>>().join("\n")));
    }
    Ok(out)
}

/// Full local transcription of `info.path`. Intermediate WAVs/JSON go in `work_dir`.
pub fn transcribe(info: &media::MediaInfo, work_dir: &Path, model: &Path, language: &str) -> Result<Transcript, String> {
    if !info.has_audio {
        return Err("This video has no audio track to transcribe.".into());
    }
    if !model.is_file() {
        return Err(format!("Whisper model not found at {}", model.display()));
    }
    let lang_ok = language == "auto" || (language.len() == 2 && language.chars().all(|c| c.is_ascii_lowercase()));
    if !lang_ok {
        return Err("Language must be 'auto' or a two-letter code such as 'tl' or 'en'.".into());
    }
    let ffmpeg = media::find_tool("ffmpeg").ok_or("FFmpeg not found.")?;
    let whisper = media::find_tool("whisper-cli").ok_or("whisper-cli not found (brew install whisper-cpp).")?;
    let chunk_dir = work_dir.join("chunks");
    std::fs::create_dir_all(&chunk_dir).map_err(|e| format!("Cannot create work folder: {e}"))?;

    let wav = work_dir.join("audio.wav");
    run(
        Command::new(&ffmpeg)
            .args(["-hide_banner", "-nostdin", "-v", "error", "-y", "-i"])
            .arg(&info.path)
            .args(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
            .arg(&wav),
        "audio extraction",
    )?;

    let sd = run(
        Command::new(&ffmpeg)
            .args(["-hide_banner", "-nostdin", "-nostats", "-i"])
            .arg(&wav)
            .arg("-af")
            .arg(format!("silencedetect=noise={SILENCE_NOISE_DB}dB:d={SILENCE_MIN_SEC}"))
            .args(["-f", "null", "-"]),
        "silence detection",
    )?;
    let silences = parse_silences(&String::from_utf8_lossy(&sd.stderr), info.duration_sec);
    let chunks = speech_chunks(&silences, info.duration_sec);
    if chunks.is_empty() {
        return Err("No speech found (the audio is silent).".into());
    }

    let mut chunk_files = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        let f = chunk_dir.join(format!("c{i:03}.wav"));
        run(
            Command::new(&ffmpeg)
                .args(["-hide_banner", "-nostdin", "-v", "error", "-y"])
                .args(["-ss", &format!("{:.3}", c.start), "-to", &format!("{:.3}", c.end), "-i"])
                .arg(&wav)
                .args(["-c:a", "pcm_s16le"])
                .arg(&f),
            "chunk extraction",
        )?;
        chunk_files.push(f);
    }

    // One whisper-cli run for all chunks so the model loads once; it writes <chunk>.wav.json.
    let mut cmd = Command::new(&whisper);
    cmd.arg("-m").arg(model).args(["-l", language, "-ml", "1", "-sow", "-ojf", "-np"]);
    // DTW token anchors need flash attention off in this whisper.cpp build.
    let preset = dtw_preset(model);
    if let Some(p) = &preset {
        cmd.args(["-dtw", p, "-nfa"]);
    }
    for f in &chunk_files {
        cmd.arg("-f").arg(f);
    }
    run(&mut cmd, "whisper-cli")?;

    let env = envelope(&read_wav_samples(&wav)?);
    let mut words = Vec::new();
    let mut langs = Vec::new();
    let mut used_dtw = false;
    for (c, f) in chunks.iter().zip(&chunk_files) {
        let mut json_path = f.clone().into_os_string();
        json_path.push(".json");
        let json = std::fs::read_to_string(&json_path).map_err(|e| format!("Missing whisper output: {e}"))?;
        let (lang, ws) = parse_whisper_chunk(&json, *c)?;
        if !lang.is_empty() && !langs.contains(&lang) {
            langs.push(lang);
        }
        used_dtw |= ws.iter().any(|w| w.anchor_first.is_some());
        for (text, start, end) in refine_words(&ws, &env, *c) {
            words.push(Word { id: words.len(), text, start, end });
        }
    }
    Ok(Transcript {
        source_id: "clip_1".into(),
        duration_sec: info.duration_sec,
        language_requested: language.into(),
        languages_detected: langs,
        words,
        silences,
        speech_chunks: chunks,
        whisper_model: model.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        timing_method: if used_dtw { "dtw+energy" } else { "offsets+energy" }.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_silencedetect_output() {
        let err = "[silencedetect @ 0x1] silence_start: 2.5\n[silencedetect @ 0x1] silence_end: 4.9 | silence_duration: 2.4\n\
                   [silencedetect @ 0x1] silence_start: 9.0\n";
        let s = parse_silences(err, 10.0);
        assert_eq!(s, vec![Interval { start: 2.5, end: 4.9 }, Interval { start: 9.0, end: 10.0 }]);
    }

    #[test]
    fn chunks_are_padded_complements() {
        let s = vec![Interval { start: 2.5, end: 4.9 }, Interval { start: 9.0, end: 10.0 }];
        let c = speech_chunks(&s, 10.0);
        assert_eq!(c.len(), 2);
        assert!((c[0].start - 0.0).abs() < 1e-9 && (c[0].end - 2.6).abs() < 1e-9);
        assert!((c[1].start - 4.8).abs() < 1e-9 && (c[1].end - 9.1).abs() < 1e-9);
        // Leading silence and no trailing speech fragment.
        let c = speech_chunks(&[Interval { start: 0.0, end: 1.0 }], 1.1);
        assert!(c.is_empty());
    }

    #[test]
    fn parses_whisper_words_with_offset_and_clamp() {
        let json = r#"{"result":{"language":"tl"},"transcription":[
          {"text":"","offsets":{"from":0,"to":50}},
          {"text":" Sobrang","offsets":{"from":100,"to":480}},
          {"text":" dali","offsets":{"from":480,"to":9000}}]}"#;
        let (lang, w) = parse_whisper_chunk(json, Interval { start: 10.0, end: 11.0 }).unwrap();
        assert_eq!(lang, "tl");
        assert_eq!(w.len(), 2);
        assert_eq!((w[0].text.as_str(), w[0].start, w[0].end), ("Sobrang", 10.1, 10.48));
        assert_eq!(w[1].end, 11.0); // clamped to chunk end
        assert_eq!(w[0].anchor_first, None); // no tokens -> no DTW anchor
    }

    #[test]
    fn dtw_anchors_skip_punctuation_tokens() {
        let json = r#"{"result":{"language":"en"},"transcription":[
          {"text":" you,","offsets":{"from":0,"to":300},"tokens":[{"text":" you","t_dtw":39},{"text":",","t_dtw":65}]}]}"#;
        let (_, w) = parse_whisper_chunk(json, Interval { start: 6.0, end: 8.0 }).unwrap();
        assert_eq!((w[0].anchor_first, w[0].anchor_last), (Some(6.39), Some(6.39)));
    }

    fn raw(text: &str, start: f64, end: f64, anchor: f64) -> RawWord {
        RawWord { text: text.into(), start, end, anchor_first: Some(anchor), anchor_last: Some(anchor) }
    }

    #[test]
    fn refine_cuts_at_real_pause_between_anchors() {
        // 0.0-0.6 speech ("you"), 0.6-0.65 quiet, 0.65-0.95 speech ("um"), 0.95-1.15 quiet, 1.15-1.5 speech ("how").
        let mut env = vec![5000.0; 150];
        for f in 60..65 { env[f] = 2.0; }
        for f in 95..115 { env[f] = 2.0; }
        // Whisper's own edges are late by ~0.3 s; anchors sit inside the true words.
        let words = vec![raw("you,", 0.0, 0.95, 0.39), raw("um,", 0.95, 1.25, 0.87), raw("how", 1.25, 1.5, 1.37)];
        let r = refine_words(&words, &env, Interval { start: 0.0, end: 1.5 });
        let close = |a: f64, b: f64| (a - b).abs() < 0.011;
        assert!(close(r[0].2, 0.60), "{r:?}");
        assert!(close(r[1].1, 0.65) && close(r[1].2, 0.95), "{r:?}");
        assert!(close(r[2].1, 1.15), "{r:?}");
    }

    #[test]
    fn dtw_presets_follow_model_names() {
        assert_eq!(dtw_preset(Path::new("/m/ggml-small.bin")).as_deref(), Some("small"));
        assert_eq!(dtw_preset(Path::new("/m/ggml-base.en.bin")).as_deref(), Some("base.en"));
        assert_eq!(dtw_preset(Path::new("/m/ggml-large-v3.bin")).as_deref(), Some("large.v3"));
        assert_eq!(dtw_preset(Path::new("/m/ggml-large-v3-turbo.bin")).as_deref(), Some("large.v3.turbo"));
        assert_eq!(dtw_preset(Path::new("/m/custom.bin")), None);
    }

    #[test]
    fn rejects_bad_language_codes() {
        let info = media::MediaInfo {
            path: "/x.mp4".into(), file_name: "x.mp4".into(), size_bytes: 1, duration_sec: 1.0,
            width: 2, height: 2, video_codec: "h264".into(), fps: 30.0, has_audio: true,
        };
        let model = std::env::current_exe().unwrap(); // any existing file
        let e = transcribe(&info, Path::new("/tmp"), &model, "tl; rm -rf /").unwrap_err();
        assert!(e.contains("Language"));
    }
}
