//! The pipeline above the tensors: analysis, clustering, timeline, mapping
//! and refinement over one [`DiarizationBackend`].

use steno_core::{AudioBuffer16k, BoxError, ClusterChunk, DiarizationResult, Embedding};

use crate::backend::DiarizationBackend;
use crate::clustering::{self, ClusteringConfig};
use crate::error::DiarizeError;
use crate::extraction::{self, Analysis, ExtractionRules};
use crate::refinement::{self, Rules, SliceEmbedder};
use crate::to_f64;
use crate::{mapping, timeline};

/// The clustering knobs, the window step, the stage rules and whether
/// refinement runs.
#[derive(Debug, Clone, PartialEq)]
pub struct DiarizerConfig {
    /// Mean pairwise cosine distance (average linkage) at or below which two
    /// clusters merge; see [`DEFAULT_CLUSTERING_THRESHOLD`].
    pub clustering_threshold: f32,
    pub min_speakers: Option<usize>,
    pub max_speakers: Option<usize>,
    /// Whether the refinement pass runs after the mapping.
    pub refines_clusters: bool,
    /// Seconds between two segmentation windows. Votes stay exact for any step
    /// of three samples or more (see `timeline`).
    pub step_seconds: f64,
    pub extraction: ExtractionRules,
    pub timeline: timeline::TimelineRules,
    pub refinement: Rules,
}

/// The clustering cut: `FluidAudio`'s Euclidean 0.8 on unit vectors as a
/// cosine distance (`d^2 = 2 - 2 cos`). The calibration calls passed at every
/// cut from 0.20 to 0.60 on both backends, so the derived value is kept.
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
    /// Audio shorter than this yields no speakers without touching the models.
    pub const MINIMUM_AUDIO_SECONDS: f64 = 1.0;

    fn clustering(&self, threshold: f32) -> ClusteringConfig {
        ClusteringConfig {
            threshold,
            min_speakers: self.min_speakers,
            max_speakers: self.max_speakers,
        }
    }

    fn step_samples(&self, sample_rate: usize) -> usize {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let step = (self.step_seconds * to_f64(sample_rate)).round().max(1.0) as usize;
        step
    }
}

/// One backend and one configuration; `diarize` runs the whole chain, the
/// pieces are public for the calibration harness.
pub struct Pipeline<B: DiarizationBackend = Box<dyn DiarizationBackend>> {
    backend: B,
    config: DiarizerConfig,
}

impl<B: DiarizationBackend> Pipeline<B> {
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

    /// Replaces the configuration; the backend stays loaded.
    pub fn set_config(&mut self, config: DiarizerConfig) {
        self.config = config;
    }

    /// The backend, for what it reports about itself.
    #[must_use]
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// No usable embedding (silence, noise) is a result with no speakers, not
    /// a failure.
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

    /// Clustering at `threshold`, the timeline and the mapping, before
    /// refinement.
    #[must_use]
    pub fn map(&self, analysis: &Analysis, threshold: f32) -> DiarizationResult {
        let vectors: Vec<Vec<f32>> = analysis
            .embeddings
            .iter()
            .map(|embedding| embedding.embedding.clone())
            .collect();
        let labels = clustering::cluster(&vectors, &self.config.clustering(threshold));
        let assignments: Vec<Option<usize>> = labels.iter().copied().map(Some).collect();
        let turns = timeline::turns(analysis, &assignments, &self.config.timeline);
        mapping::result(&turns, &chunks(analysis, labels))
    }

    /// The refinement pass over a mapped result, this pipeline as embedder.
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

    /// The embedding of `audio` heard as one voice: the duration-weighted unit
    /// mean of every window embedding of the slice.
    pub fn embed_slice(
        &mut self,
        audio: &AudioBuffer16k,
    ) -> Result<Option<Embedding>, DiarizeError> {
        if audio.duration() < DiarizerConfig::MINIMUM_AUDIO_SECONDS {
            return Ok(None);
        }
        let analysis = self.analyze(audio)?;
        let one_voice = std::iter::repeat(0);
        Ok(mapping::cluster_embedding(&chunks(&analysis, one_voice)))
    }
}

/// The window embeddings as the mapping's chunks, the `i`-th in the
/// cluster of the `i`-th label; `mapping::result` sets their quality.
fn chunks(analysis: &Analysis, labels: impl IntoIterator<Item = usize>) -> Vec<ClusterChunk> {
    analysis
        .embeddings
        .iter()
        .zip(labels)
        .map(|(embedding, label)| ClusterChunk {
            speaker_label: timeline::cluster_label(label),
            start: embedding.start,
            end: embedding.end,
            embedding: embedding.embedding.clone(),
            quality: 1.0,
        })
        .collect()
}

impl<B: DiarizationBackend> SliceEmbedder for Pipeline<B> {
    fn embedding(&mut self, audio: &AudioBuffer16k) -> Result<Option<Embedding>, BoxError> {
        self.embed_slice(audio)
            .map_err(|error| Box::new(error) as BoxError)
    }
}
