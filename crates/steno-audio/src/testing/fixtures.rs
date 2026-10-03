//! The 48 kHz synthetic signals the audio tests and `aec-bench --synthetic`
//! use. Swift: `Sources/StenoAudio/Testing/AudioFixtures.swift`.
//!
//! Tones and sweeps from integer phase accumulators, a speech-like far-end
//! from seeded SplitMix64 noise, a seeded room impulse response, and the
//! echoed microphone built from them. Deterministic on every machine;
//! never committed, always built in test setup. Same seeds and arithmetic
//! as Swift, so the Speex ERLE table matches `steno dev aec-bench
//! --synthetic` (proven by the spike, re-asserted in `tests/aec.rs`).

use crate::aec::EchoMetrics;

pub struct AudioFixtures;

/// SplitMix64, as `Sources/StenoCore/Testing/SplitMix64.swift`.
#[derive(Debug, Clone)]
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)` from the top 53 bits.
    pub fn unit(&mut self) -> f64 {
        // 53 bits fit an f64 mantissa exactly.
        let numerator = (self.next() >> 11) as f64;
        numerator / (1u64 << 53) as f64
    }
}

impl AudioFixtures {
    pub const SAMPLE_RATE: f64 = 48_000.0;

    /// A sine of `frequency` for `seconds` at `amplitude`.
    #[must_use]
    pub fn tone(frequency: f64, seconds: f64, amplitude: f64) -> Vec<f32> {
        let count = (seconds * Self::SAMPLE_RATE) as usize;
        let increment = Self::phase_increment(frequency);
        let mut phase: u32 = 0;
        (0..count)
            .map(|_| {
                let sample = (amplitude * Self::sine(phase)) as f32;
                phase = phase.wrapping_add(increment);
                sample
            })
            .collect()
    }

    /// A linear sine sweep.
    #[must_use]
    pub fn sweep(start: f64, end: f64, seconds: f64, amplitude: f64) -> Vec<f32> {
        let count = (seconds * Self::SAMPLE_RATE) as usize;
        let mut phase: u32 = 0;
        (0..count)
            .map(|index| {
                let progress = index as f64 / (count.max(2) - 1) as f64;
                let sample = (amplitude * Self::sine(phase)) as f32;
                phase = phase.wrapping_add(Self::phase_increment(start + (end - start) * progress));
                sample
            })
            .collect()
    }

    /// Seeded white noise in `-amplitude...amplitude`.
    #[must_use]
    pub fn noise(seconds: f64, seed: u64, amplitude: f64) -> Vec<f32> {
        let mut generator = SplitMix64(seed);
        let count = (seconds * Self::SAMPLE_RATE) as usize;
        (0..count)
            .map(|_| (amplitude * (generator.unit() * 2.0 - 1.0)) as f32)
            .collect()
    }

    /// Broadband, syllable-modulated noise: seeded noise through a one-pole
    /// low-pass (`tilt` is the pole), gated by a 4 Hz raised-cosine envelope
    /// with a pause every fourth syllable, scaled so the loudest sample is
    /// `peak`. Enough excitation for an adaptive filter to converge on.
    #[must_use]
    pub fn speech_like_far(seconds: f64) -> Vec<f32> {
        Self::speech_like_far_with(seconds, 0x5EED_0048, 0.7, 0.7)
    }

    #[must_use]
    pub fn speech_like_far_with(seconds: f64, seed: u64, peak: f64, tilt: f32) -> Vec<f32> {
        let raw = Self::noise(seconds, seed, 1.0);
        let syllable = Self::SAMPLE_RATE / 4.0;
        let mut state: f32 = 0.0;
        let mut samples = vec![0.0f32; raw.len()];
        for (index, sample) in samples.iter_mut().enumerate() {
            state = tilt * state + (1.0 - tilt) * raw[index];
            let position = (index as f64 % syllable) / syllable;
            let syllable_index = (index as f64 / syllable) as usize;
            let envelope = if syllable_index % 4 == 3 {
                0.0
            } else {
                0.5 * (1.0 - (2.0 * std::f64::consts::PI * position).cos())
            };
            *sample = envelope as f32 * state;
        }
        let loudest = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        if loudest > 0.0 {
            let scale = peak as f32 / loudest;
            for s in &mut samples {
                *s *= scale;
            }
        }
        samples
    }

    /// A room: unit direct path followed by 48 seeded discrete reflections
    /// within 100 ms, exponentially decaying to -60 dB at the end.
    #[must_use]
    pub fn room_impulse_response() -> Vec<f32> {
        Self::room_impulse_response_with(0.1, 48, 0.1, 0x1200_0000)
    }

    #[must_use]
    pub fn room_impulse_response_with(
        seconds: f64,
        reflections: usize,
        reflection_gain: f64,
        seed: u64,
    ) -> Vec<f32> {
        let count = (seconds * Self::SAMPLE_RATE) as usize;
        let mut generator = SplitMix64(seed);
        let mut response = vec![0.0f32; count];
        response[0] = 1.0;
        let decay = -3.0 * 10f64.ln() / count as f64;
        for _ in 0..reflections {
            let position = 1 + (generator.next() % (count as u64 - 1)) as usize;
            let unit = generator.unit();
            response[position] +=
                (reflection_gain * (decay * position as f64).exp() * (unit * 2.0 - 1.0)) as f32;
        }
        response
    }

    /// The microphone in a call with nobody talking: the far-end through
    /// the room after 60 ms at -6 dB, plus seeded noise at -60 dBFS.
    #[must_use]
    pub fn echo_mic(far: &[f32], impulse_response: &[f32]) -> Vec<f32> {
        Self::echo_mic_with(far, impulse_response, 0.060, 0.5, 0x0A0B_0C0D, 0.001)
    }

    #[must_use]
    pub fn echo_mic_with(
        far: &[f32],
        impulse_response: &[f32],
        delay: f64,
        echo_gain: f64,
        noise_seed: u64,
        noise_amplitude: f64,
    ) -> Vec<f32> {
        let delayed = EchoMetrics::convolve(
            far,
            impulse_response,
            (delay * Self::SAMPLE_RATE).round() as usize,
        );
        let floor = Self::noise(
            far.len() as f64 / Self::SAMPLE_RATE,
            noise_seed,
            noise_amplitude,
        );
        delayed
            .iter()
            .zip(&floor)
            .map(|(d, f)| echo_gain as f32 * d + f)
            .collect()
    }

    #[must_use]
    pub fn phase_increment(frequency: f64) -> u32 {
        (frequency / Self::SAMPLE_RATE * 4_294_967_296.0).round() as u32
    }

    #[must_use]
    pub fn sine(phase: u32) -> f64 {
        (2.0 * std::f64::consts::PI * f64::from(phase) / 4_294_967_296.0).sin()
    }
}
