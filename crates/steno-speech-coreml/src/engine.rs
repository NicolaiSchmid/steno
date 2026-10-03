//! `SpeechEngine` over the CoreML pipeline: the Rust twin of
//! `StenoSpeech.ParakeetEngine` for `parakeet-v3`. Models load on the
//! first `prepare` (or the first `transcribe`) from the Swift app's model
//! directory and stay loaded for the engine's lifetime.
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

use crate::SpeechError;
use crate::backend::Backend;
use crate::pipeline::{Config, Transcriber};
use crate::segments::raw_segments;

/// The engine id Steno stores in `Settings.speechEngineID`
/// (`SpeechEngineID.parakeetV3`).
pub const ENGINE_ID: &str = "parakeet-v3";

/// The 25 European languages of Parakeet TDT v3 (NVIDIA model card;
/// `SpeechEngineID.parakeetV3Languages`).
pub const LANGUAGES: [&str; 25] = [
    "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt", "mt",
    "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk",
];

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
    config: Config,
    languages: BTreeSet<LanguageTag>,
    transcriber: Mutex<Option<Arc<Transcriber>>>,
}

impl std::fmt::Debug for CoreMlParakeetEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreMlParakeetEngine")
            .field("model_directory", &self.model_directory)
            .field("config", &self.config)
            .field("loaded", &self.is_loaded())
            .finish_non_exhaustive()
    }
}

impl CoreMlParakeetEngine {
    /// An engine over the models in `model_directory`; nothing loads yet.
    #[must_use]
    pub fn new(model_directory: impl Into<PathBuf>) -> CoreMlParakeetEngine {
        CoreMlParakeetEngine::with_config(model_directory, Config::default())
    }

    #[must_use]
    pub fn with_config(
        model_directory: impl Into<PathBuf>,
        config: Config,
    ) -> CoreMlParakeetEngine {
        CoreMlParakeetEngine {
            model_directory: model_directory.into(),
            config,
            languages: LANGUAGES
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

    /// The transcriber, loading the models on first use. `loadLocal` in
    /// Swift never touches the network and neither does this: a missing
    /// model directory is an error, not a download (the model store owns
    /// downloads).
    fn loaded(&self) -> Result<Arc<Transcriber>, SpeechError> {
        let mut slot = self
            .transcriber
            .lock()
            .map_err(|_| SpeechError::CoreMl("engine lock poisoned".to_owned()))?;
        if let Some(transcriber) = slot.as_ref() {
            return Ok(Arc::clone(transcriber));
        }
        let backend = Backend::load(&self.model_directory)?;
        let transcriber = Arc::new(Transcriber::with_config(backend, self.config));
        *slot = Some(Arc::clone(&transcriber));
        Ok(transcriber)
    }

    /// `transcribe` without the trait: the segments for `audio`.
    pub fn transcribe_buffer(
        &self,
        audio: &AudioBuffer16k,
    ) -> Result<Vec<RawSegment>, SpeechError> {
        if audio.is_empty() {
            return Ok(Vec::new());
        }
        let transcriber = self.loaded()?;
        let transcript = transcriber.transcribe(&audio.samples)?;
        Ok(raw_segments(
            &transcript.tokens,
            transcriber.vocab(),
            audio.duration(),
        ))
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
        self.loaded()?;
        Ok(())
    }

    /// `hint` is never sent to the model: Parakeet runs unpinned, as the
    /// Swift engine does (its `Language` parameter only separates Latin
    /// from Cyrillic script), so Denglish comes out mixed.
    async fn transcribe(
        &self,
        audio: &AudioBuffer16k,
        _hint: Option<&LanguageTag>,
    ) -> BoundaryResult<Vec<RawSegment>> {
        Ok(self.transcribe_buffer(audio)?)
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
