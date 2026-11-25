//! Acoustic echo cancellation for `mic`. The `tts` queue plays each
//! synthesized chunk as a WAV file, and notes that file and the moment its
//! player started here, as the echo reference. When a recording stops,
//! `mic` takes the references that overlap it and [`cancel`] subtracts the
//! speaker's sound from the file, so the agent's own voice does not come
//! back as the user's speech.
//!
//! Three steps, all on 16 kHz mono samples:
//!
//! 1. The reference is laid on the recording's timeline from the clock
//!    times. Player start-up, the output buffer and the input buffer add
//!    an unknown delay, so [`find_delay`] correlates the two loudness
//!    envelopes to find it, to about 2 ms.
//! 2. An NLMS filter ([`TAPS`] taps, 32 ms) learns the path from speaker to
//!    microphone, room included, and subtracts its estimate of the echo.
//!    It adapts only while the reference is loud and the microphone is not
//!    much louder than the echo estimate (a Geigel test), so the user's
//!    own voice is not learned as echo.
//! 3. Where the filter has taken out nearly all the energy, nothing else is
//!    changed: no gating and no noise gate, so the voice stays natural.
//!
//! The work runs once, at `record-stop`, on samples already in memory, so
//! it adds no delay while the user speaks and a few milliseconds per second
//! of audio at the end. A recording with no overlapping reference is not
//! touched. `[mic] echo_cancel = false` turns this off.
//!
//! Only the `Http` engine of `tts` keeps a file to use as the reference;
//! `say`, `espeak` and `spd-say` play inside the system and are not
//! cancelled.

#![allow(clippy::chunks_exact_to_as_chunks)]

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The sample rate of a recording, and of the work done here.
pub const RATE: u32 = 16_000;
/// Filter length: 32 ms covers the speaker-to-microphone path of a laptop
/// or a desk, with the alignment from [`find_delay`] done first.
const TAPS: usize = 512;
/// NLMS step size. Higher converges faster and risks eating speech.
const MU: f32 = 0.35;
/// Added to the reference power, so silence does not blow the step up.
const EPS: f32 = 1e-3;
/// After the first second of reference the filter adapts only while the
/// microphone is below this multiple of the loudest recent echo estimate:
/// anything louder is the user talking over the speaker.
const DOUBLE_TALK: f32 = 1.5;
/// Once double talk shows, adaptation stays off this long: the user's
/// voice keeps crossing the threshold between peaks.
const HANGOVER: usize = RATE as usize / 4;
/// The step size after the warm-up: smaller, so a stray sample of the
/// user's voice moves the filter less.
const MU_TRACK: f32 = 0.1;
/// Samples of loud reference during which the filter adapts regardless,
/// so it can converge before its own estimate is a usable yardstick.
const WARMUP: usize = RATE as usize;
/// How many references are kept.
const KEEP: usize = 32;
/// References older than this are dropped when a new one arrives.
const MAX_AGE: Duration = Duration::from_secs(15 * 60);
/// The delay search: the reference sounds in the recording this much
/// before its clock time at the earliest, and this much after at the
/// latest. The recording's own start is only known to a fraction of a
/// second.
const SEARCH_BEFORE: Duration = Duration::from_millis(400);
const SEARCH_AFTER: Duration = Duration::from_millis(1500);
/// The delay search runs at 4 kHz: every 4th sample, after averaging.
const DECIMATE: usize = 4;
/// How much of the reference the delay search looks at.
const SEARCH_SECS: usize = 8;
/// The lowest normalized correlation that counts as hearing the reference.
const MIN_COHERENCE: f32 = 0.05;

/// A WAV file played at a moment, as 16 kHz mono samples.
#[derive(Clone, Debug)]
struct Reference {
    /// Milliseconds since the Unix epoch when the player started.
    started_ms: u64,
    samples: Vec<f32>,
}

/// The audio the node played lately, shared by `tts` (which adds) and
/// `mic` (which reads).
#[derive(Default)]
pub struct EchoReference {
    items: Mutex<VecDeque<Reference>>,
}

impl EchoReference {
    /// Notes that the WAV at `path` starts playing now. A file that is
    /// not a readable PCM WAV is ignored.
    pub fn played(&self, path: &Path) {
        self.played_at(path, now_ms());
    }

    fn played_at(&self, path: &Path, started_ms: u64) {
        let Some(samples) = read_wav(path) else {
            tracing::debug!("echo: cannot use {} as a reference", path.display());
            return;
        };
        let mut items = self.items.lock().unwrap();
        let oldest = started_ms.saturating_sub(MAX_AGE.as_millis() as u64);
        items.retain(|r| r.started_ms >= oldest);
        if items.len() >= KEEP {
            items.pop_front();
        }
        items.push_back(Reference {
            started_ms,
            samples,
        });
    }

    /// Removes the echo from `mic`, the samples of a recording that ended
    /// at `ended_ms`. Returns how many samples carried a reference.
    pub fn cancel(&self, mic: &mut [i16], ended_ms: u64) -> usize {
        let items = self.items.lock().unwrap();
        let refs: Vec<Reference> = items.iter().cloned().collect();
        drop(items);
        cancel(mic, ended_ms, &refs)
    }

    /// Cancels the echo in the WAV file at `path`, in place. `ended_ms` is
    /// when the recording stopped.
    pub fn cancel_file(&self, path: &Path, ended_ms: u64) -> std::io::Result<usize> {
        if self.items.lock().unwrap().is_empty() {
            return Ok(0);
        }
        let bytes = std::fs::read(path)?;
        let Some((at, len)) = data_chunk(&bytes) else {
            return Ok(0);
        };
        let mut samples: Vec<i16> = bytes[at..at + len]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        let touched = self.cancel(&mut samples, ended_ms);
        if touched == 0 {
            return Ok(0);
        }
        let mut out = bytes;
        for (i, s) in samples.iter().enumerate() {
            out[at + 2 * i..at + 2 * i + 2].copy_from_slice(&s.to_le_bytes());
        }
        // Write beside the file and rename, so a reader never sees half.
        let tmp = path.with_extension("wav.tmp");
        std::fs::write(&tmp, out)?;
        std::fs::rename(&tmp, path)?;
        Ok(touched)
    }
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The reference signal on the recording's timeline: `mic.len()` samples
/// that are the speaker's output as far as the clocks say. `None` when no
/// reference overlaps the recording's search window.
fn timeline(mic_len: usize, ended_ms: u64, refs: &[Reference]) -> Option<Vec<f32>> {
    let dur_ms = mic_len as u64 * 1000 / RATE as u64;
    let start_ms = ended_ms.saturating_sub(dur_ms) as i64;
    let mut out = vec![0f32; mic_len];
    let mut any = false;
    for r in refs {
        let offset = (r.started_ms as i64 - start_ms) * RATE as i64 / 1000;
        let end = offset + r.samples.len() as i64;
        let slack = (SEARCH_AFTER.as_secs_f32() * RATE as f32) as i64;
        let before = (SEARCH_BEFORE.as_secs_f32() * RATE as f32) as i64;
        if end < -before || offset > mic_len as i64 + slack {
            continue;
        }
        for (i, s) in r.samples.iter().enumerate() {
            let at = offset + i as i64;
            if (0..mic_len as i64).contains(&at) {
                out[at as usize] += s;
            }
        }
        any = true;
    }
    any.then_some(out)
}

/// Subtracts the echo of `refs` from `mic`. Returns the number of samples
/// the filter ran over, 0 when nothing overlapped.
fn cancel(mic: &mut [i16], ended_ms: u64, refs: &[Reference]) -> usize {
    let Some(reference) = timeline(mic.len(), ended_ms, refs) else {
        return 0;
    };
    let x: Vec<f32> = mic.iter().map(|&s| s as f32 / 32768.0).collect();
    let delay = find_delay(&x, &reference);
    let Some(delay) = delay else {
        return 0;
    };
    let cleaned = nlms(&x, &reference, delay);
    for (m, c) in mic.iter_mut().zip(cleaned) {
        *m = (c * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
    }
    mic.len()
}

/// The delay, in samples, from `reference` to its echo in `mic`: how late
/// the speaker's sound arrives against the clock times. Negative when it
/// arrives early. `None` when the microphone's waveform does not follow
/// the reference's.
///
/// This correlates waveforms at 4 kHz, over at most [`SEARCH_SECS`] of the
/// loud part of the reference. Unrelated speech from the user adds noise
/// to every lag alike, so it does not move the peak the way it moves a
/// loudness envelope.
fn find_delay(mic: &[f32], reference: &[f32]) -> Option<i64> {
    let down = |s: &[f32]| -> Vec<f32> {
        s.chunks_exact(DECIMATE)
            .map(|c| c.iter().sum::<f32>() / DECIMATE as f32)
            .collect()
    };
    let (m, r) = (down(mic), down(reference));
    let peak = r.iter().fold(0f32, |a, v| a.max(v.abs()));
    if peak < 1e-3 {
        return None;
    }
    let first = r.iter().position(|v| v.abs() > peak * 0.05)?;
    let end = (first + SEARCH_SECS * (RATE as usize / DECIMATE)).min(r.len());
    let seg = &r[first..end];
    let pa: f32 = seg.iter().map(|v| v * v).sum();
    let per_sec = (RATE as usize / DECIMATE) as f32;
    let back = (SEARCH_BEFORE.as_secs_f32() * per_sec) as i64;
    let fwd = (SEARCH_AFTER.as_secs_f32() * per_sec) as i64;
    let mut best: Option<(i64, f32)> = None;
    for lag in -back..=fwd {
        let (mut dot, mut pb) = (0f32, 0f32);
        for (i, a) in seg.iter().enumerate() {
            let j = (first + i) as i64 + lag;
            if j < 0 || j >= m.len() as i64 {
                continue;
            }
            let b = m[j as usize];
            dot += a * b;
            pb += b * b;
        }
        let score = dot.abs() / (pa * pb).sqrt().max(1e-9);
        if best.is_none_or(|(_, s)| score > s) {
            best = Some((lag, score));
        }
    }
    let (lag, score) = best?;
    // Uncorrelated signals score about 1/sqrt(samples), a few thousandths;
    // a thin echo under loud speech still scores far above that.
    (score > MIN_COHERENCE).then_some(lag * DECIMATE as i64)
}

/// The NLMS canceller. `delay` shifts the reference so the filter's taps
/// start half a filter before the echo's main arrival, which leaves room
/// for the alignment error and the early reflections.
fn nlms(mic: &[f32], reference: &[f32], delay: i64) -> Vec<f32> {
    let lead = (TAPS / 4) as i64;
    let at = |i: i64| -> f32 {
        let j = i - delay + lead;
        if j >= 0 && (j as usize) < reference.len() {
            reference[j as usize]
        } else {
            0.0
        }
    };
    let mut w = vec![0f32; TAPS];
    let mut x = vec![0f32; TAPS]; // x[k] = reference at i - k
    let mut power = 0f32;
    let mut out = Vec::with_capacity(mic.len());
    let mut peak = 0f32;
    let mut ypeak = 0f32;
    let mut active = 0usize;
    let mut hold = 0usize;
    for (i, &d) in mic.iter().enumerate() {
        let new = at(i as i64);
        let old = x[TAPS - 1];
        x.rotate_right(1);
        x[0] = new;
        power += new * new - old * old;
        power = power.max(0.0);
        peak = (peak * 0.9995).max(new.abs());
        let y: f32 = w.iter().zip(&x).map(|(a, b)| a * b).sum();
        let e = d - y;
        ypeak = (ypeak * 0.9995).max(y.abs());
        if new.abs() > 1e-3 {
            active += 1;
        }
        // Adapt while the reference plays and the mic is not louder than
        // the echo estimate allows: louder means the user is talking.
        let warm = active < WARMUP;
        if !warm && d.abs() > DOUBLE_TALK * ypeak {
            hold = HANGOVER;
        }
        if hold > 0 {
            hold -= 1;
        } else if peak > 1e-3 {
            let g = if warm { MU } else { MU_TRACK } * e / (power + EPS);
            for (wk, xk) in w.iter_mut().zip(&x) {
                *wk += g * xk;
            }
        }
        out.push(e);
    }
    out
}

/// The byte range of the samples in a WAV file's `data` chunk. ffmpeg
/// leaves the size fields wrong while it writes, so the file's length caps
/// the range.
fn data_chunk(bytes: &[u8]) -> Option<(usize, usize)> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        if id == b"data" {
            let start = at + 8;
            let len = size.min(bytes.len() - start) & !1;
            return Some((start, len));
        }
        at += 8 + size + (size & 1);
    }
    None
}

/// Reads a PCM WAV (16-bit, 8-bit or 32-bit float, any rate, any channel
/// count) as 16 kHz mono samples in -1..1.
fn read_wav(path: &Path) -> Option<Vec<f32>> {
    let bytes = std::fs::read(path).ok()?;
    let (start, len) = data_chunk(&bytes)?;
    // fmt chunk: format, channels, rate, ..., bits
    let mut at = 12;
    let (mut format, mut channels, mut rate, mut bits) = (0u16, 0u16, 0u32, 0u16);
    while at + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        if &bytes[at..at + 4] == b"fmt " && at + 24 <= bytes.len() {
            let f = &bytes[at + 8..];
            format = u16::from_le_bytes([f[0], f[1]]);
            channels = u16::from_le_bytes([f[2], f[3]]);
            rate = u32::from_le_bytes([f[4], f[5], f[6], f[7]]);
            bits = u16::from_le_bytes([f[14], f[15]]);
            break;
        }
        at += 8 + size + (size & 1);
    }
    if channels == 0 || rate == 0 {
        return None;
    }
    let data = &bytes[start..start + len];
    let mono: Vec<f32> = match (format, bits) {
        (1 | 0xFFFE, 16) => data
            .chunks_exact(2 * channels as usize)
            .map(|f| {
                f.chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                    .sum::<f32>()
                    / channels as f32
            })
            .collect(),
        (3, 32) => data
            .chunks_exact(4 * channels as usize)
            .map(|f| {
                f.chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .sum::<f32>()
                    / channels as f32
            })
            .collect(),
        _ => return None,
    };
    Some(resample(&mono, rate))
}

/// Linear resampling to [`RATE`], with a box average first when going
/// down, so the high band does not fold back.
fn resample(x: &[f32], rate: u32) -> Vec<f32> {
    if rate == RATE {
        return x.to_vec();
    }
    let ratio = rate as f64 / RATE as f64;
    let n = (x.len() as f64 / ratio) as usize;
    let width = ratio.ceil().max(1.0) as usize;
    (0..n)
        .map(|i| {
            let pos = i as f64 * ratio;
            let k = pos as usize;
            if ratio > 1.0 {
                let end = (k + width).min(x.len());
                x[k..end].iter().sum::<f32>() / (end - k).max(1) as f32
            } else {
                let frac = (pos - k as f64) as f32;
                let next = x.get(k + 1).copied().unwrap_or(x[k]);
                x[k] * (1.0 - frac) + next * frac
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic, speech-like signal: bursts of a few harmonics
    /// with a slow amplitude, and gaps.
    fn voiced(n: usize, seed: u32) -> Vec<f32> {
        voice(n, seed, 180.0, 3.1)
    }

    /// `voiced` with its own pitch and rhythm.
    fn voice(n: usize, seed: u32, pitch: f32, rhythm: f32) -> Vec<f32> {
        let mut state = seed;
        let mut noise = move || {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (state >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        };
        (0..n)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                let env = ((t * rhythm).sin() * 0.5 + 0.5).powf(2.0);
                let tone: f32 = (1..5)
                    .map(|h| (2.0 * std::f32::consts::PI * pitch * h as f32 * t).sin() / h as f32)
                    .sum();
                (tone * 0.25 + noise() * 0.3) * env
            })
            .collect()
    }

    fn to_i16(x: &[f32]) -> Vec<i16> {
        x.iter().map(|v| (v * 32768.0) as i16).collect()
    }

    fn power(x: &[i16]) -> f64 {
        x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64
    }

    /// The reference as the speaker's echo reaches the mic: a delay, an
    /// attenuation and two reflections.
    fn echo_of(reference: &[f32], delay: usize) -> Vec<f32> {
        let mut out = vec![0f32; reference.len()];
        for (i, s) in reference.iter().enumerate() {
            for (extra, gain) in [(0usize, 0.5f32), (37, 0.2), (91, -0.1)] {
                if let Some(o) = out.get_mut(i + delay + extra) {
                    *o += s * gain;
                }
            }
        }
        out
    }

    fn reference_at(samples: Vec<f32>, started_ms: u64) -> Reference {
        Reference {
            started_ms,
            samples,
        }
    }

    #[test]
    fn echo_is_removed_and_quiet_mic_stays_quiet() {
        let n = RATE as usize * 6;
        let speaker = voiced(n, 7);
        // The reference starts 300 ms into the recording; its sound takes
        // 120 ms to arrive.
        let lead = RATE as usize * 3 / 10;
        let mut padded = vec![0f32; lead];
        padded.extend(&speaker[..n - lead]);
        let mic_f = echo_of(&padded, RATE as usize * 3 / 25);
        let mut mic = to_i16(&mic_f);
        let before = power(&mic);
        // The recording spans [100000, 106000).
        let refs = [reference_at(speaker.clone(), 100_300)];
        let touched = cancel(&mut mic, 106_000, &refs);
        assert_eq!(touched, n);
        let after = power(&mic[RATE as usize * 2..]);
        let erle = 10.0 * (before / after.max(1.0)).log10();
        assert!(erle > 15.0, "echo reduced by only {erle:.1} dB");
    }

    #[test]
    fn near_end_speech_survives() {
        let n = RATE as usize * 8;
        let speaker = voiced(n, 11);
        let near = voice(n, 99, 137.0, 4.3);
        // The user speaks only in the last 3 seconds.
        let near: Vec<f32> = near
            .iter()
            .enumerate()
            .map(|(i, v)| if i > RATE as usize * 5 { *v * 1.5 } else { 0.0 })
            .collect();
        let echo = echo_of(&speaker, RATE as usize / 10);
        let mic_f: Vec<f32> = echo.iter().zip(&near).map(|(a, b)| a + b).collect();
        let mut mic = to_i16(&mic_f);
        let refs = [reference_at(speaker, 50_000)];
        cancel(&mut mic, 50_000 + 8000, &refs);
        // Correlation of the cleaned tail with the user's voice.
        let tail = RATE as usize * 6..n;
        let (mut dot, mut pa, mut pb) = (0f64, 0f64, 0f64);
        for i in tail {
            let (a, b) = (mic[i] as f64 / 32768.0, near[i] as f64);
            dot += a * b;
            pa += a * a;
            pb += b * b;
        }
        let corr = dot / (pa * pb).sqrt();
        assert!(corr > 0.9, "near-end speech correlation fell to {corr:.2}");
    }

    #[test]
    fn a_recording_with_no_reference_is_untouched() {
        let mut mic = to_i16(&voiced(RATE as usize * 2, 3));
        let copy = mic.clone();
        assert_eq!(cancel(&mut mic, 10_000, &[]), 0);
        assert_eq!(mic, copy);
        // A reference that played an hour earlier does not overlap.
        let far = [reference_at(voiced(RATE as usize, 5), 1_000)];
        assert_eq!(cancel(&mut mic, 10_000_000, &far), 0);
        assert_eq!(mic, copy);
    }

    #[test]
    fn a_mic_that_never_heard_the_reference_is_untouched() {
        let mut mic = to_i16(&voiced(RATE as usize * 4, 21));
        let copy = mic.clone();
        // An unrelated reference at the same time: the envelopes may
        // correlate a little, but not past the threshold.
        let other: Vec<f32> = (0..RATE as usize * 4)
            .map(|i| {
                // Bursts at an unrelated rhythm.
                let on = ((i / 5300) % 3 == 0) as u8 as f32;
                on * ((i as f32 * 0.37).sin() * 0.3)
            })
            .collect();
        let refs = [reference_at(other, 20_000)];
        cancel(&mut mic, 24_000, &refs);
        let drift: f64 = mic
            .iter()
            .zip(&copy)
            .map(|(a, b)| (*a as f64 - *b as f64).abs())
            .sum::<f64>()
            / mic.len() as f64;
        assert!(
            drift < 800.0,
            "an unheard reference changed the audio by {drift}"
        );
    }

    #[test]
    fn wav_files_round_trip_through_cancel_file() {
        let dir = tempfile::tempdir().unwrap();
        let speaker = voiced(RATE as usize * 3, 13);
        // The reference WAV at 24 kHz, as pocket-tts writes it.
        let ref_path = dir.path().join("ref.wav");
        let up: Vec<i16> = (0..speaker.len() * 3 / 2)
            .map(|i| {
                let p = i as f32 * 2.0 / 3.0;
                let k = p as usize;
                let f = p - k as f32;
                let a = speaker[k.min(speaker.len() - 1)];
                let b = speaker[(k + 1).min(speaker.len() - 1)];
                ((a * (1.0 - f) + b * f) * 32768.0) as i16
            })
            .collect();
        write_wav(&ref_path, 24_000, &up);
        let mic_path = dir.path().join("rec.wav");
        let mic = to_i16(&echo_of(&speaker, 1600));
        write_wav(&mic_path, RATE, &mic);
        let before = power(&mic);

        let echo = EchoReference::default();
        echo.played_at(&ref_path, 5_000);
        let touched = echo.cancel_file(&mic_path, 5_000 + 3000).unwrap();
        assert_eq!(touched, mic.len());
        let bytes = std::fs::read(&mic_path).unwrap();
        let (at, len) = data_chunk(&bytes).unwrap();
        let after: Vec<i16> = bytes[at..at + len]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(after.len(), mic.len());
        assert!(power(&after[RATE as usize..]) < before / 10.0);
    }

    #[test]
    fn unreadable_references_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let echo = EchoReference::default();
        echo.played(&dir.path().join("missing.wav"));
        std::fs::write(dir.path().join("junk.wav"), b"not a wav").unwrap();
        echo.played(&dir.path().join("junk.wav"));
        assert!(echo.items.lock().unwrap().is_empty());
    }

    fn write_wav(path: &Path, rate: u32, samples: &[i16]) {
        let data_len = samples.len() as u32 * 2;
        let mut b = Vec::new();
        b.extend(b"RIFF");
        b.extend((36 + data_len).to_le_bytes());
        b.extend(b"WAVEfmt ");
        b.extend(16u32.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(rate.to_le_bytes());
        b.extend((rate * 2).to_le_bytes());
        b.extend(2u16.to_le_bytes());
        b.extend(16u16.to_le_bytes());
        b.extend(b"data");
        b.extend(data_len.to_le_bytes());
        for s in samples {
            b.extend(s.to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }
}
