//! `SecretStore` implementations: the platform keyring (the Keychain, the
//! Windows credential store) through the `keyring` crate, and the 0600
//! JSON file the Swift CLI used where no keyring is reachable
//! (`STENO_<KEY>` wins over the file). Linux uses the file for the
//! app too; the crate doc says why.
//! Swift: `apps/macos/Steno/Services/KeychainSecretStore.swift`,
//! `Sources/StenoCore/Testing/FileSecretStore.swift`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use steno_core::{SecretKey, SecretStore, StenoPaths, async_trait, protocols::BoundaryResult};

use crate::files::{Access, replace_file, restrict_new_file};

/// The service every Steno keyring entry is filed under, on every
/// platform: the Swift app's (`KeychainSecretStore.defaultService` in
/// `apps/macos/Steno/Services/KeychainSecretStore.swift`), so the Rust app
/// reads the API key the Swift app stored on the Mac. The account is the
/// `SecretKey`'s raw value, as in Swift. Swift also labels the item
/// `Steno <key>`; the `keyring` crate cannot set a label (the Keychain
/// then shows the service), and lookups match on service and account only.
pub const KEYRING_SERVICE: &str = "uno.schmid.steno.mac";

/// The platform keyring when `keyring` is set and the platform has one
/// that persists (macOS, Windows), else the secrets file under the
/// support directory (the CLI, headless machines, Linux).
#[must_use]
pub fn secret_store(keyring: bool, paths: &StenoPaths) -> Arc<dyn SecretStore> {
    if uses_file(keyring) {
        Arc::new(FileSecretStore::in_support_directory(
            &paths.support_directory,
        ))
    } else {
        Arc::new(KeyringSecretStore)
    }
}

/// Whether the file store answers: always when the keyring was not asked
/// for, and on Linux regardless.
#[must_use]
pub fn uses_file(keyring: bool) -> bool {
    !keyring || cfg!(target_os = "linux")
}

/// The platform keyring.
#[derive(Debug, Default)]
pub struct KeyringSecretStore;

impl KeyringSecretStore {
    fn entry(key: &SecretKey) -> Result<keyring::Entry, keyring::Error> {
        keyring::Entry::new(KEYRING_SERVICE, key.as_str())
    }
}

#[async_trait]
impl SecretStore for KeyringSecretStore {
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
        match Self::entry(key)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(Box::new(error)),
        }
    }

    /// `None` and an empty value remove the entry, as in Swift.
    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        let entry = Self::entry(key)?;
        match value.filter(|value| !value.is_empty()) {
            Some(value) => entry.set_password(value)?,
            None => match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(error) => return Err(Box::new(error)),
            },
        }
        Ok(())
    }
}

/// A JSON object of secrets in one file, owner-only (0600) where the
/// platform has modes. An environment variable `STENO_<KEY>`
/// (`llm-api-key` becomes `STENO_LLM_API_KEY`) overrides the file, so a
/// CLI run never has to store a key.
///
/// Every read goes to the file; nothing is cached, so the CLI and the app
/// (which share the file on Linux) see each other's writes. A write is a
/// read-modify-write under an advisory lock on `<file>.lock`, so two
/// processes writing different keys both keep theirs, and it replaces the
/// file by renaming a complete, synced temporary file over it, so a crash
/// or a full disk leaves the old file or the new one, never a torn one.
/// An empty file reads as no secrets, as in Swift.
#[derive(Debug)]
pub struct FileSecretStore {
    path: PathBuf,
    environment: BTreeMap<String, String>,
}

impl FileSecretStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, environment: BTreeMap<String, String>) -> Self {
        FileSecretStore {
            path: path.into(),
            environment,
        }
    }

    /// `secrets.json` under the support directory, reading the process
    /// environment.
    #[must_use]
    pub fn in_support_directory(support_directory: &Path) -> Self {
        Self::new(
            support_directory.join("secrets.json"),
            std::env::vars().collect(),
        )
    }

    /// The variable that overrides `key`. Swift: `environmentVariable(for:)`.
    #[must_use]
    pub fn environment_variable(key: &SecretKey) -> String {
        format!("STENO_{}", key.as_str().to_uppercase().replace('-', "_"))
    }

    fn read(&self) -> std::io::Result<BTreeMap<String, String>> {
        match std::fs::read(&self.path) {
            Ok(bytes) if bytes.is_empty() => Ok(BTreeMap::new()),
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(error),
        }
    }

    /// The file next to the secrets with `suffix` appended to its name.
    fn beside(&self, suffix: &str) -> PathBuf {
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(suffix);
        self.path.with_file_name(name)
    }

    /// Applies `change` to the stored map under the lock and writes the
    /// result atomically.
    fn update(&self, change: impl FnOnce(&mut BTreeMap<String, String>)) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock = restrict_new_file(std::fs::OpenOptions::new().create(true).write(true))
            .open(self.beside(".lock"))?;
        lock.lock()?;
        let mut map = self.read()?;
        change(&mut map);
        let data = serde_json::to_vec_pretty(&map).map_err(std::io::Error::other)?;
        replace_file(&self.path, &data, Access::OwnerOnly)
    }
}

#[async_trait]
impl SecretStore for FileSecretStore {
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
        if let Some(value) = self.environment.get(&Self::environment_variable(key))
            && !value.is_empty()
        {
            return Ok(Some(value.clone()));
        }
        Ok(self.read()?.get(key.as_str()).cloned())
    }

    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        self.update(|map| match value {
            Some(value) => {
                map.insert(key.as_str().to_owned(), value.to_owned());
            }
            None => {
                map.remove(key.as_str());
            }
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_file_store_round_trips_and_the_environment_wins_for_every_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().join("secrets.json"), BTreeMap::new());
        let key = SecretKey::llm_api_key();
        assert_eq!(store.secret(&key).await.unwrap(), None);
        store.set_secret(&key, Some("sk-1")).await.unwrap();
        assert_eq!(store.secret(&key).await.unwrap().as_deref(), Some("sk-1"));
        let again = FileSecretStore::new(dir.path().join("secrets.json"), BTreeMap::new());
        assert_eq!(again.secret(&key).await.unwrap().as_deref(), Some("sk-1"));
        store.set_secret(&key, None).await.unwrap();
        assert_eq!(store.secret(&key).await.unwrap(), None);

        assert_eq!(
            FileSecretStore::environment_variable(&key),
            "STENO_LLM_API_KEY"
        );
        let identity = SecretKey::from("handover-identity");
        assert_eq!(
            FileSecretStore::environment_variable(&identity),
            "STENO_HANDOVER_IDENTITY"
        );
        let env = FileSecretStore::new(
            dir.path().join("secrets.json"),
            BTreeMap::from([
                ("STENO_LLM_API_KEY".to_owned(), "sk-env".to_owned()),
                ("STENO_HANDOVER_IDENTITY".to_owned(), "pem-env".to_owned()),
            ]),
        );
        assert_eq!(env.secret(&key).await.unwrap().as_deref(), Some("sk-env"));
        assert_eq!(
            env.secret(&identity).await.unwrap().as_deref(),
            Some("pem-env")
        );
    }

    #[tokio::test]
    async fn an_empty_file_reads_as_no_secrets_and_takes_a_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        std::fs::write(&path, b"").unwrap();
        let store = FileSecretStore::new(&path, BTreeMap::new());
        let key = SecretKey::llm_api_key();
        assert_eq!(store.secret(&key).await.unwrap(), None);
        store.set_secret(&key, Some("sk-1")).await.unwrap();
        assert_eq!(store.secret(&key).await.unwrap().as_deref(), Some("sk-1"));
    }

    #[tokio::test]
    async fn one_store_sees_another_store_s_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let first = FileSecretStore::new(&path, BTreeMap::new());
        let second = FileSecretStore::new(&path, BTreeMap::new());
        let (a, b) = (SecretKey::from("a"), SecretKey::from("b"));
        assert_eq!(first.secret(&a).await.unwrap(), None);
        second.set_secret(&a, Some("1")).await.unwrap();
        assert_eq!(first.secret(&a).await.unwrap().as_deref(), Some("1"));
        first.set_secret(&b, Some("2")).await.unwrap();
        assert_eq!(second.secret(&a).await.unwrap().as_deref(), Some("1"));
        assert_eq!(second.secret(&b).await.unwrap().as_deref(), Some("2"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_file_is_owner_only_whatever_mode_it_had() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let store = FileSecretStore::new(&path, BTreeMap::new());
        let key = SecretKey::llm_api_key();
        store
            .set_secret(&key, Some("a-rather-long-key-value"))
            .await
            .unwrap();
        assert_eq!(mode(&path), 0o600);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        store.set_secret(&key, Some("k")).await.unwrap();
        assert_eq!(
            mode(&path),
            0o600,
            "a write replaces the file, mode and all"
        );
        assert_eq!(mode(&store.beside(".lock")), 0o600);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            serde_json::from_str::<BTreeMap<String, String>>(&text).unwrap(),
            BTreeMap::from([(key.as_str().to_owned(), "k".to_owned())])
        );
    }

    /// Keys per writer in the concurrency tests.
    const ROUNDS: usize = 20;
    const WRITER_PATH: &str = "STENO_TEST_SECRETS_WRITER_PATH";
    const WRITER_PREFIX: &str = "STENO_TEST_SECRETS_WRITER_PREFIX";

    async fn write_keys(path: &Path, prefix: &str) {
        let store = FileSecretStore::new(path, BTreeMap::new());
        for round in 0..ROUNDS {
            store
                .set_secret(&SecretKey(format!("{prefix}-{round}")), Some("value"))
                .await
                .unwrap();
        }
    }

    /// Reads the file until `done`, failing on a torn read.
    fn watch_for_tears(path: &Path, mut done: impl FnMut() -> bool) {
        while !done() {
            if let Ok(bytes) = std::fs::read(path) {
                assert!(
                    serde_json::from_slice::<BTreeMap<String, String>>(&bytes).is_ok(),
                    "torn read: {:?}",
                    String::from_utf8_lossy(&bytes)
                );
            }
        }
    }

    fn assert_every_key(path: &Path, prefixes: &[&str]) {
        let map: BTreeMap<String, String> =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        for prefix in prefixes {
            for round in 0..ROUNDS {
                assert!(
                    map.contains_key(&format!("{prefix}-{round}")),
                    "{prefix}-{round} was lost"
                );
            }
        }
    }

    #[test]
    fn two_threads_writing_different_keys_lose_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let writers: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|prefix| {
                let path = path.clone();
                std::thread::spawn(move || {
                    tokio::runtime::Builder::new_current_thread()
                        .build()
                        .unwrap()
                        .block_on(write_keys(&path, prefix));
                })
            })
            .collect();
        watch_for_tears(&path, || {
            writers.iter().all(std::thread::JoinHandle::is_finished)
        });
        for writer in writers {
            writer.join().unwrap();
        }
        assert_every_key(&path, &["a", "b"]);
    }

    /// One writer of [`two_processes_writing_different_keys_lose_nothing`];
    /// does nothing unless that test started this process.
    #[tokio::test]
    async fn secrets_writer_process() {
        if let (Ok(path), Ok(prefix)) = (std::env::var(WRITER_PATH), std::env::var(WRITER_PREFIX)) {
            write_keys(Path::new(&path), &prefix).await;
        }
    }

    #[test]
    fn two_processes_writing_different_keys_lose_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let mut writers: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|prefix| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "secrets::tests::secrets_writer_process",
                        "--exact",
                        "--test-threads=1",
                        "--quiet",
                    ])
                    .env(WRITER_PATH, &path)
                    .env(WRITER_PREFIX, prefix)
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        watch_for_tears(&path, || {
            writers
                .iter_mut()
                .all(|writer| writer.try_wait().unwrap().is_some())
        });
        for writer in &mut writers {
            assert!(writer.wait().unwrap().success());
        }
        assert_every_key(&path, &["a", "b"]);
    }

    #[test]
    fn linux_uses_the_file_store_even_when_the_keyring_is_asked_for() {
        assert!(uses_file(false));
        assert_eq!(uses_file(true), cfg!(target_os = "linux"));
    }

    /// Reads a Swift source from the repository root.
    fn swift_source(path: &str) -> String {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        std::fs::read_to_string(root.join(path)).unwrap_or_else(|error| panic!("{path}: {error}"))
    }

    #[test]
    fn the_keyring_names_are_the_swift_app_s() {
        let keychain = swift_source("apps/macos/Steno/Services/KeychainSecretStore.swift");
        assert!(
            keychain.contains(&format!(
                "static let defaultService = \"{KEYRING_SERVICE}\""
            )),
            "KeychainSecretStore.defaultService is not {KEYRING_SERVICE}"
        );
        assert!(
            keychain.contains("kSecAttrAccount as String: key.rawValue"),
            "the Swift account is no longer the key's raw value"
        );
        let keys = swift_source("Sources/StenoCore/Protocols/SecretStore.swift");
        assert!(
            keys.contains(&format!(
                "static let llmAPIKey = SecretKey(rawValue: \"{}\")",
                SecretKey::LLM_API_KEY
            )),
            "SecretKey.llmAPIKey is not {}",
            SecretKey::LLM_API_KEY
        );
    }
}
