//! The resamplers' aliasing, measured with a sweep (stable plan A9): a
//! sine at 0.5 sampled at 44.1 kHz, swept linearly from 7.5 kHz to
//! 22.05 kHz at 1 kHz a second, through the two paths 44.1 kHz audio takes
//! to 16 kHz:
//!
//! - the decoder's, for the phone's recordings: the windowed sinc
//!   ([`SincResampler`](steno_audio::codec::sinc::SincResampler)) straight
//!   to 16 kHz;
//! - the capture's, for a device that runs at 44.1 kHz: the processing
//!   thread's [`RateConverter`] to 48 kHz, then the exact 3:1 FIR the
//!   master's decode and the live sidecar share.
//!
//! Above 8 kHz nothing of the sweep belongs in a 16 kHz lane, so every
//! sample that comes out of an input window above 8 kHz is aliasing: an
//! input at `f` lands at `16 kHz - f` below 16 kHz and at `f - 16 kHz`
//! above. The output is cut into 100 ms windows (100 Hz of the sweep each)
//! and each window's level is read against the sweep's.
//!
//! Measured: the capture path leaves nothing above -60 dB anywhere below
//! 8 kHz (-88 dB in the window from 8.0 to 8.1 kHz, then below the 16-bit
//! FIR's floor). The decoder's sinc keeps aliases under -60 dB wherever
//! they land below 7 kHz (-67 dB at 6.9 to 7.0 kHz, under -90 dB below
//! 6.8 kHz), but its transition band (cutoff 0.45 of 16 kHz, 64 taps)
//! folds 8 to 9 kHz into 7 to 8 kHz at -21 to -58 dB. That band is above
//! the flat passband (0.3 dB to 6 kHz), and `steno-speech`'s
//! `fleurs_wer_from_44k1_is_at_most_a_tenth_of_a_point_over_48k` measures
//! what it costs a transcript.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

mod common;

use steno_audio::codec::SymphoniaAudioCodec;
use steno_audio::realtime::RateConverter;

use common::level_against_sine;

const AMPLITUDE: f32 = 0.5;
const FROM: f64 = 7_500.0;
const TO: f64 = 22_050.0;
/// Hertz a second.
const SLOPE: f64 = 1_000.0;
/// Output samples a window: 100 ms at 16 kHz, 100 Hz of the sweep.
const WINDOW: usize = 1_600;

/// The sweep at 44.1 kHz.
fn sweep() -> Vec<f32> {
    let seconds = (TO - FROM) / SLOPE;
    let count = (seconds * 44_100.0) as usize;
    (0..count)
        .map(|i| {
            let t = i as f64 / 44_100.0;
            let phase = 2.0 * std::f64::consts::PI * (FROM * t + SLOPE * t * t / 2.0);
            (f64::from(AMPLITUDE) * phase.sin()) as f32
        })
        .collect()
}

/// One window of the 16 kHz output: the sweep's range over it and the
/// level against the sweep's.
struct Window {
    from: f64,
    to: f64,
    level: f32,
}

impl Window {
    /// Where the window's aliases land, in hertz: the lowest and highest.
    fn lands(&self) -> (f64, f64) {
        let fold = |f: f64| (16_000.0 - f).abs();
        let (a, b) = (fold(self.from), fold(self.to));
        (a.min(b), a.max(b))
    }
}

/// The output's whole windows above 8 kHz of input, short of the sweep's
/// last window (its end is a step, which spreads over the band).
fn windows(output: &[f32]) -> Vec<Window> {
    let span = WINDOW as f64 / 16_000.0 * SLOPE;
    output
        .as_chunks::<WINDOW>()
        .0
        .iter()
        .enumerate()
        .map(|(index, chunk)| {
            let from = FROM + index as f64 * span;
            Window {
                from,
                to: from + span,
                level: level_against_sine(chunk, AMPLITUDE),
            }
        })
        .filter(|w| w.from >= 8_000.0 && w.to <= TO - span)
        .collect()
}

fn print(name: &str, windows: &[Window]) {
    for w in windows {
        let (low, high) = w.lands();
        println!(
            "{name}: {:5.0} to {:5.0} Hz lands at {low:5.0} to {high:5.0} Hz, {:7.1} dB",
            w.from, w.to, w.level
        );
    }
}

/// The worst window among those whose aliases land wholly inside
/// `low..high` hertz.
fn worst(windows: &[Window], low: f64, high: f64) -> f32 {
    windows
        .iter()
        .filter(|w| {
            let (from, to) = w.lands();
            from >= low && to <= high
        })
        .map(|w| w.level)
        .fold(f32::NEG_INFINITY, f32::max)
}

#[test]
fn the_decoders_sinc_keeps_aliases_below_7_khz_under_60_db() {
    let output = SymphoniaAudioCodec::to_16k(&sweep(), 44_100);
    let windows = windows(&output);
    print("decoder", &windows);
    let speech = worst(&windows, 0.0, 7_000.0);
    assert!(speech < -60.0, "aliases below 7 kHz at {speech} dB");
    // The transition band, as the module doc states it.
    let top = worst(&windows, 7_000.0, 8_000.0);
    assert!(
        (-22.0..=-19.0).contains(&top),
        "8 to 9 kHz fold to 7 to 8 kHz at up to {top} dB"
    );
}

#[test]
fn the_capture_path_keeps_every_alias_below_8_khz_under_60_db() {
    let input = sweep();
    let mut converter = RateConverter::new(44_100.0, 48_000.0, 441);
    let mut master = Vec::with_capacity(input.len() * 48 / 44 + 64);
    let mut block = vec![0.0f32; converter.max_output()];
    for chunk in input.chunks(441) {
        let written = converter.process(chunk, &mut block);
        master.extend_from_slice(&block[..written]);
    }
    let output = SymphoniaAudioCodec::to_16k(&master, 48_000);
    let windows = windows(&output);
    print("capture", &windows);
    let band = worst(&windows, 0.0, 8_000.0);
    assert!(band < -60.0, "aliases below 8 kHz at {band} dB");
}
