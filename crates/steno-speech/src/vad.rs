//! Voice activity detection: Silero VAD through ONNX Runtime (the model
//! file from the sherpa-onnx `asr-models` release, fetched by the model
//! store) and an energy detector for tests. Both yield per-window speech
//! probabilities that [`regions_from_probabilities`] turns into regions
//! with Silero's own hysteresis: speech starts at the first window over the
//! threshold and ends after `min_silence_seconds` under the negative
//! threshold; regions shorter than `min_speech_seconds` are dropped and the
//! rest padded by `pad_seconds`. Ported from `spikes/onnx-speech/src/vad.rs`,
//! which went through the sherpa-onnx C API.
//! Swift: none; the Swift app has no VAD in front of Parakeet.

use std::ops::Range;
use std::path::Path;

use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;

use crate::backend::{FRAME_SAMPLES, sample_count};
use crate::error::SpeechError;
use crate::onnx::{OnnxOptions, open_session, outlet_tensor};

/// Samples per Silero window at 16 kHz.
pub const WINDOW: usize = 512;

/// Speech regions as sample ranges, sorted and non-overlapping.
pub trait VoiceActivityDetector: Send {
    fn speech_regions(&mut self, samples: &[f32]) -> Result<Vec<Range<usize>>, SpeechError>;
}

/// The defaults are the sherpa-onnx configuration the spike D harness ran
/// (`spikes/onnx-speech/src/vad.rs` and `main.rs`: threshold 0.5, minimum
/// silence 0.25 s, minimum speech 0.1 s), not Silero's own 100 ms of
/// silence and 250 ms of speech; the negative threshold and the padding
/// are Silero's.
#[derive(Debug, Clone, PartialEq)]
pub struct VadConfig {
    /// A window at or over this probability is speech.
    pub threshold: f32,
    /// Speech ends only when the probability falls under this
    /// (Silero's `threshold - 0.15`).
    pub negative_threshold: f32,
    /// How long the probability must stay under the negative threshold
    /// before the region closes.
    pub min_silence_seconds: f32,
    /// Regions shorter than this are dropped.
    pub min_speech_seconds: f32,
    /// Added before and after every kept region.
    pub pad_seconds: f32,
}

impl Default for VadConfig {
    fn default() -> Self {
        VadConfig {
            threshold: 0.5,
            negative_threshold: 0.35,
            min_silence_seconds: 0.25,
            min_speech_seconds: 0.1,
            pad_seconds: 0.03,
        }
    }
}

/// Regions from one probability per `window` samples over `total` samples.
#[must_use]
pub fn regions_from_probabilities(
    probabilities: &[f32],
    window: usize,
    total: usize,
    config: &VadConfig,
) -> Vec<Range<usize>> {
    let min_silence = sample_count(config.min_silence_seconds);
    let min_speech = sample_count(config.min_speech_seconds);
    let pad = sample_count(config.pad_seconds);
    let mut raw: Vec<Range<usize>> = Vec::new();
    let mut start: Option<usize> = None;
    let mut silence_since: Option<usize> = None;
    for (i, &p) in probabilities.iter().enumerate() {
        let position = i * window;
        match start {
            None => {
                if p >= config.threshold {
                    start = Some(position);
                }
            }
            Some(s) => {
                if p >= config.threshold {
                    silence_since = None;
                } else if p < config.negative_threshold {
                    let since = *silence_since.get_or_insert(position);
                    if position - since >= min_silence {
                        raw.push(s..since);
                        start = None;
                        silence_since = None;
                    }
                }
            }
        }
    }
    if let Some(s) = start {
        let end = silence_since
            .unwrap_or(probabilities.len() * window)
            .min(total);
        raw.push(s..end);
    }
    let mut regions: Vec<Range<usize>> = Vec::new();
    for region in raw {
        if region.len() < min_speech || region.is_empty() {
            continue;
        }
        let padded = region.start.saturating_sub(pad)..(region.end + pad).min(total);
        match regions.last_mut() {
            Some(last) if padded.start <= last.end => last.end = last.end.max(padded.end),
            _ => regions.push(padded),
        }
    }
    regions
}

/// How the model carries its recurrent state.
enum StateLayout {
    /// v4: `h` and `c`, `[2, 1, 64]` each.
    Separate { h: String, c: String, len: usize },
    /// v5: one `state` of `[2, 1, 128]`, and 64 samples of context before
    /// each window.
    Combined { name: String, len: usize },
}

/// Silero VAD, v4 or v5, over a session of its own.
pub struct SileroVad {
    session: Session,
    config: VadConfig,
    input: String,
    /// The `sr` input and whether it is a scalar; the k2-fsa re-export of
    /// v4 (16 kHz branch only) has none.
    sample_rate: Option<(String, bool)>,
    state: StateLayout,
    context: usize,
}

impl SileroVad {
    /// Loads `silero_vad.onnx` (or the v5 file) and reads its input layout.
    pub fn load(
        path: &Path,
        options: &OnnxOptions,
        config: VadConfig,
    ) -> Result<Self, SpeechError> {
        let session = open_session(path, options)?;
        let mut input = None;
        let mut sample_rate = None;
        let mut h = None;
        let mut c = None;
        let mut combined = None;
        for outlet in session.inputs() {
            let Some((_, shape)) = outlet_tensor(outlet.dtype()) else {
                continue;
            };
            let len = shape
                .iter()
                .map(|&d| if d > 0 { d.unsigned_abs() as usize } else { 1 })
                .product::<usize>();
            match outlet.name() {
                "input" | "x" => input = Some(outlet.name().to_owned()),
                "sr" => sample_rate = Some((outlet.name().to_owned(), shape.is_empty())),
                "h" => h = Some((outlet.name().to_owned(), len)),
                "c" => c = Some((outlet.name().to_owned(), len)),
                "state" => combined = Some((outlet.name().to_owned(), len)),
                _ => {}
            }
        }
        let Some(input) = input else {
            return Err(SpeechError::Shape(format!(
                "{}: not a Silero VAD model (no `input` or `x` input)",
                path.display()
            )));
        };
        let (state, context) = match (h, c, combined) {
            (Some((h, len)), Some((c, _)), None) => (StateLayout::Separate { h, c, len }, 0),
            (None, None, Some((name, len))) => (StateLayout::Combined { name, len }, 64),
            _ => {
                return Err(SpeechError::Shape(format!(
                    "{}: unknown Silero state layout",
                    path.display()
                )));
            }
        };
        Ok(SileroVad {
            session,
            config,
            input,
            sample_rate,
            state,
            context,
        })
    }

    /// One speech probability per [`WINDOW`] samples; the last window is
    /// zero-padded.
    pub fn probabilities(&mut self, samples: &[f32]) -> Result<Vec<f32>, SpeechError> {
        let (mut h, mut c) = match &self.state {
            StateLayout::Separate { len, .. } => (vec![0.0f32; *len], vec![0.0f32; *len]),
            StateLayout::Combined { len, .. } => (vec![0.0f32; *len], Vec::new()),
        };
        let separate = matches!(self.state, StateLayout::Separate { .. });
        let wanted = if separate { 3 } else { 2 };
        let mut context = vec![0.0f32; self.context];
        let mut frame = vec![0.0f32; self.context + WINDOW];
        let mut probabilities = Vec::with_capacity(samples.len() / WINDOW + 1);
        for window in samples.chunks(WINDOW) {
            frame[..self.context].copy_from_slice(&context);
            frame[self.context..self.context + window.len()].copy_from_slice(window);
            frame[self.context + window.len()..].fill(0.0);
            let mut inputs: Vec<(&str, SessionInputValue<'static>)> = vec![(
                self.input.as_str(),
                Tensor::from_array(([1, frame.len()], frame.clone()))?.into(),
            )];
            if let Some((name, scalar)) = &self.sample_rate {
                inputs.push((
                    name.as_str(),
                    if *scalar {
                        Tensor::from_array(((), vec![16_000i64]))?.into()
                    } else {
                        Tensor::from_array(([1usize], vec![16_000i64]))?.into()
                    },
                ));
            }
            match &self.state {
                StateLayout::Separate {
                    h: h_name,
                    c: c_name,
                    len,
                } => {
                    inputs.push((
                        h_name.as_str(),
                        Tensor::from_array(([2, 1, len / 2], h.clone()))?.into(),
                    ));
                    inputs.push((
                        c_name.as_str(),
                        Tensor::from_array(([2, 1, len / 2], c.clone()))?.into(),
                    ));
                }
                StateLayout::Combined { name, len } => {
                    inputs.push((
                        name.as_str(),
                        Tensor::from_array(([2, 1, len / 2], h.clone()))?.into(),
                    ));
                }
            }
            let outputs = self.session.run(inputs)?;
            if outputs.len() < wanted {
                return Err(SpeechError::Shape(format!(
                    "Silero VAD returned {} outputs, expected {wanted}",
                    outputs.len()
                )));
            }
            let (_, probability) = outputs[0].try_extract_tensor::<f32>()?;
            probabilities.push(probability.first().copied().unwrap_or(0.0));
            let (_, next_h) = outputs[1].try_extract_tensor::<f32>()?;
            copy_state(&mut h, next_h)?;
            if separate {
                let (_, next_c) = outputs[2].try_extract_tensor::<f32>()?;
                copy_state(&mut c, next_c)?;
            }
            if self.context > 0 {
                context.copy_from_slice(&frame[frame.len() - self.context..]);
            }
        }
        Ok(probabilities)
    }
}

/// Carries a state output into the next step; a length other than the
/// declared one is a model-shape error, not a panic on the model path.
fn copy_state(state: &mut [f32], next: &[f32]) -> Result<(), SpeechError> {
    if next.len() != state.len() {
        return Err(SpeechError::Shape(format!(
            "Silero VAD state output has {} values, the input takes {}",
            next.len(),
            state.len()
        )));
    }
    state.copy_from_slice(next);
    Ok(())
}

impl VoiceActivityDetector for SileroVad {
    fn speech_regions(&mut self, samples: &[f32]) -> Result<Vec<Range<usize>>, SpeechError> {
        let probabilities = self.probabilities(samples)?;
        Ok(regions_from_probabilities(
            &probabilities,
            WINDOW,
            samples.len(),
            &self.config,
        ))
    }
}

/// A detector on RMS energy per window, for tests and synthetic audio.
#[derive(Debug, Clone, PartialEq)]
pub struct EnergyVad {
    /// RMS at or over this is speech.
    pub rms_threshold: f32,
    pub config: VadConfig,
}

impl Default for EnergyVad {
    fn default() -> Self {
        EnergyVad {
            rms_threshold: 0.01,
            config: VadConfig::default(),
        }
    }
}

/// An [`EnergyVad`] whose threshold follows the recording's own level
/// ([`adaptive_rms_threshold`]), so a quiet lane is not taken for
/// silence. The `CoreML` engine runs it: Silero runs on ONNX Runtime,
/// which the Mac app keeps out of its process (plan invariant 4).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AdaptiveEnergyVad {
    pub config: VadConfig,
}

impl VoiceActivityDetector for AdaptiveEnergyVad {
    fn speech_regions(&mut self, samples: &[f32]) -> Result<Vec<Range<usize>>, SpeechError> {
        EnergyVad {
            rms_threshold: adaptive_rms_threshold(samples),
            config: self.config.clone(),
        }
        .speech_regions(samples)
    }
}

/// [`adaptive_rms_threshold`]'s bounds and scale: about -66 and -42 dBFS,
/// and 0.3 of the 75th percentile.
const ADAPTIVE_RMS_FLOOR: f32 = 0.0005;
const ADAPTIVE_RMS_CEILING: f32 = 0.008;
const ADAPTIVE_RMS_SCALE: f32 = 0.3;
const ADAPTIVE_RMS_PERCENTILE: f64 = 0.75;

/// A speech threshold scaled to the recording's level: 0.3 of the 75th
/// percentile of the per-frame (80 ms) RMS over the frames that are not
/// digital silence, clamped to about -66 to -42 dBFS; the ceiling for a
/// recording that is all silence.
/// Swift: `FluidAudio`'s `ChunkProcessor.adaptiveSpeechRmsThreshold`.
#[must_use]
pub fn adaptive_rms_threshold(samples: &[f32]) -> f32 {
    let mut frame_rms: Vec<f32> = samples
        .as_chunks::<FRAME_SAMPLES>()
        .0
        .iter()
        .map(|frame| frame.iter().map(|x| x * x).sum::<f32>() / FRAME_SAMPLES as f32)
        .filter(|mean_square| *mean_square > 0.0)
        .map(f32::sqrt)
        .collect();
    if frame_rms.is_empty() {
        return ADAPTIVE_RMS_CEILING;
    }
    frame_rms.sort_by(f32::total_cmp);
    let index =
        ((frame_rms.len() as f64 * ADAPTIVE_RMS_PERCENTILE) as usize).min(frame_rms.len() - 1);
    (frame_rms[index] * ADAPTIVE_RMS_SCALE).clamp(ADAPTIVE_RMS_FLOOR, ADAPTIVE_RMS_CEILING)
}

impl VoiceActivityDetector for EnergyVad {
    fn speech_regions(&mut self, samples: &[f32]) -> Result<Vec<Range<usize>>, SpeechError> {
        let probabilities: Vec<f32> = samples
            .chunks(WINDOW)
            .map(|window| {
                let rms = (window.iter().map(|x| x * x).sum::<f32>() / window.len() as f32).sqrt();
                if rms >= self.rms_threshold { 1.0 } else { 0.0 }
            })
            .collect();
        Ok(regions_from_probabilities(
            &probabilities,
            WINDOW,
            samples.len(),
            &self.config,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> VadConfig {
        VadConfig {
            min_silence_seconds: 0.1, // 1600 samples, about three windows
            min_speech_seconds: 0.05,
            pad_seconds: 0.0,
            ..VadConfig::default()
        }
    }

    #[test]
    fn speech_starts_at_the_threshold_and_ends_after_the_minimum_silence() {
        // 10 windows: speech on 2..=5, one dip at 6 that is too short, speech 7, silence after.
        let p = [
            0.0, 0.1, 0.9, 0.8, 0.6, 0.7, 0.2, 0.9, 0.1, 0.1, 0.1, 0.1, 0.0,
        ];
        let regions = regions_from_probabilities(&p, WINDOW, p.len() * WINDOW, &config());
        assert_eq!(regions, vec![2 * WINDOW..8 * WINDOW]);
    }

    #[test]
    fn the_negative_threshold_keeps_speech_through_a_soft_dip() {
        let p = [0.9, 0.4, 0.4, 0.4, 0.4, 0.4, 0.9, 0.0, 0.0, 0.0, 0.0];
        let regions = regions_from_probabilities(&p, WINDOW, p.len() * WINDOW, &config());
        assert_eq!(regions, vec![0..7 * WINDOW]);
    }

    #[test]
    fn short_bursts_are_dropped_and_padding_merges_neighbours() {
        let short = VadConfig {
            min_speech_seconds: 0.1, // 1600 samples: four windows
            ..config()
        };
        let p = [0.9, 0.0, 0.0, 0.0, 0.0];
        assert!(regions_from_probabilities(&p, WINDOW, p.len() * WINDOW, &short).is_empty());
        let padded = VadConfig {
            pad_seconds: 0.1,
            ..config()
        };
        let p = [0.9, 0.9, 0.0, 0.0, 0.0, 0.0, 0.9, 0.9, 0.0, 0.0, 0.0, 0.0];
        let regions = regions_from_probabilities(&p, WINDOW, p.len() * WINDOW, &padded);
        assert_eq!(regions, vec![0..(8 * WINDOW + 1600).min(12 * WINDOW)]);
    }

    #[test]
    fn speech_running_to_the_end_closes_at_the_last_sample() {
        let p = [0.0, 0.9, 0.9];
        assert_eq!(
            regions_from_probabilities(&p, WINDOW, 1400, &config()),
            vec![WINDOW..1400]
        );
        assert!(regions_from_probabilities(&[], WINDOW, 0, &config()).is_empty());
    }

    #[test]
    fn the_energy_detector_finds_a_burst() {
        let mut samples = vec![0.0f32; 16_000];
        for (i, x) in samples[4_000..8_000].iter_mut().enumerate() {
            *x = if i % 2 == 0 { 0.3 } else { -0.3 };
        }
        let mut vad = EnergyVad {
            config: VadConfig {
                pad_seconds: 0.0,
                ..VadConfig::default()
            },
            ..EnergyVad::default()
        };
        let regions = vad.speech_regions(&samples).unwrap();
        assert_eq!(regions.len(), 1);
        assert!(regions[0].start <= 4_000 && regions[0].start + WINDOW > 4_000);
        assert!(regions[0].end >= 8_000 && regions[0].end < 8_000 + WINDOW);
    }

    #[test]
    fn the_adaptive_threshold_follows_the_level_within_its_bounds() {
        // Speech at 0.004 RMS (about -48 dBFS, under the fixed 0.01) over
        // digital silence, which the percentile skips.
        let mut quiet = vec![0.0f32; 16_000];
        for (i, x) in quiet[4_000..12_000].iter_mut().enumerate() {
            *x = if i % 2 == 0 { 0.004 } else { -0.004 };
        }
        assert!((adaptive_rms_threshold(&quiet) - 0.0012).abs() < 1e-6);
        let regions = AdaptiveEnergyVad {
            config: VadConfig {
                pad_seconds: 0.0,
                ..VadConfig::default()
            },
        }
        .speech_regions(&quiet)
        .unwrap();
        assert_eq!(regions.len(), 1, "{regions:?}");
        assert!(regions[0].start <= 4_000 && regions[0].end >= 11_776);
        assert!(
            EnergyVad::default()
                .speech_regions(&quiet)
                .unwrap()
                .is_empty()
        );
        assert_eq!(adaptive_rms_threshold(&vec![0.5; 16_000]), 0.008);
        assert_eq!(adaptive_rms_threshold(&vec![0.0001; 16_000]), 0.0005);
        assert_eq!(adaptive_rms_threshold(&[0.0; 4_000]), 0.008);
    }
}
