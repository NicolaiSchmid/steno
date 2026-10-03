//! `SecretStore` implementations: the platform keyring (the Keychain, the
//! Windows credential store) through the `keyring` crate, and the 0600
//! JSON file the Swift CLI used where no keyring is reachable
//! (`STENO_LLM_API_KEY` wins over the file). Linux uses the file for the
//! app too; the crate doc says why.
//! Swift: `apps/macos/Steno/Services/KeychainSecretStore.swift`,
//! `Sources/StenoCore/Testing/FileSecretStore.swift`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use steno_core::{SecretKey, SecretStore, StenoPaths, async_trait, protocols::BoundaryResult};

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

/// A JSON object of secrets in one file, created with mode 0600 where the
/// platform has modes. The environment overrides the LLM API key so a CLI
/// run never has to store it.
#[derive(Debug)]
pub struct FileSecretStore {
    path: PathBuf,
    environment: BTreeMap<String, String>,
    cache: Mutex<Option<BTreeMap<String, String>>>,
}

impl FileSecretStore {
    pub const API_KEY_VARIABLE: &'static str = "STENO_LLM_API_KEY";

    #[must_use]
    pub fn new(path: impl Into<PathBuf>, environment: BTreeMap<String, String>) -> Self {
        FileSecretStore {
            path: path.into(),
            environment,
            cache: Mutex::new(None),
        }
    }

    /// `secrets.json` under the support directory, reading the process
    /// environment.
    #[must_use]
    pub fn in_support_directory(support_directory: &std::path::Path) -> Self {
        Self::new(
            support_directory.join("secrets.json"),
            std::env::vars().collect(),
        )
    }

    fn load(&self) -> std::io::Result<BTreeMap<String, String>> {
        if let Some(cached) = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            return Ok(cached.clone());
        }
        let map = match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error),
        };
        *self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(map.clone());
        Ok(map)
    }

    /// Writes the file owner-only from the first byte: it is created with
    /// mode 0600 (never world-readable in between), and an existing file
    /// is truncated in place, keeping its mode.
    fn save(&self, map: &BTreeMap<String, String>) -> std::io::Result<()> {
        use std::io::Write as _;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_vec_pretty(map).map_err(std::io::Error::other)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        options.open(&self.path)?.write_all(&data)?;
        *self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(map.clone());
        Ok(())
    }
}

#[async_trait]
impl SecretStore for FileSecretStore {
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
        if key.as_str() == SecretKey::LLM_API_KEY
            && let Some(value) = self.environment.get(Self::API_KEY_VARIABLE)
            && !value.is_empty()
        {
            return Ok(Some(value.clone()));
        }
        Ok(self.load()?.get(key.as_str()).cloned())
    }

    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        let mut map = self.load()?;
        match value {
            Some(value) => {
                map.insert(key.as_str().to_owned(), value.to_owned());
            }
            None => {
                map.remove(key.as_str());
            }
        }
        self.save(&map)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_file_store_round_trips_and_the_environment_wins() {
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
        let env = FileSecretStore::new(
            dir.path().join("secrets.json"),
            BTreeMap::from([(
                FileSecretStore::API_KEY_VARIABLE.to_owned(),
                "sk-env".to_owned(),
            )]),
        );
        assert_eq!(env.secret(&key).await.unwrap().as_deref(), Some("sk-env"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_file_is_owner_only_and_a_shorter_rewrite_leaves_no_tail() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let store = FileSecretStore::new(&path, BTreeMap::new());
        let key = SecretKey::llm_api_key();
        store
            .set_secret(&key, Some("a-rather-long-key-value"))
            .await
            .unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        store.set_secret(&key, Some("k")).await.unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            serde_json::from_str::<BTreeMap<String, String>>(&text).is_ok(),
            "{text}"
        );
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
            keychain.contains(&format!("static let defaultService = \"{KEYRING_SERVICE}\"")),
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
