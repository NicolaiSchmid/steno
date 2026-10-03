//! RMS, ERLE, Goertzel and convolution helpers shared by the echo canceller
//! tests, the live AEC path test and `aec-bench`.
//! Swift: `Sources/StenoAudio/AEC/EchoMetrics.swift`.

use std::ops::Range;

use steno_core::EchoCanceller;

use crate::realtime::LevelMeter;

/// Namespace for the measurement helpers; no state.
pub struct EchoMetrics;

impl EchoMetrics {
    /// Linear RMS of `samples`; 0 for an empty slice.
    #[must_use]
    pub fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let sum: f64 = samples.iter().map(|&s| f64::from(s * s)).sum();
        // Exact in f64 for any recording; the result fits f32.
        (sum / samples.len() as f64).sqrt() as f32
    }

    /// dBFS of a linear magnitude; `-160` for silence. The meter's scale.
    #[must_use]
    pub fn decibels(linear: f32) -> f32 {
        LevelMeter::decibels(linear)
    }

    /// Echo return loss enhancement over `range`: how much quieter the
    /// processed signal is than the unprocessed near-end, in dB. Positive is
    /// better; with nothing but echo on the near-end it measures
    /// cancellation.
    #[must_use]
    pub fn erle(near_end: &[f32], processed: &[f32], range: Range<usize>) -> f32 {
        let end = range.end.min(near_end.len()).min(processed.len());
        let start = range.start.min(end);
        let before = Self::rms(&near_end[start..end]);
        let after = Self::rms(&processed[start..end]);
        if before <= 0.0 {
            return 0.0;
        }
        Self::decibels(before) - Self::decibels(after)
    }

    /// The linear magnitude of one frequency in `samples` (Goertzel),
    /// scaled so a full-scale sine at `frequency` reads 1.
    #[must_use]
    pub fn tone_level(samples: &[f32], frequency: f64, sample_rate: f64) -> f32 {
        let count = samples.len();
        if count == 0 {
            return 0.0;
        }
        let omega = 2.0 * std::f64::consts::PI * frequency / sample_rate;
        let coefficient = 2.0 * omega.cos();
        let (mut previous, mut before_previous) = (0.0f64, 0.0f64);
        for &sample in samples {
            let current = f64::from(sample) + coefficient * previous - before_previous;
            before_previous = previous;
            previous = current;
        }
        let real = previous - before_previous * omega.cos();
        let imaginary = before_previous * omega.sin();
        ((real * real + imaginary * imaginary).sqrt() * 2.0 / count as f64) as f32
    }

    /// Direct-form convolution of `signal` with `impulse_response`,
    /// truncated to `signal.len()`, optionally delayed by `delay` samples
    /// (zeros first). Zero taps are skipped, so a sparse room costs one
    /// multiply per reflection per sample.
    #[must_use]
    pub fn convolve(signal: &[f32], impulse_response: &[f32], delay: usize) -> Vec<f32> {
        let mut output = vec![0.0f32; signal.len()];
        for (tap_index, &tap) in impulse_response
            .iter()
            .enumerate()
            .filter(|(_, t)| **t != 0.0)
        {
            let shift = tap_index + delay;
            if shift >= signal.len() {
                continue;
            }
            for n in shift..signal.len() {
                output[n] += signal[n - shift] * tap;
            }
        }
        output
    }

    /// Runs `canceller` over whole frames of `near_end` with `far_end` and
    /// returns the processed signal (the last partial frame is dropped).
    pub fn run(
        canceller: &mut dyn EchoCanceller,
        near_end: &[f32],
        far_end: &[f32],
        frame_size: usize,
    ) -> Vec<f32> {
        let frames = near_end.len().min(far_end.len()) / frame_size;
        let mut output = vec![0.0f32; frames * frame_size];
        for frame in 0..frames {
            let range = frame * frame_size..(frame + 1) * frame_size;
            canceller.process(
                &near_end[range.clone()],
                &far_end[range.clone()],
                &mut output[range],
            );
        }
        output
    }
}
