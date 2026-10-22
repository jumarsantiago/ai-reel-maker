//! Tempo and beat grid of a music file, so travel cuts can land on the beat. Plain onset-energy analysis:
//! loudness rises are autocorrelated to find the beat period (60-180 BPM), then the grid is shifted to sit
//! on the strongest onsets. Works best on music with a clear, steady pulse.

use crate::media;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

const RATE: usize = 11025;
const HOP: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeatGrid {
    /// Seconds between beats.
    pub period: f64,
    /// Time of the first beat, from where the analysis started (0 <= phase < period).
    pub phase: f64,
    /// How clearly the pulse stood out (0-1); low values mean the music has no steady beat.
    pub confidence: f64,
}

impl BeatGrid {
    pub fn bpm(&self) -> f64 {
        60.0 / self.period
    }
}

/// Onset strength per hop (positive rises in short-time energy).
pub fn onset_envelope(samples: &[f32]) -> Vec<f64> {
    let energy: Vec<f64> = samples.chunks(HOP).map(|c| c.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>().sqrt()).collect();
    let mut env = vec![0.0; energy.len()];
    for i in 1..energy.len() {
        env[i] = (energy[i] - energy[i - 1]).max(0.0);
    }
    env
}

/// Beat grid from an onset envelope sampled at `rate` frames per second.
pub fn grid_from_envelope(env: &[f64], rate: f64) -> Option<BeatGrid> {
    let (min_lag, max_lag) = ((rate * 60.0 / 180.0).floor() as usize, (rate * 60.0 / 60.0).ceil() as usize);
    if env.len() < max_lag * 4 {
        return None;
    }
    // Smooth a little so a beat period that falls between two whole lags still forms one clear peak
    // (otherwise half-tempo, whose lag happens to be closer to a whole number, can win).
    let mut smooth = env.to_vec();
    for _ in 0..2 {
        smooth = (0..smooth.len())
            .map(|i| {
                let a = smooth[i.saturating_sub(1)];
                let c = smooth[(i + 1).min(smooth.len() - 1)];
                0.25 * a + 0.5 * smooth[i] + 0.25 * c
            })
            .collect();
    }
    let mean = smooth.iter().sum::<f64>() / smooth.len() as f64;
    let e: Vec<f64> = smooth.iter().map(|v| v - mean).collect();
    let ac = |lag: usize| e.iter().zip(&e[lag..]).map(|(a, b)| a * b).sum::<f64>() / (e.len() - lag) as f64;
    let zero = ac(0).max(1e-12);
    // Prefer tempos near 120 BPM a little, so half/double tempo ties go to the musical one.
    let (best_lag, best) = (min_lag..=max_lag)
        .map(|lag| {
            let bpm = rate * 60.0 / lag as f64;
            let weight = (-((bpm / 120.0).log2().powi(2)) / 2.0).exp();
            (lag, ac(lag) * (0.6 + 0.4 * weight))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    // Refine to a fractional lag with a parabola through the neighbours.
    let refine = |l: usize| {
        let (y0, y1, y2) = (ac(l - 1), ac(l), ac(l + 1));
        let denom = y0 - 2.0 * y1 + y2;
        l as f64 + if denom.abs() > 1e-12 { (0.5 * (y0 - y2) / denom).clamp(-0.5, 0.5) } else { 0.0 }
    };
    // Autocorrelation at a fractional lag (linear interpolation between samples).
    let ac_frac = |lag: f64| {
        let (k, f) = (lag.floor() as usize, lag.fract());
        if k + 1 >= e.len() {
            return 0.0;
        }
        (0..e.len() - k - 1).map(|i| e[i] * (e[i + k] * (1.0 - f) + e[i + k + 1] * f)).sum::<f64>() / (e.len() - k - 1) as f64
    };
    let mut lag = refine(best_lag);
    // Octave check: a steady pulse at half the period that is almost as strong means the real beat is faster.
    while lag / 2.0 >= min_lag as f64 && ac_frac(lag / 2.0) >= 0.6 * ac_frac(lag) {
        lag = refine((lag / 2.0).round() as usize);
    }
    let period = lag / rate;
    // Phase: the offset whose comb of beats collects the most onset energy.
    let steps = lag.round().max(1.0) as usize;
    let (phase_idx, _) = (0..steps)
        .map(|p| {
            let mut s = 0.0;
            let mut t = p as f64;
            while (t as usize) < env.len() {
                s += env[t as usize];
                t += lag;
            }
            (p, s)
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    Some(BeatGrid { period, phase: phase_idx as f64 / rate, confidence: (best / zero).clamp(0.0, 1.0) })
}

/// Beat grid of `music` starting `start` seconds in (up to 90 seconds analysed).
pub fn detect(music: &Path, start: f64) -> Result<Option<BeatGrid>, String> {
    let ffmpeg = media::find_tool("ffmpeg").ok_or("FFmpeg not found.")?;
    let mut child = Command::new(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-v", "error", "-ss", &format!("{:.3}", start.max(0.0)), "-t", "90", "-i"])
        .arg(music)
        .args(["-vn", "-ac", "1", "-ar", &RATE.to_string(), "-f", "f32le", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to start FFmpeg: {e}"))?;
    let mut buf = Vec::new();
    child.stdout.take().expect("piped").read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let _ = child.wait();
    let samples: Vec<f32> = buf.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    Ok(grid_from_envelope(&onset_envelope(&samples), RATE as f64 / HOP as f64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click_track(bpm: f64, phase: f64, secs: f64) -> Vec<f32> {
        let period = 60.0 / bpm;
        (0..(secs * RATE as f64) as usize)
            .map(|i| {
                let t = i as f64 / RATE as f64;
                let since = (t - phase).rem_euclid(period);
                if t < phase { 0.0 } else { ((2.0 * std::f64::consts::PI * 1000.0 * t).sin() * (-60.0 * since).exp()) as f32 }
            })
            .collect()
    }

    #[test]
    fn finds_tempo_and_phase_of_a_click_track() {
        for (bpm, phase) in [(120.0, 0.2), (96.0, 0.05), (140.0, 0.31)] {
            let env = onset_envelope(&click_track(bpm, phase, 30.0));
            let g = grid_from_envelope(&env, RATE as f64 / HOP as f64).unwrap();
            assert!((g.bpm() - bpm).abs() < 1.5, "bpm {} vs {bpm}", g.bpm());
            let err = ((g.phase - phase).rem_euclid(g.period)).min((phase - g.phase).rem_euclid(g.period));
            assert!(err < 0.04, "phase {} vs {phase}", g.phase);
            assert!(g.confidence > 0.2, "confidence {}", g.confidence);
        }
    }

    #[test]
    fn real_ffmpeg_click_track() {
        let Some(ffmpeg) = media::find_tool("ffmpeg") else { return };
        let dir = std::env::temp_dir().join(format!("arm-beats-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("clicks.m4a");
        // 128 BPM clicks, first beat at 0.1s.
        let expr = "sin(2*PI*1200*t)*exp(-50*mod(t-0.1+10*0.46875\\,0.46875))*gte(t\\,0.1)";
        assert!(Command::new(&ffmpeg)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", &format!("aevalsrc={expr}:s=44100:d=20"), "-c:a", "aac"])
            .arg(&p)
            .status()
            .unwrap()
            .success());
        let g = detect(&p, 0.0).unwrap().unwrap();
        assert!((g.bpm() - 128.0).abs() < 2.0, "bpm {}", g.bpm());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
