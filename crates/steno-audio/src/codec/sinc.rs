//! Arbitrary-ratio resampling through a Kaiser-windowed sinc with a
//! polyphase table: the pure-Rust stand-in for `AVAudioConverter` at
//! maximum quality on rates the exact 3:1 path does not cover (the phone's
//! 44.1 kHz). No Swift equivalent.
//!
//! For every output sample the input position `n * ratio` is split into an
//! integer index and a fraction; the fraction selects two adjacent
//! sub-filters of the table (`PHASES` per input sample) which are linearly
//! interpolated. 64 taps per phase at the output rate's Nyquist (cutoff
//! 0.45 of the lower rate), beta 9. Measured from 44.1 kHz in
//! `tests/codec.rs`: within 0.3 dB to 6 kHz, -1.3 dB at 6.5 kHz, 12 kHz
//! aliases below -50 dB (the design stopband is about -69 dB); plenty for
//! speech. The exact 3:1 FIR the 48 kHz path uses is the flat one.

use crate::writer::Resampler48kTo16k;

#[derive(Debug, Clone)]
pub struct SincResampler {
    ratio: f64,
    /// `PHASES + 1` sub-filters of `TAPS` coefficients, the last one equal
    /// to the first shifted by a sample so interpolation never reads past
    /// the table.
    table: Vec<f32>,
    taps: usize,
}

impl SincResampler {
    pub const PHASES: usize = 128;
    pub const TAPS: usize = 64;

    /// `input_rate` to `output_rate`, both in hertz.
    #[must_use]
    pub fn new(input_rate: f64, output_rate: f64) -> Self {
        let ratio = input_rate / output_rate;
        // The low-pass sits below the lower Nyquist, normalised to the
        // input rate; when upsampling the input's own band is the limit.
        let cutoff = 0.45 * output_rate.min(input_rate) / input_rate;
        let taps = Self::TAPS;
        let mut table = Vec::with_capacity((Self::PHASES + 1) * taps);
        for phase in 0..=Self::PHASES {
            // Phase counts are tiny.
            let fraction = phase as f64 / Self::PHASES as f64;
            let mut coefficients = Vec::with_capacity(taps);
            let centre = (taps / 2) as f64;
            let denominator = Resampler48kTo16k::bessel_i0(9.0);
            let mut sum = 0.0f64;
            for k in 0..taps {
                let x = k as f64 - centre + 1.0 - fraction;
                let sinc = if x == 0.0 {
                    2.0 * cutoff
                } else {
                    (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
                };
                let ratio_k = x / centre;
                let window = if ratio_k.abs() >= 1.0 {
                    0.0
                } else {
                    Resampler48kTo16k::bessel_i0(9.0 * (1.0 - ratio_k * ratio_k).sqrt())
                        / denominator
                };
                coefficients.push(sinc * window);
                sum += sinc * window;
            }
            // The coefficients are small; the sum is near 2 * cutoff.
            table.extend(coefficients.iter().map(|c| (c / sum) as f32));
        }
        Self { ratio, table, taps }
    }

    /// The whole signal; the output has `ceil(len / ratio)` samples, which
    /// the caller trims to the exact expected length.
    #[must_use]
    pub fn resample(&self, input: &[f32]) -> Vec<f32> {
        if input.is_empty() {
            return Vec::new();
        }
        let half = self.taps / 2;
        let count = (input.len() as f64 / self.ratio).ceil() as usize;
        let mut output = Vec::with_capacity(count);
        for n in 0..count {
            let position = n as f64 * self.ratio;
            let index = position.floor();
            let fraction = position - index;
            let index = index as usize;
            let phase = (fraction * Self::PHASES as f64) as usize;
            let blend = (fraction * Self::PHASES as f64 - phase as f64) as f32;
            let low = &self.table[phase * self.taps..(phase + 1) * self.taps];
            let high = &self.table[(phase + 1) * self.taps..(phase + 2) * self.taps];
            let mut accumulator = 0.0f32;
            for k in 0..self.taps {
                // Tap k reads the input sample at index - half + 1 + k.
                let Some(at) = (index + 1 + k).checked_sub(half) else {
                    continue;
                };
                if at >= input.len() {
                    continue;
                }
                let coefficient = low[k] + (high[k] - low[k]) * blend;
                accumulator += coefficient * input[at];
            }
            output.push(accumulator);
        }
        output
    }
}
