//! The `CoreML` backend over `FluidAudio`'s compiled diarization models
//! (`Segmentation.mlmodelc`, `FBank.mlmodelc`, `Embedding.mlmodelc` from
//! the `speaker-diarization` repository the Swift app installs in
//! [`model_directory`]), so the Mac keeps the embeddings the Swift app
//! stored. The segmentation model takes `audio` `[1, 1, 160000]` and
//! returns `[1, 589, 7]` powerset logits; `FBank` turns the window into
//! `fbank_features`; `Embedding` takes those and per-frame `weights`
//! `[1, 589]` and returns `embedding` `[1, 256]`, so the mask is applied
//! inside the model rather than by selecting frames. The leading
//! dimension of every input is the batch; it is pinned to 1 as
//! `FluidAudio`'s `SegmentationProcessor` does, whatever default the model
//! declares (the models accept 1 to 32 and `Segmentation.mlmodelc`
//! declares 32). macOS only; the bindings live in `binding`, the one
//! module in this crate allowed `unsafe` (the crate denies it elsewhere).

#[allow(unsafe_code)]
mod binding;

use std::path::{Path, PathBuf};

use binding::{Array, Model};
use steno_core::StenoPaths;

use crate::backend::{BackendError, DiarizationBackend, SegmentationGeometry};
use crate::error::DiarizeError;

/// `<support>/Models/fluidaudio/speaker-diarization`: where the Swift
/// app's model store installs these models, its default models root plus
/// `ModelAsset.offlineDiarizer`'s `frameworkRoot` and `modelFolder`
/// (`Sources/StenoSpeech/Models/ModelAsset.swift`). A models directory
/// moved in the Swift app's settings is not followed.
#[must_use]
pub fn model_directory(paths: &StenoPaths) -> PathBuf {
    paths
        .support_directory
        .join("Models")
        .join("fluidaudio")
        .join("speaker-diarization")
}

/// The three models.
pub struct CoreMlBackend {
    segmentation: Model,
    fbank: Model,
    embedding: Model,
    geometry: SegmentationGeometry,
    segmentation_input: Vec<usize>,
    fbank_input: Vec<usize>,
    weights_input: Vec<usize>,
}

impl std::fmt::Debug for CoreMlBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreMlBackend")
            .field("geometry", &self.geometry)
            .finish_non_exhaustive()
    }
}

impl CoreMlBackend {
    /// Loads the three `.mlmodelc` directories from `models_dir`.
    pub fn load(models_dir: &Path) -> Result<Self, DiarizeError> {
        let load = |name: &str| Model::load(&models_dir.join(name)).map_err(DiarizeError::backend);
        let segmentation = load("Segmentation.mlmodelc")?;
        let fbank = load("FBank.mlmodelc")?;
        let embedding = load("Embedding.mlmodelc")?;
        let geometry = SegmentationGeometry::PYANNOTE_3_0;
        let segmentation_input = one_window(
            segmentation
                .input_shape("audio")
                .unwrap_or_else(|| vec![1, 1, geometry.window_samples]),
        );
        let fbank_input = one_window(
            fbank
                .input_shape("audio")
                .unwrap_or_else(|| vec![1, geometry.window_samples]),
        );
        let weights_input = one_window(
            embedding
                .input_shape("weights")
                .unwrap_or_else(|| vec![1, geometry.frames_per_window]),
        );
        let window = *segmentation_input.last().unwrap_or(&0);
        if window != geometry.window_samples {
            return Err(DiarizeError::metadata(format!(
                "Segmentation.mlmodelc takes {window} samples, expected {}",
                geometry.window_samples
            )));
        }
        Ok(CoreMlBackend {
            segmentation,
            fbank,
            embedding,
            geometry,
            segmentation_input,
            fbank_input,
            weights_input,
        })
    }
}

/// `shape` with its leading (batch) dimension set to 1: the pipeline
/// feeds one window per prediction, as `FluidAudio` does, whatever batch
/// the model declares as its default.
fn one_window(mut shape: Vec<usize>) -> Vec<usize> {
    if let Some(batch) = shape.first_mut() {
        *batch = 1;
    }
    shape
}

impl DiarizationBackend for CoreMlBackend {
    fn geometry(&self) -> &SegmentationGeometry {
        &self.geometry
    }

    fn segment(&mut self, window: &[f32]) -> Result<Vec<f32>, BackendError> {
        if window.len() != self.geometry.window_samples {
            return Err(format!(
                "segmentation window has {} samples, expected {}",
                window.len(),
                self.geometry.window_samples
            )
            .into());
        }
        let input = Array::from_f32(&self.segmentation_input, window)?;
        let output = self.segmentation.predict(&[("audio", &input)])?;
        let logits = output
            .array("segments")
            .or_else(|_| output.array("log_probs"))
            .or_else(|_| output.first_array())?;
        let values = logits.to_f32()?;
        let expected = self.geometry.frames_per_window * self.geometry.num_classes;
        if values.len() != expected {
            return Err(format!(
                "Segmentation.mlmodelc returned {} values (shape {:?}), expected {expected}",
                values.len(),
                logits.shape()
            )
            .into());
        }
        Ok(values)
    }

    fn embed(&mut self, window: &[f32], weights: &[f32]) -> Result<Option<Vec<f32>>, BackendError> {
        if weights.iter().all(|weight| *weight <= 0.0) {
            return Ok(None);
        }
        let audio = Array::from_f32(&self.fbank_input, window)?;
        let features = self.fbank.predict(&[("audio", &audio)])?;
        let fbank = features.array("fbank_features")?;
        // The weights land on the model's frame count, nearest frame.
        let frames = *self.weights_input.last().unwrap_or(&weights.len());
        let mut resampled = vec![0.0f32; frames];
        for (index, target) in resampled.iter_mut().enumerate() {
            let source = index * weights.len() / frames.max(1);
            *target = weights.get(source).copied().unwrap_or(0.0);
        }
        let mask = Array::from_f32(&self.weights_input, &resampled)?;
        let output = self
            .embedding
            .predict(&[("fbank_features", &fbank), ("weights", &mask)])?;
        // The length is checked once for both backends, in extraction.
        let embedding = output.array("embedding")?.to_f32()?;
        if embedding.iter().any(|value| !value.is_finite()) {
            return Ok(None);
        }
        Ok(Some(embedding))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_models_sit_where_the_swift_app_installs_them() {
        assert_eq!(
            model_directory(&StenoPaths::new("/tmp/support")),
            PathBuf::from("/tmp/support/Models/fluidaudio/speaker-diarization")
        );
    }
}
