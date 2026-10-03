//! `--db PATH`, the failure type with its exit code, and the dependencies
//! every command that processes needs. Swift: `Sources/steno/Wiring.swift`.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Args;
use steno_core::{SecretKey, Settings, StenoPaths, Store};
use steno_pipeline::{MeetingEventBus, PipelineDependencies};
use uuid::Uuid;

/// A usage error exits 1, a runtime failure 2.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Runtime(String),
}

impl Failure {
    pub fn usage(message: impl Into<String>) -> Self {
        Failure::Usage(message.into())
    }

    pub fn runtime(message: impl std::fmt::Display) -> Self {
        Failure::Runtime(message.to_string())
    }
}

pub type Outcome = Result<(), Failure>;

/// `--db PATH`, shared by every command that opens the database. Defaults
/// to the database under the support directory, which follows `HOME`.
#[derive(Debug, Clone, Args)]
pub struct DatabaseOptions {
    /// Path of the SQLite database.
    #[arg(long = "db", value_name = "PATH")]
    pub database_path: Option<PathBuf>,
}

impl DatabaseOptions {
    pub fn path(&self) -> Result<PathBuf, Failure> {
        match &self.database_path {
            Some(path) => Ok(path.clone()),
            None => Ok(paths()?.database_path()),
        }
    }

    pub fn open(&self) -> Result<Arc<Store>, Failure> {
        steno_services::open_store(&self.path()?).map_err(Failure::runtime)
    }
}

/// The engine ids the Swift CLI knew; `--engine` lists them in its error
/// and its help. Only `parakeet-v3` has a Rust engine today.
pub const ENGINE_IDS: [&str; 4] = [
    "parakeet-v3",
    "parakeet-ultra",
    "parakeet-de",
    "whisperkit-large-v3-turbo",
];

/// `--engine <id>`: without it the pipeline runs the fakes; with it the
/// named engine, the ONNX diarizer and cosine speaker memory over the
/// store, models downloading on first use.
#[derive(Debug, Clone, Args)]
pub struct SpeechOptions {
    /// Speech engine id (parakeet-v3, parakeet-ultra, parakeet-de, whisperkit-large-v3-turbo); every id runs the ONNX Parakeet v3 engine off the Mac.
    #[arg(long, value_name = "engine")]
    pub engine: Option<String>,
}

impl SpeechOptions {
    pub fn validate(&self) -> Result<(), Failure> {
        match &self.engine {
            Some(engine) if !ENGINE_IDS.contains(&engine.as_str()) => Err(Failure::usage(format!(
                "error: invalid value '{engine}' for '--engine <engine>': expected one of {}",
                ENGINE_IDS.join(", ")
            ))),
            _ => Ok(()),
        }
    }
}

/// The parsed `<meeting-id>` argument.
pub fn parse_uuid(argument: &str) -> Result<Uuid, String> {
    Uuid::parse_str(argument).map_err(|_| format!("{argument} is not a UUID."))
}

/// The support paths the CLI uses for models and secrets.
pub fn paths() -> Result<StenoPaths, Failure> {
    StenoPaths::create_default().map_err(Failure::runtime)
}

/// The LLM API key from the CLI's secret store: `STENO_LLM_API_KEY` or
/// the 0600 secrets file in the support directory.
pub async fn api_key() -> Result<Option<String>, Failure> {
    steno_services::secret_store(false, &paths()?)
        .secret(&SecretKey::llm_api_key())
        .await
        .map_err(Failure::runtime)
}

/// The LLM passes from the settings, `None` without an endpoint.
pub async fn llm_passes(
    settings: &Settings,
) -> Result<Option<steno_services::llm::Passes>, Failure> {
    Ok(steno_services::llm::passes(
        settings,
        api_key().await?.as_deref(),
        &steno_services::llm::codex_store(),
        steno_adapters::runtime::local_time_zone(),
    ))
}

/// The pipeline dependencies: core's fakes for speech and diarization
/// unless `--engine` names a real one; the LLM passes from the settings;
/// delivery through the real coordinator unless a command passes its own.
pub fn dependencies(
    store: Arc<Store>,
    settings: &Settings,
    engine: Option<&str>,
    models_directory: Option<&std::path::Path>,
    dispatcher: Option<Arc<dyn steno_core::DeliveryDispatcher>>,
    llm: Option<steno_services::llm::Passes>,
    events: MeetingEventBus,
) -> Result<PipelineDependencies, Failure> {
    let models_directory = match models_directory {
        Some(directory) => steno_services::speech::absolute(directory),
        None => steno_services::speech::models_directory(settings, &paths()?),
    };
    let (speech_engine, diarizer, memory): (
        Arc<dyn steno_core::SpeechEngine>,
        Arc<dyn steno_core::Diarizer>,
        Arc<dyn steno_core::SpeakerMemory>,
    ) = match engine {
        Some(_) => (
            steno_services::speech::speech_engine(settings, &models_directory),
            steno_services::speech::diarizer(&models_directory),
            Arc::new(steno_pipeline::StoreSpeakerMemory::new(store.clone())),
        ),
        None => (
            Arc::new(steno_core::testing::FakeSpeechEngine::default()),
            Arc::new(steno_core::testing::FakeDiarizer::default()),
            Arc::new(steno_core::testing::InMemorySpeakerMemory::new(
                store.persons().map_err(Failure::runtime)?,
            )),
        ),
    };
    let dispatcher = dispatcher
        .unwrap_or_else(|| Arc::new(steno_adapters::DeliveryCoordinator::new(store.clone())));
    let dependencies = PipelineDependencies::new(
        Arc::new(steno_audio::SymphoniaAudioCodec::new()),
        speech_engine,
        diarizer,
        memory,
        dispatcher,
        store,
        events,
    );
    Ok(match llm {
        Some(passes) => dependencies.with_llm(Some(passes.cleaner), Some(passes.summarizer)),
        None => dependencies,
    })
}

/// A hex SHA-256, `sha256sum`'s spelling.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest as _;
    use std::fmt::Write as _;
    let digest = sha2::Sha256::digest(data);
    digest
        .iter()
        .fold(String::with_capacity(64), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}
