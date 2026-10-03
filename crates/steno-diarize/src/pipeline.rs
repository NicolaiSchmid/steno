//! The pipeline above the tensors: analysis, clustering, timeline, mapping
//! and refinement over one [`TensorBackend`].

use steno_core::{AudioBuffer16k, BoxError, ClusterChunk, DiarizationResult, Embedding};

use crate::backend::TensorBackend;
use crate::clustering::{self, ClusteringConfig};
use crate::error::DiarizeError;
use crate::extraction::{self, Analysis, ExtractionRules};
use crate::refinement::{self, Rules, SliceEmbedder};
use crate::to_f64;
use crate::{mapping, timeline};

/// The clustering knobs and whether the refinement pass runs, Steno's
/// `FluidDiarizerConfig` with the window step and the stage rules exposed
/// for the calibration harness. Swift:
/// `Sources/StenoSpeech/Diarization/FluidDiarizer.swift`.
#[derive(Debug, Clone, PartialEq)]
pub struct DiarizerConfig {
    /// Mean pairwise cosine distance between the members of two clusters
    /// (average linkage) at or below which they merge; see
    /// [`DEFAULT_CLUSTERING_THRESHOLD`] and [`crate::clustering`].
    pub clustering_threshold: f32,
    pub min_speakers: Option<usize>,
    pub max_speakers: Option<usize>,
    /// Whether the refinement pass runs after the mapping; off, the
    /// clusters come back as the mapping produced them.
    pub refines_clusters: bool,
    /// Seconds between two segmentation windows; `FluidAudio`'s community
    /// configuration steps a fifth of the ten-second window.
    pub step_seconds: f64,
    pub extraction: ExtractionRules,
    pub timeline: timeline::TimelineRules,
    pub refinement: Rules,
}

/// The clustering cut: `FluidAudio`'s Euclidean 0.8 on unit vectors as a
/// cosine distance (`d^2 = 2 - 2 cos`). The G3 sweep over the seven
/// calibration calls (gate G3 in
/// `.plans/2026-10-01-cross-platform-speech-stack.md`) passed at every
/// cut from 0.20 to 0.60 on both backends, so the derived value is kept
/// rather than tuned to the corpus.
pub const DEFAULT_CLUSTERING_THRESHOLD: f32 = 0.32;

impl Default for DiarizerConfig {
    fn default() -> Self {
        DiarizerConfig {
            clustering_threshold: DEFAULT_CLUSTERING_THRESHOLD,
            min_speakers: None,
            max_speakers: None,
            refines_clusters: true,
            step_seconds: 2.0,
            extraction: ExtractionRules::default(),
            timeline: timeline::TimelineRules::default(),
            refinement: Rules::default(),
        }
    }
}

impl DiarizerConfig {
    /// Audio shorter than this has nothing to cluster and yields no
    /// speakers without touching the models, as `FluidDiarizer` does.
    pub const MINIMUM_AUDIO_SECONDS: f64 = 1.0;

    fn clustering(&self, threshold: f32) -> ClusteringConfig {
        ClusteringConfig {
            threshold,
            min_speakers: self.min_speakers,
            max_speakers: self.max_speakers,
        }
    }

    fn step_samples(&self, sample_rate: usize) -> usize {
        // Positive seconds times the rate; nothing to truncate.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let step = (self.step_seconds * to_f64(sample_rate)).round().max(1.0) as usize;
        step
    }
}

/// One backend and one configuration; `diarize` runs the whole chain,
/// the pieces are public for the calibration harness, which analyses a
/// lane once and sweeps the clustering cut.
pub struct Pipeline<B: TensorBackend = Box<dyn TensorBackend>> {
    backend: B,
    config: DiarizerConfig,
}

impl<B: TensorBackend> Pipeline<B> {
    /// A pipeline over a loaded backend.
    #[must_use]
    pub fn new(backend: B, config: DiarizerConfig) -> Self {
        Pipeline { backend, config }
    }

    /// The configuration every call runs with.
    #[must_use]
    pub fn config(&self) -> &DiarizerConfig {
        &self.config
    }

    /// Replaces the configuration for the calls that follow; the backend
    /// stays loaded.
    pub fn set_config(&mut self, config: DiarizerConfig) {
        self.config = config;
    }

    /// The backend, for what it reports about itself (geometry, call
    /// counts in tests).
    #[must_use]
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Silence, room noise or a lane nobody spoke on is not a failed
    /// meeting: no usable embedding becomes a result with no speakers,
    /// like audio under [`DiarizerConfig::MINIMUM_AUDIO_SECONDS`].
    pub fn diarize(&mut self, audio: &AudioBuffer16k) -> Result<DiarizationResult, DiarizeError> {
        if audio.duration() < DiarizerConfig::MINIMUM_AUDIO_SECONDS {
            return Ok(DiarizationResult::default());
        }
        let analysis = self.analyze(audio)?;
        let mapped = self.map(&analysis, self.config.clustering_threshold);
        if !self.config.refines_clusters || mapped.clusters.is_empty() {
            return Ok(mapped);
        }
        self.refine(&mapped, audio)
    }

    /// Segmentation and embeddings for `audio`; no clustering yet.
    pub fn analyze(&mut self, audio: &AudioBuffer16k) -> Result<Analysis, DiarizeError> {
        let step = self
            .config
            .step_samples(self.backend.geometry().sample_rate);
        extraction::analyze(
            &mut self.backend,
            &audio.samples,
            step,
            &self.config.extraction,
        )
    }

    /// Clustering at `threshold`, the timeline and the mapping; the result
    /// before refinement.
    #[must_use]
    pub fn map(&self, analysis: &Analysis, threshold: f32) -> DiarizationResult {
        let vectors: Vec<Vec<f32>> = analysis
            .embeddings
            .iter()
            .map(|embedding| embedding.embedding.clone())
            .collect();
        let labels = clustering::cluster(&vectors, &self.config.clustering(threshold));
        let assignments: Vec<Option<usize>> = labels.iter().map(|label| Some(*label)).collect();
        let turns = timeline::turns(analysis, &assignments, &self.config.timeline);
        let chunks: Vec<ClusterChunk> = analysis
            .embeddings
            .iter()
            .zip(&labels)
            .map(|(embedding, label)| ClusterChunk {
                speaker_label: timeline::cluster_label(*label),
                start: embedding.start,
                end: embedding.end,
                embedding: embedding.embedding.clone(),
                quality: 1.0,
            })
            .collect();
        mapping::result(&turns, &chunks)
    }

    /// The refinement pass over a mapped result, this pipeline as the
    /// slice embedder.
    pub fn refine(
        &mut self,
        mapped: &DiarizationResult,
        audio: &AudioBuffer16k,
    ) -> Result<DiarizationResult, DiarizeError> {
        let rules = self.config.refinement.clone();
        let clusters = refinement::refine(&mapped.clusters, audio, &rules, self)
            .map_err(DiarizeError::Backend)?;
        Ok(DiarizationResult { clusters })
    }

    /// The embedding of `audio` heard as one voice: every window
    /// embedding of the slice, whoever the model split it into, in the
    /// duration-weighted unit mean the mapping uses for a cluster. What
    /// Swift gets from `FluidAudio` capped at one speaker.
    pub fn embed_slice(
        &mut self,
        audio: &AudioBuffer16k,
    ) -> Result<Option<Embedding>, DiarizeError> {
        if audio.duration() < DiarizerConfig::MINIMUM_AUDIO_SECONDS {
            return Ok(None);
        }
        let analysis = self.analyze(audio)?;
        let chunks: Vec<ClusterChunk> = analysis
            .embeddings
            .iter()
            .map(|embedding| ClusterChunk {
                speaker_label: timeline::cluster_label(0),
                start: embedding.start,
                end: embedding.end,
                embedding: embedding.embedding.clone(),
                quality: 1.0,
            })
            .collect();
        Ok(mapping::cluster_embedding(&chunks))
    }
}

impl<B: TensorBackend> SliceEmbedder for Pipeline<B> {
    fn embedding(&mut self, audio: &AudioBuffer16k) -> Result<Option<Embedding>, BoxError> {
        self.embed_slice(audio)
            .map_err(|error| Box::new(error) as BoxError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults are the plan's and `FluidAudio`'s numbers; nothing
    /// else pins them, so a slipped constant would otherwise pass every
    /// behavioural test that builds its own rules.
    #[test]
    fn the_default_configuration_is_the_calibrated_one() {
        let config = DiarizerConfig::default();
        assert_eq!(config.clustering_threshold, DEFAULT_CLUSTERING_THRESHOLD);
        assert_eq!(DEFAULT_CLUSTERING_THRESHOLD, 0.32);
        assert_eq!(config.min_speakers, None);
        assert_eq!(config.max_speakers, None);
        assert!(config.refines_clusters);
        assert_eq!(config.step_seconds, 2.0);
        assert_eq!(config.step_samples(16_000), 32_000);
        assert_eq!(config.extraction.min_segment_seconds, 1.0);
        assert!(config.extraction.exclude_overlap);
        assert_eq!(config.extraction.min_active_ratio, 0.2);
        assert_eq!(config.timeline.min_duration_on, 1.0);
        assert_eq!(config.timeline.min_duration_off, 0.1);
        assert_eq!(config.refinement.minimum_seconds, 30.0);
        assert_eq!(config.refinement.merge_threshold, 0.60);
        assert_eq!(config.refinement.absorb_threshold, 0.30);
        assert_eq!(DiarizerConfig::MINIMUM_AUDIO_SECONDS, 1.0);
        let clustering = config.clustering(0.5);
        assert_eq!(clustering.threshold, 0.5);
        assert_eq!(
            config.clustering(config.clustering_threshold).threshold,
            0.32
        );
    }
}
