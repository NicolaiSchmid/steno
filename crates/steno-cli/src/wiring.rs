//! `--db PATH`, the failure type with its exit code, and the dependencies
//! every command that processes needs. Swift: `Sources/steno/Wiring.swift`.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use clap::Args;
use steno_core::{
    DatabaseLock, DatabaseLockError, SecretKey, Settings, StenoPaths, Store, StoreError,
};
use steno_pipeline::{MeetingEventBus, PipelineDependencies};
use steno_services::BuildError;
use steno_services::speech::SpeechSetup;
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
/// to the database under the support directory, which follows `HOME`. A
/// path with the extension `lock` is refused: it would be its own lock
/// file (`DatabaseLock::path_for`). Rust only.
#[derive(Debug, Clone, Args)]
pub struct DatabaseOptions {
    /// Path of the SQLite database.
    #[arg(long = "db", value_name = "PATH")]
    pub database_path: Option<PathBuf>,
}

impl DatabaseOptions {
    pub fn path(&self) -> Result<PathBuf, Failure> {
        match &self.database_path {
            Some(path)
                if path
                    .extension()
                    .is_some_and(|extension| extension == "lock") =>
            {
                Err(Failure::usage(format!(
                    "{} would be its own lock file; give the database another extension.",
                    path.display()
                )))
            }
            Some(path) => Ok(path.clone()),
            None => Ok(paths()?.database_path()),
        }
    }

    /// Opens the database for a command that writes, holding its lock
    /// (`steno_core::DatabaseLock`) until the process ends; refused while
    /// the app or another steno command holds it, so the two never process
    /// one meeting twice or fail each other's recording. Rust only: the
    /// Swift CLI took no lock.
    pub fn open(&self) -> Result<Arc<Store>, Failure> {
        let path = self.path()?;
        match take_lock(&path, true)? {
            Lock::Taken(_) => steno_services::open_store(&path).map_err(Failure::runtime),
            Lock::Held => Err(Failure::runtime(format!(
                "Steno, or another steno command, is using {}; quit it first, then run this command again.",
                path.display()
            ))),
        }
    }

    /// Opens the database for a command that only reads. With no app on
    /// it, as [`DatabaseOptions::open`] does, migrating included, but it
    /// lets go of the lock once the database is open (unless this process
    /// holds it for a write), so a long read never keeps the app out.
    /// Beside the app, which holds it, without migrating
    /// (`Store::open_without_migrating`), so a newer CLI never changes the
    /// schema under an older app: SQLite's WAL lets it read while the app
    /// writes. Refused while the database lacks a migration this build
    /// would apply. Rust only.
    pub fn open_to_read(&self) -> Result<Arc<Store>, Failure> {
        let path = self.path()?;
        match take_lock(&path, false)? {
            // The lock, if this call took it, guards the migration and is
            // let go of at the end of this arm.
            Lock::Taken(_lock) => steno_services::open_store(&path).map_err(Failure::runtime),
            Lock::Held => match Store::open_without_migrating(&path) {
                Ok(store) => Ok(Arc::new(store)),
                Err(StoreError::PendingMigration(_)) => Err(Failure::runtime(
                    "Steno is updating its database, or an older Steno is running; quit it first, then run this command again.",
                )),
                Err(error) => Err(Failure::runtime(error)),
            },
        }
    }
}

/// Whether [`take_lock`] took the database's lock.
#[derive(Debug)]
enum Lock {
    /// This process holds it, or runs on a filesystem without locks. The
    /// lock this call took for its caller to let go of, if it took one.
    Taken(Option<DatabaseLock>),
    /// Another process (the app, another steno command) holds it.
    Held,
}

/// The database locks this process holds until it exits, by lock file;
/// `None` for one on a filesystem without locks.
static HELD_LOCKS: std::sync::Mutex<Vec<(PathBuf, Option<DatabaseLock>)>> =
    std::sync::Mutex::new(Vec::new());

/// Takes the lock of the database at `database`, at once or not at all:
/// for the rest of the process when `keep`, else for the caller to let go
/// of. Nothing to take when this process holds it already. On a filesystem
/// without locks the command runs without it and says so once. Through
/// `steno_services::lock_database`, which creates the database's folder
/// first as the app does, so a first command on a fresh `--db` path is
/// not an error.
fn take_lock(database: &Path, keep: bool) -> Result<Lock, Failure> {
    let mut held = HELD_LOCKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = DatabaseLock::path_for(database);
    if held.iter().any(|(taken, _)| *taken == path) {
        return Ok(Lock::Taken(None));
    }
    match steno_services::lock_database(database, std::time::Duration::ZERO) {
        Ok(lock) if !keep => Ok(Lock::Taken(Some(lock))),
        Ok(lock) => {
            held.push((path, Some(lock)));
            Ok(Lock::Taken(None))
        }
        Err(BuildError::Lock(DatabaseLockError::Held(_))) => Ok(Lock::Held),
        Err(BuildError::Lock(error @ DatabaseLockError::Unsupported { .. })) => {
            eprintln!("Running without the database lock: {error}");
            held.push((path, None));
            Ok(Lock::Taken(None))
        }
        Err(error) => Err(Failure::runtime(error)),
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

/// `--engine <id>`: without it the pipeline runs the fakes; with it
/// Parakeet v3 where the flag's help says, the ONNX diarizer and cosine
/// speaker memory over the store. The diarizer's models, and in the speech
/// sidecar Parakeet's, download on first use (from their hosts or the
/// mirror); the `CoreML` Parakeet must be installed.
#[derive(Debug, Clone, Args)]
pub struct SpeechOptions {
    #[arg(
        long,
        value_name = "engine",
        help = "Speech engine id (parakeet-v3, parakeet-ultra, parakeet-de, whisperkit-large-v3-turbo); every id runs Parakeet v3 in steno-speech-sidecar, which must sit beside steno, except parakeet-v3 on the Mac, which runs on CoreML unless speech.json chooses the sidecar."
    )]
    pub engine: Option<String>,
}

impl SpeechOptions {
    pub fn validate(&self) -> Result<(), Failure> {
        match &self.engine {
            Some(engine) if !ENGINE_IDS.contains(&engine.as_str()) => Err(Failure::usage(format!(
                "invalid value '{engine}' for '--engine <engine>': expected one of {}",
                ENGINE_IDS.join(", ")
            ))),
            _ => Ok(()),
        }
    }
}

/// The absolute path with `.` and `..` resolved lexically and no trailing
/// separator, so the same vault gets the same destination id, and a path
/// the CLI stores or prints is the same however it was spelled. Swift:
/// `URL.standardizedFileURL.path`, which does not resolve symlinks either.
pub fn standardized(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut result = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(result.components().next_back(), Some(Component::Normal(_))) {
                    result.pop();
                }
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

/// The parsed `<meeting-id>` argument.
pub fn parse_uuid(argument: &str) -> Result<Uuid, String> {
    Uuid::parse_str(argument).map_err(|_| format!("{argument} is not a UUID."))
}

/// The support paths the CLI uses for models and secrets.
pub fn paths() -> Result<StenoPaths, Failure> {
    StenoPaths::create_default().map_err(Failure::runtime)
}

/// The speech setup over `models_directory`, the speech settings read
/// from the default support directory without creating it.
pub fn speech_setup(models_directory: PathBuf) -> SpeechSetup {
    SpeechSetup::in_models_directory(
        models_directory,
        &StenoPaths::new(StenoPaths::default_support_directory()),
    )
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
    let (speech_engine, diarizer, memory): (
        Arc<dyn steno_core::SpeechEngine>,
        Arc<dyn steno_core::Diarizer>,
        Arc<dyn steno_core::SpeakerMemory>,
    ) = match engine {
        Some(engine) => {
            let speech = speech_setup(match models_directory {
                Some(directory) => standardized(directory),
                None => steno_services::speech::models_directory(settings, &paths()?),
            });
            (
                // The flag names the engine for this run, as the Swift CLI's
                // `makeSpeechEngine(engine, ...)` did; the stored id does not.
                steno_services::speech::speech_engine(engine, &speech),
                steno_services::speech::diarizer(&speech),
                Arc::new(steno_pipeline::StoreSpeakerMemory::new(store.clone())),
            )
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_standardized_lexically() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(standardized(&cwd.join("vault/")), cwd.join("vault"));
        assert_eq!(
            standardized(&cwd.join("./notes/../vault/.")),
            cwd.join("vault")
        );
        assert_eq!(standardized(Path::new("vault")), cwd.join("vault"));
        let root = cwd.ancestors().last().unwrap().to_path_buf();
        assert_eq!(standardized(&root.join("..")), root);
    }

    /// A command that opens the database twice holds its lock once: the
    /// second open must not find its own process in the way.
    #[test]
    fn opening_twice_in_one_process_holds_the_lock_once() {
        let dir = tempfile::tempdir().unwrap();
        let options = DatabaseOptions {
            database_path: Some(dir.path().join("steno.sqlite")),
        };
        options.open().unwrap();
        options.open().unwrap();
        options.open_to_read().unwrap();
        assert!(matches!(
            DatabaseLock::acquire(&dir.path().join("steno.sqlite")),
            Err(DatabaseLockError::Held(_))
        ));
    }

    /// A database named like its own lock file is refused before anything
    /// opens it.
    #[test]
    fn a_database_with_the_extension_lock_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let options = DatabaseOptions {
            database_path: Some(dir.path().join("steno.lock")),
        };
        assert!(matches!(options.path(), Err(Failure::Usage(_))));
        assert!(matches!(options.open(), Err(Failure::Usage(_))));
        assert!(!dir.path().join("steno.lock").exists());
    }

    /// A command that only reads lets go of a lock it found free once the
    /// database is open, so the app can start while it runs.
    #[test]
    fn reading_with_a_free_lock_lets_go_of_it() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("steno.sqlite");
        let options = DatabaseOptions {
            database_path: Some(database.clone()),
        };
        let _store = options.open_to_read().unwrap();
        DatabaseLock::acquire(&database).expect("the read let go of the lock");
    }

    /// `--engine` over the models directory `<dir>/models-\xff` (a name
    /// that is not UTF-8) passed to `dependencies`, as `steno process`
    /// passes the settings', with `stored` as the settings' engine: the
    /// error of the engine's `prepare`, which fails without the network.
    #[cfg(unix)]
    async fn prepare_error(dir: &Path, flag: &str, stored: &str) -> (PathBuf, String) {
        use std::os::unix::ffi::OsStrExt as _;
        let models = dir.join(std::ffi::OsStr::from_bytes(b"models-\xff"));
        let store = Arc::new(Store::open(dir.join("steno.sqlite")).unwrap());
        let settings = Settings {
            speech_engine_id: stored.to_owned(),
            ..Settings::default()
        };
        let dependencies = dependencies(
            store,
            &settings,
            Some(flag),
            Some(&models),
            None,
            None,
            MeetingEventBus::new(),
        )
        .unwrap();
        let error = dependencies.speech_engine.prepare().await.unwrap_err();
        (models, error.to_string())
    }

    /// The flag, not the stored id, picks the engine, over the models
    /// directory the caller passes. Every engine reports `parakeet-v3`, and off
    /// the Mac every id runs in the speech sidecar, so only the Mac can
    /// tell the engines apart: there the flag's `parakeet-v3` is the
    /// `CoreML` engine, the stored Whisper id the sidecar's. `CoreML`
    /// misses its bundles under the given directory's
    /// `fluidaudio/parakeet-tdt-0.6b-v3`; the sidecar would refuse the
    /// root before any download.
    #[cfg(target_os = "macos")]
    #[tokio::test(flavor = "multi_thread")]
    async fn the_engine_flag_wins_over_the_stored_engine() {
        let dir = tempfile::tempdir().unwrap();
        let (models, error) =
            prepare_error(dir.path(), "parakeet-v3", "whisperkit-large-v3-turbo").await;
        let coreml = steno_services::speech::coreml_model_directory(&models);
        assert!(
            error.contains(&coreml.display().to_string()),
            "the CoreML engine over the given models directory: {error}"
        );
    }

    /// Off the Mac the sidecar runs every id; its refusal of the root that
    /// is not UTF-8 names the `onnx/` folder of the given directory, not
    /// the default one.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[tokio::test(flavor = "multi_thread")]
    async fn the_engine_runs_over_the_models_dir_it_is_given() {
        let dir = tempfile::tempdir().unwrap();
        let (models, error) = prepare_error(dir.path(), "parakeet-v3", "parakeet-v3").await;
        assert!(
            error.contains(&models.join("onnx").display().to_string()),
            "the sidecar over the given models directory: {error}"
        );
    }
}
