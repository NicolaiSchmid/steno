//! The crafted cases of the extra windows (`ChunkerConfig::decode_gaps`),
//! shared by both engines' tests: this crate's `tests/extra_windows.rs`
//! (ONNX) and `steno-speech-coreml`'s (`CoreML`), which includes this file
//! by path. The cases are cut from FLEURS German `cat/` (file 02 for the
//! loud speech, 03 for the quiet), so nothing here is a recording of a
//! meeting:
//!
//! - quiet speech, 6.65 s or 9.1 s at -20 and -26 dB, as the lead-in,
//!   between two loud stretches and as the tail, where a detector may hear
//!   no speech and the chunks leave it out ([`quiet_cases`]);
//! - 4 s of digital silence before, 9.1 s between and 4 s after the loud
//!   speech, and the same with synthetic room noise (filtered noise and
//!   mains hum at -50 dBFS) under the whole file ([`quiet_room`]): chunks
//!   decode part of each stretch, extra windows the rest of the noise
//!   (digital silence gets none), and no word may lie in them.
//!
//! `STENO_EXTRA_WINDOWS_OUT` writes every case as a 16 kHz WAV with an
//! empty Swift baseline beside it, so another revision's harness runs the
//! same audio.

// Each test target uses only part of it.
#![allow(dead_code)]

use std::ops::Range;
use std::path::{Path, PathBuf};

use steno_speech::wav::read_pcm16;
use steno_speech::{SAMPLE_RATE, Transcript};

/// Words a silent or noisy stretch keeps clear of its loud neighbours: a
/// word whose middle falls this close to a stretch's edge is theirs.
pub const EDGE_SECONDS: f64 = 0.25;

/// `STENO_FLEURS_DIR`, or `None` with a message.
pub fn fleurs() -> Option<PathBuf> {
    let dir = std::env::var_os("STENO_FLEURS_DIR").map(PathBuf::from);
    if dir.is_none() {
        eprintln!("STENO_FLEURS_DIR unset: nothing to run");
    }
    dir
}

fn seconds(at: f64) -> usize {
    // Seconds into a FLEURS file, minutes at most.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let samples = (at * SAMPLE_RATE as f64).round() as usize;
    samples
}

/// Seconds at sample `samples`.
pub fn time(samples: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let at = samples as f64 / SAMPLE_RATE as f64;
    at
}

/// `from..to` seconds of `cat/<file>.wav`.
fn cut(dir: &Path, file: &str, from: f64, to: f64) -> Vec<f32> {
    let samples = read_pcm16(&dir.join("cat").join(format!("{file}.wav"))).expect("FLEURS file");
    samples[seconds(from)..seconds(to)].to_vec()
}

fn scaled(samples: &[f32], db: f32) -> Vec<f32> {
    let gain = 10f32.powf(db / 20.0);
    samples.iter().map(|x| x * gain).collect()
}

/// One crafted file and the stretches its checks look at.
pub struct Case {
    pub name: String,
    pub samples: Vec<f32>,
    pub stretches: Vec<Range<usize>>,
}

/// Joins `parts`; the stretches are the parts marked `true`.
fn join(name: String, parts: &[(&[f32], bool)]) -> Case {
    let mut samples = Vec::new();
    let mut stretches = Vec::new();
    for &(part, marked) in parts {
        if marked {
            stretches.push(samples.len()..samples.len() + part.len());
        }
        samples.extend_from_slice(part);
    }
    Case {
        name,
        samples,
        stretches,
    }
}

/// The quiet cases: `(position, level)` for the lead-in, a 6.65 s and a
/// 9.1 s gap and the tail, at 0, -20 and -26 dB.
pub fn quiet_cases(dir: &Path) -> Vec<(String, f32, Case)> {
    let loud = cut(dir, "fleurs-de-cat-02", 9.94, 24.22);
    let after = cut(dir, "fleurs-de-cat-02", 25.54, 38.46);
    let mut cases = Vec::new();
    for db in [0.0, -20.0, -26.0] {
        let short = scaled(&cut(dir, "fleurs-de-cat-03", 17.0, 23.65), db);
        let long = scaled(&cut(dir, "fleurs-de-cat-03", 17.0, 26.1), db);
        let level = format!("{}dB", db.abs());
        for (position, parts) in [
            ("lead", vec![(&short[..], true), (&loud[..], false)]),
            (
                "gap6",
                vec![(&loud[..], false), (&short[..], true), (&after[..], false)],
            ),
            (
                "gap9",
                vec![(&loud[..], false), (&long[..], true), (&after[..], false)],
            ),
            ("tail", vec![(&loud[..], false), (&short[..], true)]),
        ] {
            let case = join(format!("{position}-{level}"), &parts);
            cases.push((position.to_owned(), db, case));
        }
    }
    cases
}

/// Room tone: white noise through a one-pole low-pass, with 50 and 100 Hz
/// hum, at `dbfs` RMS; the same samples every run.
fn room_noise(len: usize, dbfs: f32) -> Vec<f32> {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut low = 0.0f32;
    let mut out: Vec<f32> = (0..len)
        .map(|i| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            // The top 24 bits as a uniform value in -1..1.
            #[allow(clippy::cast_precision_loss)]
            let white = (state >> 40) as f32 / (1u64 << 23) as f32 - 1.0;
            low = 0.97 * low + 0.03 * white;
            #[allow(clippy::cast_precision_loss)]
            let t = i as f32 / SAMPLE_RATE as f32;
            let hum = 0.3 * (std::f32::consts::TAU * 50.0 * t).sin()
                + 0.1 * (std::f32::consts::TAU * 100.0 * t).sin();
            low + 0.05 * hum
        })
        .collect();
    #[allow(clippy::cast_precision_loss)]
    let rms = (out.iter().map(|x| x * x).sum::<f32>() / len as f32).sqrt();
    let gain = 10f32.powf(dbfs / 20.0) / rms;
    for x in &mut out {
        *x *= gain;
    }
    out
}

/// Silence before, between and after the loud speech; with `noise`, room
/// tone under the whole file.
pub fn quiet_room(dir: &Path, noise: bool) -> Case {
    let loud = cut(dir, "fleurs-de-cat-02", 9.94, 24.22);
    let after = cut(dir, "fleurs-de-cat-02", 25.54, 38.46);
    let edge = vec![0.0f32; seconds(4.0)];
    let gap = vec![0.0f32; seconds(9.1)];
    let name = if noise { "room-noise" } else { "silence" };
    let mut case = join(
        name.to_owned(),
        &[
            (&edge, true),
            (&loud, false),
            (&gap, true),
            (&after, false),
            (&edge, true),
        ],
    );
    if noise {
        let noise = room_noise(case.samples.len(), -50.0);
        for (x, n) in case.samples.iter_mut().zip(noise) {
            *x += n;
        }
    }
    case
}

/// Words whose middle lies inside `range`, `margin` seconds in from both
/// ends.
pub fn words_inside(transcript: &Transcript, range: &Range<usize>, margin: f64) -> Vec<String> {
    let (from, to) = (time(range.start) + margin, time(range.end) - margin);
    transcript
        .words
        .iter()
        .filter(|w| (from..to).contains(&f64::midpoint(w.start, w.end)))
        .map(|w| w.text.clone())
        .collect()
}

/// Writes `case` for another revision's harness when
/// `STENO_EXTRA_WINDOWS_OUT` is set.
pub fn write_out(case: &Case) {
    let Some(dir) = std::env::var_os("STENO_EXTRA_WINDOWS_OUT").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).expect("output directory");
    let data: Vec<u8> = case
        .samples
        .iter()
        .flat_map(|x| {
            #[allow(clippy::cast_possible_truncation)]
            let value = (x.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            value.to_le_bytes()
        })
        .collect();
    let len = u32::try_from(data.len()).expect("a short case");
    let mut wav = Vec::with_capacity(44 + data.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    let rate = u32::try_from(SAMPLE_RATE).expect("16 kHz");
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&len.to_le_bytes());
    wav.extend_from_slice(&data);
    std::fs::write(dir.join(format!("{}.wav", case.name)), wav).expect("case WAV");
    std::fs::write(dir.join(format!("{}.parakeet-v3.json", case.name)), "[]")
        .expect("empty baseline");
}

/// The extra windows over the gaps.
pub fn extra_windows(transcript: &Transcript) -> impl Iterator<Item = &Range<usize>> {
    transcript
        .spans
        .iter()
        .filter(|s| !s.chunk)
        .map(|s| &s.window)
}

/// The chunks of the layout.
pub fn chunk_count(transcript: &Transcript) -> usize {
    transcript.spans.iter().filter(|s| s.chunk).count()
}
