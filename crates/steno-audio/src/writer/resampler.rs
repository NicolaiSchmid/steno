//! 48 kHz to 16 kHz by exact 3:1 decimation through a Kaiser-windowed sinc
//! low-pass (192 taps, cutoff 7.3 kHz, stopband from about 8 kHz).
//! Swift: `Sources/StenoAudio/Writer/Resampler48kTo16k.swift`.
//!
//! Deterministic on every machine, every buffer allocated in `new`, so it
//! runs on the writer thread without allocation and its output is
//! byte-identical between CI and a Mac. One instance per lane; it keeps the
//! filter history between calls.

#[derive(Debug, Clone)]
pub struct Resampler48kTo16k {
    frame_size: usize,
    output_frame_size: usize,
    coefficients: Vec<f32>,
    history: Vec<f32>,
}

impl Resampler48kTo16k {
    pub const FACTOR: usize = 3;
    pub const TAPS: usize = 192;

    /// `frame_size` must be a multiple of 3.
    #[must_use]
    pub fn new(frame_size: usize) -> Self {
        assert!(
            frame_size.is_multiple_of(Self::FACTOR),
            "frame_size must be a multiple of 3"
        );
        Self {
            frame_size,
            output_frame_size: frame_size / Self::FACTOR,
            coefficients: Self::kaiser_sinc(Self::TAPS, 7_300.0 / 48_000.0, 9.0),
            history: vec![0.0; Self::TAPS - 1 + frame_size],
        }
    }

    #[must_use]
    pub fn frame_size(&self) -> usize {
        self.frame_size
    }

    /// Output samples per call: `frame_size / 3`.
    #[must_use]
    pub fn output_frame_size(&self) -> usize {
        self.output_frame_size
    }

    /// Consumes exactly `frame_size` input samples and produces
    /// `output_frame_size` Int16 samples (the sidecar format) in `output`,
    /// clamped to full scale.
    pub fn process(&mut self, input: &[f32], output: &mut [i16]) {
        let taps = Self::TAPS;
        let offset = taps - 1;
        self.history[offset..offset + self.frame_size].copy_from_slice(&input[..self.frame_size]);
        for (n, out) in output[..self.output_frame_size].iter_mut().enumerate() {
            // The newest sample of this output's window sits at 3n + offset.
            let newest = n * Self::FACTOR + offset;
            let mut accumulator: f32 = 0.0;
            for (k, coefficient) in self.coefficients.iter().enumerate() {
                accumulator += coefficient * self.history[newest - k];
            }
            // Clamped to [-1, 1] first, so the cast cannot truncate.
            *out = (accumulator.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        }
        // Keep the last taps - 1 input samples for the next call.
        self.history
            .copy_within(self.frame_size..self.frame_size + offset, 0);
    }

    /// Zeroes the history (a new recording).
    pub fn reset(&mut self) {
        self.history.fill(0.0);
    }

    /// Windowed-sinc low-pass, unity DC gain. `cutoff` is normalised to the
    /// input rate.
    #[must_use]
    pub fn kaiser_sinc(taps: usize, cutoff: f64, beta: f64) -> Vec<f32> {
        // Tap counts are tiny.
        let centre = (taps - 1) as f64 / 2.0;
        let denominator = Self::bessel_i0(beta);
        let mut coefficients = vec![0.0f64; taps];
        for (index, coefficient) in coefficients.iter_mut().enumerate() {
            let x = index as f64 - centre;
            let sinc = if x == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
            };
            let ratio = 2.0 * index as f64 / (taps - 1) as f64 - 1.0;
            let window = Self::bessel_i0(beta * (1.0 - ratio * ratio).sqrt()) / denominator;
            *coefficient = sinc * window;
        }
        let sum: f64 = coefficients.iter().sum();
        // The coefficients are small and the sum is near one.
        coefficients.iter().map(|c| (c / sum) as f32).collect()
    }

    /// Zeroth-order modified Bessel function of the first kind (series).
    #[must_use]
    pub fn bessel_i0(x: f64) -> f64 {
        let mut sum = 1.0;
        let mut term = 1.0;
        let half = x / 2.0;
        let mut k = 1.0;
        while term > 1e-12 * sum {
            term *= (half / k) * (half / k);
            sum += term;
            k += 1.0;
        }
        sum
    }
}
