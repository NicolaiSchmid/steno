//! The tone measurements the codec, processing and writer tests share.

#![allow(dead_code)]

use steno_audio::EchoMetrics;

/// The level of `samples` in dB against a sine of `amplitude`: 0 when a
/// tone of that amplitude kept its level.
pub fn level_against_sine(samples: &[f32], amplitude: f32) -> f32 {
    EchoMetrics::decibels(EchoMetrics::rms(samples) / (amplitude / 2f32.sqrt()))
}

/// Negative to non-negative zero crossings: a tone's whole periods.
pub fn upward_crossings(samples: &[f32]) -> usize {
    samples
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count()
}

/// A tone's frequency in hertz from its upward crossings over `samples`
/// at `sample_rate`.
pub fn frequency(samples: &[f32], sample_rate: f64) -> f64 {
    upward_crossings(samples) as f64 / (samples.len() as f64 / sample_rate)
}
