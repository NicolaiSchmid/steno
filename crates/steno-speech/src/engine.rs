//! The `SpeechEngine` over the ONNX backend: `prepare` installs and loads
//! the models once, `transcribe` runs the pipeline on a blocking thread.
//! The id is `parakeet-v3`, the Swift engine's: it is the same model and
//! the same setting; which backend runs it is the platform's business.
//! Swift: `Sources/StenoSpeech/Engines/ParakeetEngine.swift`.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use steno_core::protocols::{BoundaryResult, async_trait};
use steno_core::{AudioBuffer16k, LanguageTag, RawSegment, SpeechEngine};

use crate::chunker::{ChunkerConfig, Gaps};
use crate::error::SpeechError;
use crate::language::LanguageTagger;
use crate::model_store::{DownloadProgress, ModelAsset, ModelStore};
use crate::onnx::{OnnxBackend, OnnxOptions};
use crate::pipeline::{PipelineConfig, Transcriber};
use crate::vad::{SileroVad, VadConfig};

/// The engine id of Parakeet TDT v3, whichever backend runs it: the
/// Swift engine's, stored in `Settings.speechEngineID`
/// (`SpeechEngineID.parakeetV3`).
pub const PARAKEET_V3_ID: &str = "parakeet-v3";

/// The 25 European languages of Parakeet TDT v3 (NVIDIA model card;
/// `SpeechEngineID.parakeetV3Languages`).
pub const PARAKEET_V3_LANGUAGES: [&str; 25] = [
    "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt", "mt",
    "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk",
];

/// The shared pipeline's defaults with two chunker settings of the
/// engine's own, measured on FLEURS German `cat/` and the calibration
/// corpus (A16b in `.plans/2026-10-07-stable-promotion.md`):
///
/// - the first chunk starts at the recording's start
///   ([`ChunkerConfig::from_start`]), so the lead-in is decoded with the
///   speech after it: FLEURS 4.865 % mean WER against 4.882 % without, the
///   corpus's mean WER against the Swift transcripts 9.735 % instead of
///   9.814 %, and no word added where the Swift transcripts have none;
/// - the tail is decoded in extra windows ([`Gaps::Tail`]) once it is at
///   least [`MIN_GAP_SECONDS`](crate::chunker::MIN_GAP_SECONDS) long, so
///   quiet speech after the last speech the detector hears comes through;
///   FLEURS and the corpus are unchanged by it.
///
/// The long pauses stay skipped (`long_pause_seconds`, 3 s): extra windows
/// over every gap ([`Gaps::All`], the `CoreML` engine's setting) inserted
/// words there, 19 more on FLEURS (5.198 %) and some where the Swift
/// transcripts have none, scored worse on the corpus (9.916 %) and cost
/// +27 % on FLEURS and +41 % on a 15-minute meeting's two lanes.
#[must_use]
pub fn pipeline_config() -> PipelineConfig {
    PipelineConfig {
        chunker: ChunkerConfig {
            decode_gaps: Gaps::Tail,
            from_start: true,
            ..ChunkerConfig::default()
        },
        ..PipelineConfig::default()
    }
}

/// Parakeet v3 on ONNX Runtime.
pub struct OnnxSpeechEngine {
    store: ModelStore,
    options: OnnxOptions,
    config: PipelineConfig,
    vad: VadConfig,
    languages: BTreeSet<LanguageTag>,
    loaded: Arc<Mutex<Option<Transcriber<OnnxBackend>>>>,
}

impl OnnxSpeechEngine {
    /// [`PARAKEET_V3_ID`].
    pub const ID: &'static str = PARAKEET_V3_ID;

    /// [`PARAKEET_V3_LANGUAGES`].
    pub const LANGUAGES: [&'static str; 25] = PARAKEET_V3_LANGUAGES;

    /// An engine over `store` with the engine's settings
    /// ([`pipeline_config`]); nothing is loaded until `prepare`.
    #[must_use]
    pub fn new(store: ModelStore, options: OnnxOptions) -> Self {
        Self::with_config(store, options, pipeline_config(), VadConfig::default())
    }

    #[must_use]
    pub fn with_config(
        store: ModelStore,
        options: OnnxOptions,
        config: PipelineConfig,
        vad: VadConfig,
    ) -> Self {
        OnnxSpeechEngine {
            store,
            options,
            config,
            vad,
            languages: Self::LANGUAGES.into_iter().map(LanguageTag::from).collect(),
            loaded: Arc::new(Mutex::new(None)),
        }
    }

    #[must_use]
    pub fn store(&self) -> &ModelStore {
        &self.store
    }

    /// Installs what is missing from `store`, loads the export and Silero
    /// and returns the transcriber the engine runs; the model-gated test
    /// and the `transcribe` example open theirs the same way. Blocking;
    /// download progress goes to `tracing` at debug level.
    ///
    /// ```no_run
    /// use steno_speech::engine::pipeline_config;
    /// use steno_speech::{ModelStore, OnnxOptions, OnnxSpeechEngine, VadConfig};
    ///
    /// let mut transcriber = OnnxSpeechEngine::open_transcriber(
    ///     &ModelStore::from_environment(),
    ///     &OnnxOptions::default(),
    ///     pipeline_config(),
    ///     VadConfig::default(),
    /// )?;
    /// let samples = vec![0.0f32; 16_000]; // one second of 16 kHz mono
    /// let transcript = transcriber.transcribe(&samples, None)?;
    /// println!("{}", transcript.text());
    /// # Ok::<(), steno_speech::SpeechError>(())
    /// ```
    pub fn open_transcriber(
        store: &ModelStore,
        options: &OnnxOptions,
        config: PipelineConfig,
        vad: VadConfig,
    ) -> Result<Transcriber<OnnxBackend>, SpeechError> {
        store.ensure(&ModelAsset::silero_vad(), &mut log_download)?;
        store.ensure(&ModelAsset::parakeet_v3_fp32(), &mut log_download)?;
        Self::load_installed(store, options, config, vad)
    }

    /// [`OnnxSpeechEngine::open_transcriber`] without the downloads: the
    /// export and Silero must already be in `store`, else
    /// [`SpeechError::NotInstalled`]. What `steno-speech-sidecar` runs, so
    /// the child never opens a connection.
    pub fn load_installed(
        store: &ModelStore,
        options: &OnnxOptions,
        config: PipelineConfig,
        vad: VadConfig,
    ) -> Result<Transcriber<OnnxBackend>, SpeechError> {
        let vad_directory = store.installed_directory(&ModelAsset::silero_vad())?;
        let model_directory = store.installed_directory(&ModelAsset::parakeet_v3_fp32())?;
        let (backend, vocab) = OnnxBackend::load(&model_directory, options)?;
        let detector = SileroVad::load(&vad_directory.join("silero_vad.onnx"), options, vad)?;
        Ok(Transcriber::new(
            backend,
            vocab,
            Box::new(detector),
            LanguageTagger::new(),
            config,
        ))
    }
}

/// Download progress of the model store, to `tracing` at debug level.
pub(crate) fn log_download(progress: DownloadProgress<'_>) {
    tracing::debug!(
        file = progress.file,
        received = progress.received,
        total = progress.total,
        "model download"
    );
}

/// Runs `work` off the async executor when one is present, inline
/// otherwise.
pub(crate) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, SpeechError> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle
            .spawn_blocking(work)
            .await
            .map_err(|e| SpeechError::Worker(e.to_string())),
        Err(_) => Ok(work()),
    }
}

#[async_trait]
impl SpeechEngine for OnnxSpeechEngine {
    fn id(&self) -> &str {
        Self::ID
    }

    fn supported_languages(&self) -> &BTreeSet<LanguageTag> {
        &self.languages
    }

    async fn prepare(&self) -> BoundaryResult<()> {
        let loaded = Arc::clone(&self.loaded);
        let (store, options, config, vad) = (
            self.store.clone(),
            self.options.clone(),
            self.config.clone(),
            self.vad.clone(),
        );
        blocking(move || {
            let mut guard = loaded.lock().unwrap_or_else(PoisonError::into_inner);
            if guard.is_none() {
                *guard = Some(Self::open_transcriber(&store, &options, config, vad)?);
            }
            Ok::<_, SpeechError>(())
        })
        .await??;
        Ok(())
    }

    async fn transcribe(
        &self,
        audio: &AudioBuffer16k,
        hint: Option<&LanguageTag>,
    ) -> BoundaryResult<Vec<RawSegment>> {
        if audio.is_empty() {
            return Ok(Vec::new());
        }
        self.prepare().await?;
        let loaded = Arc::clone(&self.loaded);
        // One copy of the recording per call, for the blocking thread; the
        // speech sidecar copies again into its payload
        // (`sidecar::protocol::encode_samples`).
        let samples = audio.samples.clone();
        let hint = hint.cloned();
        let transcript = blocking(move || {
            let mut guard = loaded.lock().unwrap_or_else(PoisonError::into_inner);
            let transcriber = guard.as_mut().ok_or(SpeechError::NotPrepared)?;
            transcriber.transcribe(&samples, hint.as_ref())
        })
        .await??;
        Ok(transcript.segments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_engine_names_the_model_and_its_languages() {
        let engine = OnnxSpeechEngine::new(ModelStore::new("/nowhere"), OnnxOptions::default());
        assert_eq!(engine.id(), "parakeet-v3");
        assert_eq!(engine.supported_languages().len(), 25);
        assert!(
            engine
                .supported_languages()
                .contains(&LanguageTag::from("de"))
        );
        assert_eq!(engine.store().root(), std::path::Path::new("/nowhere"));
    }

    #[test]
    fn the_engine_decodes_the_lead_in_and_the_tail_and_skips_long_pauses() {
        let config = pipeline_config();
        assert!(config.chunker.from_start);
        assert_eq!(config.chunker.decode_gaps, Gaps::Tail);
        assert_eq!(config.chunker.long_pause_seconds, 3.0);
        let rest = PipelineConfig {
            chunker: ChunkerConfig {
                decode_gaps: Gaps::Tail,
                from_start: true,
                ..ChunkerConfig::default()
            },
            ..PipelineConfig::default()
        };
        assert_eq!(config, rest);
        let engine = OnnxSpeechEngine::new(ModelStore::new("/nowhere"), OnnxOptions::default());
        assert_eq!(engine.config, config);
    }

    #[tokio::test]
    async fn empty_audio_needs_no_models() {
        let dir = tempfile::tempdir().unwrap();
        let engine = OnnxSpeechEngine::new(ModelStore::new(dir.path()), OnnxOptions::default());
        assert!(
            engine
                .transcribe(&AudioBuffer16k::default(), None)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
