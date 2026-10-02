//! The fbank front end of the `WeSpeaker` embedding model, computed the way
//! the model was trained (`wespeaker/dataset/processor.py`,
//! `compute_fbank` and `apply_cmvn`): `torchaudio.compliance.kaldi.fbank`
//! over samples scaled to the 16-bit range, 80 mel bins between 20 Hz and
//! the Nyquist frequency, 25 ms Hamming frames every 10 ms with the edges
//! snipped, no dither, the DC offset removed, pre-emphasis 0.97, the log of
//! the mel energies floored at `f32::EPSILON`, then the mean over the
//! utterance subtracted per bin. sherpa-onnx's generic extractor differs
//! (Povey window, 7.6 kHz upper edge, centred frames); those are its
//! defaults, not the recipe, so this module follows the recipe.

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::backend::to_f64;

/// The window applied to each frame before the FFT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowType {
    /// `0.54 - 0.46 cos(2πn / (N - 1))`, what `WeSpeaker` trains with.
    Hamming,
    /// `(0.5 - 0.5 cos(2πn / (N - 1)))^0.85`, Kaldi's and sherpa-onnx's default.
    Povey,
}

/// The knobs of the extractor; [`FbankConfig::WESPEAKER`] is the recipe.
#[derive(Debug, Clone, PartialEq)]
pub struct FbankConfig {
    pub sample_rate: usize,
    pub frame_length_ms: f64,
    pub frame_shift_ms: f64,
    pub num_bins: usize,
    pub low_freq: f64,
    /// Upper mel edge; zero or negative means the Nyquist frequency plus
    /// the value, as in Kaldi.
    pub high_freq: f64,
    pub preemphasis: f32,
    pub remove_dc_offset: bool,
    pub window: WindowType,
    /// Multiplied into every sample first: `WeSpeaker` models expect the
    /// 16-bit integer range, Steno's buffers hold `-1...1`.
    pub sample_scale: f32,
}

impl FbankConfig {
    /// `compute_fbank` in `wespeaker/dataset/processor.py` with dither off,
    /// as `extract.py` forces at inference.
    pub const WESPEAKER: FbankConfig = FbankConfig {
        sample_rate: 16_000,
        frame_length_ms: 25.0,
        frame_shift_ms: 10.0,
        num_bins: 80,
        low_freq: 20.0,
        high_freq: 0.0,
        preemphasis: 0.97,
        remove_dc_offset: true,
        window: WindowType::Hamming,
        sample_scale: 32_768.0,
    };
}

/// One mel filter: its weights over a contiguous run of FFT bins.
#[derive(Debug, Clone)]
struct MelFilter {
    first_bin: usize,
    weights: Vec<f32>,
}

/// Log mel filterbank features, one row of [`Fbank::num_bins`] per frame.
pub struct Fbank {
    config: FbankConfig,
    frame_length: usize,
    frame_shift: usize,
    fft_size: usize,
    window: Vec<f32>,
    filters: Vec<MelFilter>,
    fft: Arc<dyn Fft<f32>>,
}

impl std::fmt::Debug for Fbank {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fbank")
            .field("config", &self.config)
            .field("fft_size", &self.fft_size)
            .finish_non_exhaustive()
    }
}

impl Fbank {
    #[must_use]
    pub fn new(config: FbankConfig) -> Self {
        let frame_length = ms_to_samples(config.frame_length_ms, config.sample_rate);
        let frame_shift = ms_to_samples(config.frame_shift_ms, config.sample_rate).max(1);
        let fft_size = frame_length.next_power_of_two();
        let window = window(config.window, frame_length);
        let filters = mel_filters(&config, fft_size);
        let fft = FftPlanner::new().plan_fft_forward(fft_size);
        Fbank {
            config,
            frame_length,
            frame_shift,
            fft_size,
            window,
            filters,
            fft,
        }
    }

    /// The recipe's extractor.
    #[must_use]
    pub fn wespeaker() -> Self {
        Fbank::new(FbankConfig::WESPEAKER)
    }

    #[must_use]
    pub fn num_bins(&self) -> usize {
        self.config.num_bins
    }

    #[must_use]
    pub fn frame_shift(&self) -> usize {
        self.frame_shift
    }

    #[must_use]
    pub fn frame_length(&self) -> usize {
        self.frame_length
    }

    /// Frames `samples` samples yield with the edges snipped: every frame
    /// lies fully inside the audio.
    #[must_use]
    pub fn num_frames(&self, samples: usize) -> usize {
        if samples < self.frame_length {
            0
        } else {
            1 + (samples - self.frame_length) / self.frame_shift
        }
    }

    /// The time at the centre of `frame`.
    #[must_use]
    pub fn frame_centre_seconds(&self, frame: usize) -> f64 {
        to_f64(frame * self.frame_shift + self.frame_length / 2) / to_f64(self.config.sample_rate)
    }

    /// Log mel energies, `num_frames(samples.len()) × num_bins`, row-major,
    /// before mean normalisation.
    #[must_use]
    pub fn compute(&self, samples: &[f32]) -> Vec<f32> {
        let frames = self.num_frames(samples.len());
        let mut features = Vec::with_capacity(frames * self.config.num_bins);
        let mut frame = vec![0.0f32; self.frame_length];
        let mut spectrum = vec![Complex::new(0.0f32, 0.0); self.fft_size];
        let mut scratch = vec![Complex::new(0.0f32, 0.0); self.fft.get_inplace_scratch_len()];
        let mut power = vec![0.0f32; self.fft_size / 2 + 1];
        for index in 0..frames {
            let start = index * self.frame_shift;
            for (target, sample) in frame
                .iter_mut()
                .zip(&samples[start..start + self.frame_length])
            {
                *target = sample * self.config.sample_scale;
            }
            self.process_frame(&mut frame);
            for (target, value) in spectrum.iter_mut().zip(&frame) {
                *target = Complex::new(*value, 0.0);
            }
            for target in &mut spectrum[self.frame_length..] {
                *target = Complex::new(0.0, 0.0);
            }
            self.fft.process_with_scratch(&mut spectrum, &mut scratch);
            for (target, value) in power.iter_mut().zip(&spectrum) {
                *target = value.norm_sqr();
            }
            for filter in &self.filters {
                let energy: f32 = filter
                    .weights
                    .iter()
                    .zip(&power[filter.first_bin..])
                    .map(|(weight, value)| weight * value)
                    .sum();
                features.push(energy.max(f32::EPSILON).ln());
            }
        }
        features
    }

    /// Subtracts the mean over the frames per bin, `apply_cmvn` with
    /// `norm_mean` only; a no-op on no frames.
    pub fn subtract_mean(features: &mut [f32], num_bins: usize) {
        if num_bins == 0 || features.len() < num_bins {
            return;
        }
        let frames = features.len() / num_bins;
        let mut means = vec![0.0f32; num_bins];
        for row in features.chunks_exact(num_bins) {
            for (mean, value) in means.iter_mut().zip(row) {
                *mean += value;
            }
        }
        // Frame counts stay far below 2^24, where f32 would lose them.
        #[allow(clippy::cast_precision_loss)]
        let count = frames as f32;
        for mean in &mut means {
            *mean /= count;
        }
        for row in features.chunks_exact_mut(num_bins) {
            for (value, mean) in row.iter_mut().zip(&means) {
                *value -= mean;
            }
        }
    }

    /// DC offset, pre-emphasis and the window, in Kaldi's order.
    fn process_frame(&self, frame: &mut [f32]) {
        if self.config.remove_dc_offset {
            // A frame has 400 samples; the sum stays exact in f32.
            #[allow(clippy::cast_precision_loss)]
            let mean = frame.iter().sum::<f32>() / frame.len() as f32;
            for value in frame.iter_mut() {
                *value -= mean;
            }
        }
        if self.config.preemphasis != 0.0 {
            let coefficient = self.config.preemphasis;
            for index in (1..frame.len()).rev() {
                frame[index] -= coefficient * frame[index - 1];
            }
            frame[0] -= coefficient * frame[0];
        }
        for (value, weight) in frame.iter_mut().zip(&self.window) {
            *value *= weight;
        }
    }
}

fn ms_to_samples(milliseconds: f64, sample_rate: usize) -> usize {
    // Positive and tiny (hundreds of samples); the cast is exact.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let samples = (milliseconds * to_f64(sample_rate) / 1000.0).round() as usize;
    samples
}

fn window(kind: WindowType, length: usize) -> Vec<f32> {
    let denominator = to_f64(length.saturating_sub(1)).max(1.0);
    (0..length)
        .map(|index| {
            let phase = 2.0 * std::f64::consts::PI * to_f64(index) / denominator;
            let value = match kind {
                WindowType::Hamming => 0.54 - 0.46 * phase.cos(),
                WindowType::Povey => (0.5 - 0.5 * phase.cos()).powf(0.85),
            };
            // Window weights lie in 0...1; the narrowing is intended.
            #[allow(clippy::cast_possible_truncation)]
            let weight = value as f32;
            weight
        })
        .collect()
}

/// Kaldi's mel scale.
fn mel(frequency: f64) -> f64 {
    1127.0 * (1.0 + frequency / 700.0).ln()
}

/// Kaldi's triangular filters over the FFT bins below Nyquist
/// (`MelBanks::MelBanks`): equal width on the mel scale, each spanning
/// three successive edges.
fn mel_filters(config: &FbankConfig, fft_size: usize) -> Vec<MelFilter> {
    let nyquist = to_f64(config.sample_rate) / 2.0;
    let high = if config.high_freq > 0.0 {
        config.high_freq
    } else {
        nyquist + config.high_freq
    };
    let mel_low = mel(config.low_freq);
    let mel_high = mel(high);
    let delta = (mel_high - mel_low) / to_f64(config.num_bins + 1);
    let bin_width = to_f64(config.sample_rate) / to_f64(fft_size);
    let num_fft_bins = fft_size / 2;
    (0..config.num_bins)
        .map(|bin| {
            let left = mel_low + to_f64(bin) * delta;
            let centre = left + delta;
            let right = centre + delta;
            let mut first_bin = None;
            let mut weights = Vec::new();
            for fft_bin in 0..num_fft_bins {
                let frequency_mel = mel(to_f64(fft_bin) * bin_width);
                let weight = if frequency_mel > left && frequency_mel < right {
                    if frequency_mel <= centre {
                        (frequency_mel - left) / (centre - left)
                    } else {
                        (right - frequency_mel) / (right - centre)
                    }
                } else {
                    0.0
                };
                if weight > 0.0 {
                    first_bin.get_or_insert(fft_bin);
                    // Filter weights lie in 0...1.
                    #[allow(clippy::cast_possible_truncation)]
                    weights.push(weight as f32);
                } else if first_bin.is_some() {
                    break;
                }
            }
            MelFilter {
                first_bin: first_bin.unwrap_or(0),
                weights,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_snipped_and_shifted_by_ten_milliseconds() {
        let fbank = Fbank::wespeaker();
        assert_eq!(fbank.frame_length(), 400);
        assert_eq!(fbank.frame_shift(), 160);
        assert_eq!(fbank.num_frames(399), 0);
        assert_eq!(fbank.num_frames(400), 1);
        assert_eq!(fbank.num_frames(160_000), 998);
        assert!((fbank.frame_centre_seconds(0) - 0.0125).abs() < 1e-9);
    }

    #[test]
    fn filters_cover_twenty_hertz_to_nyquist_without_gaps() {
        let fbank = Fbank::wespeaker();
        assert_eq!(fbank.filters.len(), 80);
        // Every FFT bin between the edges is covered by some filter; the
        // weights of the two filters sharing a bin sum to one.
        let mut coverage = vec![0.0f32; 256];
        for filter in &fbank.filters {
            for (offset, weight) in filter.weights.iter().enumerate() {
                coverage[filter.first_bin + offset] += weight;
            }
        }
        // The top filter's falling edge ends at Nyquist with no filter
        // after it, so the last bins taper off.
        for (bin, total) in coverage.iter().enumerate().skip(2).take(240) {
            assert!((total - 1.0).abs() < 1e-4, "bin {bin}: {total}");
        }
    }

    #[test]
    fn a_pure_tone_peaks_in_the_filter_at_its_frequency() {
        let fbank = Fbank::wespeaker();
        #[allow(clippy::cast_precision_loss)]
        let tone: Vec<f32> = (0..16_000)
            .map(|index| {
                (2.0 * std::f32::consts::PI * 1000.0 * index as f32 / 16_000.0).sin() * 0.5
            })
            .collect();
        let features = fbank.compute(&tone);
        assert_eq!(features.len(), fbank.num_frames(16_000) * 80);
        let row = &features[..80];
        let peak = row
            .iter()
            .enumerate()
            .max_by(|lhs, rhs| lhs.1.total_cmp(rhs.1))
            .map(|(bin, _)| bin)
            .unwrap();
        // The centre of filter `peak` should sit near 1 kHz on the mel scale.
        let delta = (mel(8000.0) - mel(20.0)) / 81.0;
        let centre_mel = mel(20.0) + to_f64(peak + 1) * delta;
        let centre_hz = 700.0 * ((centre_mel / 1127.0).exp() - 1.0);
        assert!(
            (centre_hz - 1000.0).abs() < 60.0,
            "peak filter centred at {centre_hz} Hz"
        );
        // Everything far from the tone is much quieter.
        assert!(row[peak] - row[70] > 5.0);
    }

    #[test]
    fn silence_floors_at_epsilon_and_mean_subtraction_centres_each_bin() {
        let fbank = Fbank::wespeaker();
        let silence = vec![0.0f32; 4_000];
        let features = fbank.compute(&silence);
        assert!(
            features
                .iter()
                .all(|value| (value - f32::EPSILON.ln()).abs() < 1e-6)
        );

        let mut mixed = vec![1.0, 2.0, 3.0, 5.0, 6.0, 7.0];
        Fbank::subtract_mean(&mut mixed, 3);
        assert_eq!(mixed, vec![-2.0, -2.0, -2.0, 2.0, 2.0, 2.0]);
        let mut empty: Vec<f32> = Vec::new();
        Fbank::subtract_mean(&mut empty, 80);
        assert_eq!(empty.len(), 0);
    }

    #[test]
    fn the_povey_window_is_available_for_the_kaldi_default() {
        let hamming = window(WindowType::Hamming, 400);
        let povey = window(WindowType::Povey, 400);
        assert!((hamming[0] - 0.08).abs() < 1e-6);
        assert!(povey[0].abs() < 1e-6);
        assert!((hamming[200] - 1.0).abs() < 1e-4 && (povey[200] - 1.0).abs() < 1e-4);
    }
}
