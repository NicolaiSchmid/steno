//! The ONNX Runtime backend: pyannote segmentation 3.0 and `WeSpeaker`
//! ResNet34-LM from the sherpa-onnx exports, with the fbank front end of
//! [`crate::fbank`] in front of the embedding model. Runs on every
//! platform; the Mac uses it only when the `CoreML` backend is not built.

use std::path::Path;

use ort::session::Session;
use ort::value::Tensor;

use crate::backend::{BackendError, SegmentationGeometry, TensorBackend, to_f64};
use crate::error::DiarizeError;
use crate::fbank::{Fbank, FbankConfig};
use crate::models::{ModelStore, PYANNOTE_SEGMENTATION_3_0, WESPEAKER_RESNET34_LM};

/// Fbank frames the embedding model is given at least; under that the
/// window's speaker is skipped (a quarter of a second).
const MIN_EMBEDDING_FRAMES: usize = 25;

/// Both sessions plus what their metadata says about the inputs.
pub struct OnnxBackend {
    segmentation: Session,
    embedding: Session,
    geometry: SegmentationGeometry,
    fbank: Fbank,
    /// Whether the embedding model expects samples in the 16-bit range
    /// (`normalize_samples = 0` in the sherpa-onnx metadata); the fbank
    /// scale is set from it.
    scales_samples: bool,
    /// Whether the per-utterance mean is subtracted from the features,
    /// `WeSpeaker`'s `apply_cmvn`.
    subtracts_mean: bool,
    embedding_dimension: usize,
}

impl std::fmt::Debug for OnnxBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnnxBackend")
            .field("geometry", &self.geometry)
            .field("scales_samples", &self.scales_samples)
            .field("subtracts_mean", &self.subtracts_mean)
            .field("embedding_dimension", &self.embedding_dimension)
            .finish_non_exhaustive()
    }
}

impl OnnxBackend {
    /// Loads both models from the store, fetching them when needed.
    pub fn from_store(store: &ModelStore, threads: usize) -> Result<Self, DiarizeError> {
        let segmentation = store.ensure(&PYANNOTE_SEGMENTATION_3_0)?;
        let embedding = store.ensure(&WESPEAKER_RESNET34_LM)?;
        OnnxBackend::load(&segmentation, &embedding, threads)
    }

    /// Loads the two model files. `threads` is the intra-op thread count
    /// of each session; zero lets ONNX Runtime decide.
    pub fn load(
        segmentation: &Path,
        embedding: &Path,
        threads: usize,
    ) -> Result<Self, DiarizeError> {
        let segmentation = session(segmentation, threads)?;
        let embedding = session(embedding, threads)?;
        let geometry = geometry_of(&segmentation)?;
        let metadata = embedding.metadata().map_err(DiarizeError::backend)?;
        let framework = metadata.custom("framework").unwrap_or_default();
        let scales_samples = metadata
            .custom("normalize_samples")
            .is_none_or(|value| value.trim() == "0");
        let subtracts_mean = framework == "wespeaker"
            || metadata
                .custom("feature_normalize_type")
                .is_some_and(|value| value == "global-mean");
        let embedding_dimension = metadata
            .custom("output_dim")
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(steno_core::Embedding::DIMENSION);
        let sample_rate: usize = metadata
            .custom("sample_rate")
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(geometry.sample_rate);
        if sample_rate != geometry.sample_rate {
            return Err(DiarizeError::metadata(format!(
                "the embedding model wants {sample_rate} Hz, the segmentation model {} Hz",
                geometry.sample_rate
            )));
        }
        drop(metadata);
        let mut config = FbankConfig::WESPEAKER;
        if !scales_samples {
            config.sample_scale = 1.0;
        }
        Ok(OnnxBackend {
            segmentation,
            embedding,
            geometry,
            fbank: Fbank::new(config),
            scales_samples,
            subtracts_mean,
            embedding_dimension,
        })
    }

    #[must_use]
    pub fn embedding_dimension(&self) -> usize {
        self.embedding_dimension
    }

    /// The fbank frames of `window` the segmentation `weights` mark: each
    /// feature frame takes the weight of the segmentation frame whose
    /// centre is nearest its own. The mean is subtracted over the whole
    /// ten-second window before the frames are picked, as pyannote's
    /// `WeSpeaker` wrapper and `FluidAudio`'s `FBank` model normalise
    /// before the mask is applied; normalising the selected frames alone
    /// would centre every speaker's features on their own voice and
    /// discard part of what tells voices apart.
    fn selected_features(&self, window: &[f32], weights: &[f32]) -> Vec<f32> {
        let mut features = self.fbank.compute(window);
        let bins = self.fbank.num_bins();
        if self.subtracts_mean {
            Fbank::subtract_mean(&mut features, bins);
        }
        let frames = features.len() / bins.max(1);
        let geometry = &self.geometry;
        let mut selected = Vec::new();
        for frame in 0..frames {
            let centre = self.fbank.frame_centre_seconds(frame) * to_f64(geometry.sample_rate);
            let shifted = centre - to_f64(geometry.receptive_field_size / 2);
            // Non-negative after the clamp; the index stays far below 2^53.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let segmentation_frame = ((shifted / to_f64(geometry.receptive_field_shift))
                .round()
                .max(0.0) as usize)
                .min(geometry.frames_per_window.saturating_sub(1));
            if weights
                .get(segmentation_frame)
                .is_some_and(|weight| *weight > 0.5)
            {
                selected.extend_from_slice(&features[frame * bins..(frame + 1) * bins]);
            }
        }
        selected
    }
}

fn session(path: &Path, threads: usize) -> Result<Session, DiarizeError> {
    let mut builder = Session::builder().map_err(DiarizeError::backend)?;
    if threads > 0 {
        // The builder error carries the builder back and is not `Send`;
        // its message is what matters.
        builder = builder
            .with_intra_threads(threads)
            .map_err(|error| DiarizeError::backend(error.to_string()))?;
    }
    builder
        .commit_from_file(path)
        .map_err(DiarizeError::backend)
}

/// The geometry from the segmentation model's sherpa-onnx metadata, the
/// pyannote 3.0 constants for anything missing.
fn geometry_of(session: &Session) -> Result<SegmentationGeometry, DiarizeError> {
    let metadata = session.metadata().map_err(DiarizeError::backend)?;
    let number = |key: &str, fallback: usize| -> usize {
        metadata
            .custom(key)
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(fallback)
    };
    let defaults = SegmentationGeometry::PYANNOTE_3_0;
    let mut geometry = SegmentationGeometry {
        sample_rate: number("sample_rate", defaults.sample_rate),
        window_samples: number("window_size", defaults.window_samples),
        frames_per_window: 0,
        receptive_field_size: number("receptive_field_size", defaults.receptive_field_size),
        receptive_field_shift: number("receptive_field_shift", defaults.receptive_field_shift),
        num_speakers: number("num_speakers", defaults.num_speakers),
        num_classes: number("num_classes", defaults.num_classes),
    };
    if geometry.receptive_field_shift == 0
        || geometry.window_samples < geometry.receptive_field_size
    {
        return Err(DiarizeError::metadata(
            "the segmentation model's metadata is unusable",
        ));
    }
    geometry.frames_per_window = (geometry.window_samples - geometry.receptive_field_size)
        / geometry.receptive_field_shift
        + 1;
    let max_classes = number("powerset_max_classes", 2);
    if max_classes != 2 {
        return Err(DiarizeError::metadata(format!(
            "the segmentation model allows {max_classes} simultaneous speakers; this crate decodes 2"
        )));
    }
    Ok(geometry)
}

fn to_i64(count: usize) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

impl TensorBackend for OnnxBackend {
    fn geometry(&self) -> &SegmentationGeometry {
        &self.geometry
    }

    fn segment(&mut self, window: &[f32]) -> Result<Vec<f32>, BackendError> {
        let expected = self.geometry.window_samples;
        if window.len() != expected {
            return Err(format!(
                "segmentation window has {} samples, expected {expected}",
                window.len()
            )
            .into());
        }
        let input = Tensor::from_array((vec![1i64, 1, to_i64(expected)], window.to_vec()))?;
        let name = self.segmentation.inputs()[0].name().to_owned();
        let outputs = self.segmentation.run(ort::inputs![name => input])?;
        let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;
        let frames = self.geometry.frames_per_window;
        let classes = self.geometry.num_classes;
        if shape.len() != 3 || shape[1] != to_i64(frames) || shape[2] != to_i64(classes) {
            return Err(format!(
                "segmentation output has shape {shape:?}, expected [1, {frames}, {classes}]"
            )
            .into());
        }
        Ok(data.to_vec())
    }

    fn embed(&mut self, window: &[f32], weights: &[f32]) -> Result<Option<Vec<f32>>, BackendError> {
        let features = self.selected_features(window, weights);
        let bins = self.fbank.num_bins();
        let frames = features.len() / bins;
        if frames < MIN_EMBEDDING_FRAMES {
            return Ok(None);
        }
        let input = Tensor::from_array((vec![1i64, to_i64(frames), to_i64(bins)], features))?;
        let name = self.embedding.inputs()[0].name().to_owned();
        let outputs = self.embedding.run(ort::inputs![name => input])?;
        let (_, data) = outputs[0].try_extract_tensor::<f32>()?;
        if data.iter().any(|value| !value.is_finite()) {
            return Ok(None);
        }
        Ok(Some(data.to_vec()))
    }
}
