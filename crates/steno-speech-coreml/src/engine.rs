//! `SpeechEngine` over the CoreML models and the shared pipeline:
//! `steno_speech`'s [`Transcriber`] (the VAD layout, the decode loop, the
//! merge, the segmentation and the language tagger) over four [`Backend`]
//! workers. Models load on the first `prepare` (or the first `transcribe`)
//! from the Swift app's model directory and stay loaded for the engine's
//! lifetime.
//! Swift: `Sources/StenoSpeech/Engines/ParakeetEngine.swift`.
//!
//! `transcribe` runs the pipeline on the calling thread: it takes seconds
//! and the boundary is documented as such (`steno_core::protocols`); the
//! pipeline host runs it on a blocking thread when it has an async
//! runtime to protect.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use steno_core::{
    AudioBuffer16k, BoundaryResult, LanguageTag, RawSegment, SpeechEngine, StenoPaths, async_trait,
};
use steno_speech::{
    AdaptiveEnergyVad, ChunkerConfig, LanguageTagger, PARAKEET_V3_ID, PARAKEET_V3_LANGUAGES,
    PipelineConfig, Transcriber, Transcript, VadConfig,
};

use crate::SpeechError;
use crate::backend::{Backend, MAX_WINDOW_SAMPLES, Models};

/// The engine id, the ONNX engine's: the same model and setting.
pub const ENGINE_ID: &str = PARAKEET_V3_ID;

/// Chunks decoded at once (`ASRConfig.parallelChunkConcurrency`).
pub const WORKERS: usize = 4;

/// The shared pipeline's defaults with two chunker settings of the
/// engine's own, measured on FLEURS German `cat/` (A2 in
/// `.plans/2026-10-07-stable-promotion.md`):
///
/// - the clamp is the models' 15 s window;
/// - no pause is long enough to be skipped: with the energy detector a
///   chunk that began just before speech after a long pause lost words
///   (5.86 % mean WER against 5.09 % without the skip), and a quiet
///   stretch the detector takes for silence is still decoded.
#[must_use]
pub fn pipeline_config() -> PipelineConfig {
    // 240,000 samples is exactly 15 s, so the clamp is the window.
    #[allow(clippy::cast_precision_loss)]
    let max_seconds = MAX_WINDOW_SAMPLES as f32 / steno_speech::SAMPLE_RATE as f32;
    PipelineConfig {
        chunker: ChunkerConfig {
            max_seconds,
            long_pause_seconds: f32::INFINITY,
            ..ChunkerConfig::default()
        },
        ..PipelineConfig::default()
    }
}

/// `Models/fluidaudio/parakeet-tdt-0.6b-v3` under the support directory:
/// where `StenoSpeech.ModelStore` installs the asset the Swift app uses.
#[must_use]
pub fn default_model_directory() -> PathBuf {
    model_directory(&StenoPaths::default_support_directory())
}

/// The model directory under `support_directory`.
#[must_use]
pub fn model_directory(support_directory: &Path) -> PathBuf {
    support_directory
        .join("Models")
        .join("fluidaudio")
        .join("parakeet-tdt-0.6b-v3")
}

/// Parakeet TDT v3 on CoreML behind `SpeechEngine`.
pub struct CoreMlParakeetEngine {
    model_directory: PathBuf,
    config: PipelineConfig,
    workers: usize,
    languages: BTreeSet<LanguageTag>,
    transcriber: Mutex<Option<Transcriber<Backend>>>,
}

impl std::fmt::Debug for CoreMlParakeetEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreMlParakeetEngine")
            .field("model_directory", &self.model_directory)
            .field("workers", &self.workers)
            .field("loaded", &self.is_loaded())
            .finish_non_exhaustive()
    }
}

impl CoreMlParakeetEngine {
    /// An engine over the models in `model_directory`; nothing loads yet.
    #[must_use]
    pub fn new(model_directory: impl Into<PathBuf>) -> CoreMlParakeetEngine {
        CoreMlParakeetEngine::with_config(model_directory, pipeline_config(), WORKERS)
    }

    /// With `config` and `workers` chunks decoded at once (at least one).
    #[must_use]
    pub fn with_config(
        model_directory: impl Into<PathBuf>,
        config: PipelineConfig,
        workers: usize,
    ) -> CoreMlParakeetEngine {
        CoreMlParakeetEngine {
            model_directory: model_directory.into(),
            config,
            workers: workers.max(1),
            languages: PARAKEET_V3_LANGUAGES
                .iter()
                .map(|tag| LanguageTag::from(*tag))
                .collect(),
            transcriber: Mutex::new(None),
        }
    }

    /// The engine over [`default_model_directory`].
    #[must_use]
    pub fn in_default_directory() -> CoreMlParakeetEngine {
        CoreMlParakeetEngine::new(default_model_directory())
    }

    #[must_use]
    pub fn model_directory(&self) -> &Path {
        &self.model_directory
    }

    /// Whether `prepare` has run.
    #[must_use]
    pub fn is_loaded(&self) -> bool {
        self.transcriber.lock().is_ok_and(|t| t.is_some())
    }

    /// Runs `work` on the transcriber, loading the models on first use.
    /// `loadLocal` in Swift never touches the network and neither does
    /// this: a missing model directory is an error, not a download (the
    /// model store owns downloads).
    fn with_transcriber<T>(
        &self,
        work: impl FnOnce(&mut Transcriber<Backend>) -> Result<T, SpeechError>,
    ) -> Result<T, SpeechError> {
        let mut slot = self
            .transcriber
            .lock()
            .map_err(|_| SpeechError::CoreMl("engine lock poisoned".to_owned()))?;
        if slot.is_none() {
            let models = Arc::new(Models::load(&self.model_directory)?);
            let vocab = models.vocab().clone();
            let backend = Backend::new(models);
            let more = vec![backend.clone(); self.workers - 1];
            *slot = Some(
                Transcriber::new(
                    backend,
                    vocab,
                    Box::new(AdaptiveEnergyVad {
                        config: VadConfig::default(),
                    }),
                    LanguageTagger::new(),
                    self.config.clone(),
                )
                .with_workers(more),
            );
        }
        let transcriber = slot.as_mut().expect("loaded above");
        work(transcriber)
    }

    /// The models' load time; loads them when they are not yet.
    pub fn load_seconds(&self) -> Result<f64, SpeechError> {
        self.with_transcriber(|t| Ok(t.backend().models().load_seconds()))
    }

    /// The whole transcript of 16 kHz mono `samples`, for the harness;
    /// `hint` steers the language tagger only.
    pub fn transcribe_samples(
        &self,
        samples: &[f32],
        hint: Option<&LanguageTag>,
    ) -> Result<Transcript, SpeechError> {
        self.with_transcriber(|t| Ok(t.transcribe(samples, hint)?))
    }

    /// `transcribe` without the trait: the segments for `audio`.
    pub fn transcribe_buffer(
        &self,
        audio: &AudioBuffer16k,
        hint: Option<&LanguageTag>,
    ) -> Result<Vec<RawSegment>, SpeechError> {
        if audio.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self.transcribe_samples(&audio.samples, hint)?.segments)
    }
}

#[async_trait]
impl SpeechEngine for CoreMlParakeetEngine {
    fn id(&self) -> &str {
        ENGINE_ID
    }

    fn supported_languages(&self) -> &BTreeSet<LanguageTag> {
        &self.languages
    }

    async fn prepare(&self) -> BoundaryResult<()> {
        self.with_transcriber(|_| Ok(()))?;
        Ok(())
    }

    /// `hint` is never sent to the model: Parakeet runs unpinned, as the
    /// Swift engine does (its `Language` parameter only separates Latin
    /// from Cyrillic script), so Denglish comes out mixed. It steers the
    /// language tagger.
    async fn transcribe(
        &self,
        audio: &AudioBuffer16k,
        hint: Option<&LanguageTag>,
    ) -> BoundaryResult<Vec<RawSegment>> {
        Ok(self.transcribe_buffer(audio, hint)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_reports_its_id_and_languages_without_models() {
        let engine = CoreMlParakeetEngine::new("/nonexistent/models");
        assert_eq!(engine.id(), ENGINE_ID);
        assert_eq!(engine.supported_languages().len(), 25);
        assert!(
            engine
                .supported_languages()
                .contains(&LanguageTag::from("de"))
        );
        assert!(!engine.is_loaded());
        assert_eq!(
            model_directory(Path::new("/tmp/Steno")),
            PathBuf::from("/tmp/Steno/Models/fluidaudio/parakeet-tdt-0.6b-v3")
        );
        assert_eq!(
            steno_speech::sample_count(pipeline_config().chunker.max_seconds),
            MAX_WINDOW_SAMPLES
        );
    }

    #[tokio::test]
    async fn empty_audio_needs_no_models_and_a_missing_directory_fails_prepare() {
        let engine = CoreMlParakeetEngine::new("/nonexistent/models");
        let segments = engine
            .transcribe(&AudioBuffer16k::default(), None)
            .await
            .unwrap();
        assert_eq!(segments, Vec::new());
        let error = engine.prepare().await.unwrap_err().to_string();
        assert!(error.contains("nonexistent"), "{error}");
        assert!(!engine.is_loaded());
    }
}
