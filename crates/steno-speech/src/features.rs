//! The `NeMo` `AudioToMelSpectrogramPreprocessor` of Parakeet TDT 0.6B v3 in
//! Rust: pre-emphasis 0.97, a centred 512-point STFT with reflect padding
//! and a 25 ms symmetric Hann window at a 10 ms hop, 128 Slaney mel bands
//! from 0 to 8 kHz, the natural log with a `2^-24` guard, and per-feature
//! normalisation over the window. The export's encoder was traced behind
//! this preprocessor, so these are its training-time values; spike F
//! showed sherpa-onnx's Kaldi-style settings scoring the same, so the
//! differences sit below the model's sensitivity.
//! Swift: none; `FluidAudio` ships this step as `Preprocessor.mlmodelc`.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

use crate::backend::Features;

pub use crate::chunker::SAMPLE_RATE;
pub const N_FFT: usize = 512;
pub const WINDOW_LENGTH: usize = 400;
pub const HOP_LENGTH: usize = 160;
pub const N_MELS: usize = 128;
const PRE_EMPHASIS: f32 = 0.97;
/// `log_zero_guard_value`, `2^-24`.
const LOG_GUARD: f32 = 5.960_464_5e-8;
/// Added to the standard deviation before dividing.
const NORMALISATION_GUARD: f32 = 1e-5;
const BINS: usize = N_FFT / 2 + 1;

/// The preprocessor with its FFT plan, window and filterbank; one per
/// backend, reused across windows.
pub struct MelExtractor {
    fft: Arc<dyn RealToComplex<f32>>,
    /// The Hann window zero-padded to `N_FFT`, as `torch.stft` pads it.
    window: Vec<f32>,
    /// `N_MELS` rows of `BINS` weights.
    filters: Vec<f32>,
    input: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
}

impl Default for MelExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MelExtractor {
    #[must_use]
    pub fn new() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(N_FFT);
        let input = fft.make_input_vec();
        let spectrum = fft.make_output_vec();
        let scratch = fft.make_scratch_vec();
        MelExtractor {
            fft,
            window: padded_hann(),
            filters: mel_filters(),
            input,
            spectrum,
            scratch,
        }
    }

    /// Frames for `samples` samples: one per hop plus one, as the centred
    /// STFT yields (`NeMo`'s `get_seq_len`). Zero for no samples.
    #[must_use]
    pub const fn frame_count(samples: usize) -> usize {
        if samples == 0 {
            0
        } else {
            samples / HOP_LENGTH + 1
        }
    }

    /// Mel features for one window of 16 kHz samples.
    pub fn features(&mut self, samples: &[f32]) -> Features {
        let frames = Self::frame_count(samples.len());
        if frames == 0 {
            return Features {
                mels: N_MELS,
                frames: 0,
                data: Vec::new(),
            };
        }
        let padded = reflect_pad(&pre_emphasise(samples), N_FFT / 2);
        let mut data = vec![0.0f32; N_MELS * frames];
        let mut power = [0.0f32; BINS];
        for t in 0..frames {
            let start = t * HOP_LENGTH;
            for (slot, (x, w)) in self
                .input
                .iter_mut()
                .zip(padded[start..start + N_FFT].iter().zip(&self.window))
            {
                *slot = x * w;
            }
            self.fft
                .process_with_scratch(&mut self.input, &mut self.spectrum, &mut self.scratch)
                .expect("buffer lengths come from the planner");
            for (p, c) in power.iter_mut().zip(&self.spectrum) {
                *p = c.norm_sqr();
            }
            for (m, row) in self.filters.as_chunks::<BINS>().0.iter().enumerate() {
                let energy: f32 = row.iter().zip(&power).map(|(f, p)| f * p).sum();
                data[m * frames + t] = (energy + LOG_GUARD).ln();
            }
        }
        normalise_per_feature(&mut data, frames);
        Features {
            mels: N_MELS,
            frames,
            data,
        }
    }
}

/// `x[n] - 0.97 x[n-1]`, the first sample unchanged.
fn pre_emphasise(samples: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(samples.len());
    let mut previous = 0.0;
    for (i, &x) in samples.iter().enumerate() {
        out.push(if i == 0 {
            x
        } else {
            x - PRE_EMPHASIS * previous
        });
        previous = x;
    }
    out
}

/// `pad` samples of reflection (edge excluded, as `torch.nn.functional.pad`
/// with `reflect`) on both sides; shorter inputs reflect repeatedly.
fn reflect_pad(samples: &[f32], pad: usize) -> Vec<f32> {
    let len = samples.len();
    let mut out = Vec::with_capacity(len + 2 * pad);
    for i in 0..len + 2 * pad {
        // Signed index into `samples`, folded back into range.
        let mut index = i as isize - pad as isize;
        let last = len as isize - 1;
        if last <= 0 {
            index = 0;
        } else {
            while index < 0 || index > last {
                if index < 0 {
                    index = -index;
                }
                if index > last {
                    index = 2 * last - index;
                }
            }
        }
        out.push(samples[index.unsigned_abs()]);
    }
    out
}

/// Per mel band over the window: subtract the mean, divide by the unbiased
/// standard deviation plus the guard (`NeMo`'s `normalize_batch`,
/// `per_feature`).
fn normalise_per_feature(data: &mut [f32], frames: usize) {
    for row in data.chunks_exact_mut(frames) {
        let count = row.len() as f64;
        let mean = row.iter().map(|&x| f64::from(x)).sum::<f64>() / count;
        let variance = if row.len() > 1 {
            row.iter()
                .map(|&x| (f64::from(x) - mean).powi(2))
                .sum::<f64>()
                / (count - 1.0)
        } else {
            0.0
        };
        let scale = 1.0 / (variance.sqrt() as f32 + NORMALISATION_GUARD);
        for x in row {
            *x = (*x - mean as f32) * scale;
        }
    }
}

/// `torch.hann_window(400, periodic=False)` centred in `N_FFT` zeros.
fn padded_hann() -> Vec<f32> {
    let mut window = vec![0.0f32; N_FFT];
    let offset = (N_FFT - WINDOW_LENGTH) / 2;
    for (n, slot) in window[offset..offset + WINDOW_LENGTH]
        .iter_mut()
        .enumerate()
    {
        let phase = 2.0 * std::f64::consts::PI * n as f64 / (WINDOW_LENGTH - 1) as f64;
        *slot = (0.5 - 0.5 * phase.cos()) as f32;
    }
    window
}

const MEL_BREAK_HZ: f64 = 1000.0;
const MEL_LINEAR_STEP: f64 = 200.0 / 3.0;

fn mel_log_step() -> f64 {
    6.4f64.ln() / 27.0
}

/// Slaney mel scale (`librosa.hz_to_mel`, `htk=False`).
fn hz_to_mel(hz: f64) -> f64 {
    if hz >= MEL_BREAK_HZ {
        MEL_BREAK_HZ / MEL_LINEAR_STEP + (hz / MEL_BREAK_HZ).ln() / mel_log_step()
    } else {
        hz / MEL_LINEAR_STEP
    }
}

fn mel_to_hz(mel: f64) -> f64 {
    let break_mel = MEL_BREAK_HZ / MEL_LINEAR_STEP;
    if mel >= break_mel {
        MEL_BREAK_HZ * (mel_log_step() * (mel - break_mel)).exp()
    } else {
        MEL_LINEAR_STEP * mel
    }
}

/// `librosa.filters.mel(sr=16000, n_fft=512, n_mels=128, fmin=0, fmax=8000)`
/// with Slaney normalisation, in the row-major layout the extractor reads.
fn mel_filters() -> Vec<f32> {
    let f_max = SAMPLE_RATE as f64 / 2.0;
    let (mel_low, mel_high) = (hz_to_mel(0.0), hz_to_mel(f_max));
    let edges: Vec<f64> = (0..N_MELS + 2)
        .map(|i| mel_to_hz(mel_low + (mel_high - mel_low) * i as f64 / (N_MELS + 1) as f64))
        .collect();
    let bin_hz: Vec<f64> = (0..BINS)
        .map(|k| k as f64 * SAMPLE_RATE as f64 / N_FFT as f64)
        .collect();
    let mut filters = vec![0.0f32; N_MELS * BINS];
    for (m, row) in filters.as_chunks_mut::<BINS>().0.iter_mut().enumerate() {
        let (low, centre, high) = (edges[m], edges[m + 1], edges[m + 2]);
        let normalisation = 2.0 / (high - low);
        for (slot, &f) in row.iter_mut().zip(&bin_hz) {
            let rising = (f - low) / (centre - low);
            let falling = (high - f) / (high - centre);
            *slot = (rising.min(falling).max(0.0) * normalisation) as f32;
        }
    }
    filters
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, seconds: f32) -> Vec<f32> {
        (0..(seconds * SAMPLE_RATE as f32) as usize)
            .map(|n| (2.0 * std::f32::consts::PI * hz * n as f32 / SAMPLE_RATE as f32).sin())
            .collect()
    }

    #[test]
    fn frame_count_follows_the_centred_stft() {
        assert_eq!(MelExtractor::frame_count(0), 0);
        assert_eq!(MelExtractor::frame_count(160), 2);
        assert_eq!(MelExtractor::frame_count(16_000), 101);
        assert_eq!(MelExtractor::frame_count(16_159), 101);
    }

    #[test]
    fn the_filterbank_is_slaney_normalised_and_covers_the_band() {
        let filters = mel_filters();
        // Every band has weight; Slaney normalisation makes the area of a
        // triangle 2 / width, so the sum over bins times the bin width is
        // about 1 for every band once the band is wider than a few bins
        // (the lowest bands are narrower than the 31 Hz bin spacing).
        for (m, row) in filters.as_chunks::<BINS>().0.iter().enumerate().skip(24) {
            let sum: f32 = row.iter().sum();
            let bin_width = SAMPLE_RATE as f32 / N_FFT as f32;
            assert!(
                (sum * bin_width - 1.0).abs() < 0.3,
                "band {m} sums to {}",
                sum * bin_width
            );
        }
        assert!((hz_to_mel(1000.0) - 15.0).abs() < 1e-9);
        assert!((mel_to_hz(hz_to_mel(3_456.0)) - 3_456.0).abs() < 1e-6);
    }

    #[test]
    fn a_tone_peaks_in_the_band_that_holds_it_and_the_window_is_normalised() {
        let mut extractor = MelExtractor::new();
        let features = extractor.features(&tone(1_000.0, 1.0));
        assert_eq!(features.frames, 101);
        assert_eq!(features.data.len(), N_MELS * 101);
        // Normalisation: every band has mean 0 over the window.
        for row in features.data.chunks_exact(features.frames) {
            let mean = row.iter().sum::<f32>() / row.len() as f32;
            assert!(mean.abs() < 1e-3);
        }
        // Before normalisation the band under 1 kHz has the energy; check
        // through the filterbank directly.
        let filters = mel_filters();
        let bin = (1_000.0 / (SAMPLE_RATE as f32 / N_FFT as f32)).round() as usize;
        let best = (0..N_MELS)
            .max_by(|&a, &b| {
                filters[a * BINS + bin]
                    .partial_cmp(&filters[b * BINS + bin])
                    .unwrap()
            })
            .unwrap();
        let edges_hz = mel_to_hz(hz_to_mel(0.0) + hz_to_mel(8_000.0) * (best + 1) as f64 / 129.0);
        assert!(
            (edges_hz - 1_000.0).abs() < 60.0,
            "band {best} centred at {edges_hz}"
        );
    }

    #[test]
    fn reflect_padding_mirrors_without_repeating_the_edge() {
        assert_eq!(
            reflect_pad(&[1.0, 2.0, 3.0, 4.0], 2),
            vec![3.0, 2.0, 1.0, 2.0, 3.0, 4.0, 3.0, 2.0]
        );
        assert_eq!(reflect_pad(&[7.0], 2), vec![7.0; 5]);
        assert_eq!(reflect_pad(&[1.0, 2.0], 3).len(), 8);
        let emphasised = pre_emphasise(&[1.0, 1.0, 1.0]);
        assert_eq!(emphasised[0], 1.0);
        assert!(emphasised[1..].iter().all(|x| (x - 0.03).abs() < 1e-6));
    }

    #[test]
    fn silence_yields_zero_features_not_nan() {
        let mut extractor = MelExtractor::new();
        let features = extractor.features(&vec![0.0; 3_200]);
        assert!(features.data.iter().all(|x| x.is_finite()));
        assert!(extractor.features(&[]).data.is_empty());
    }
}
