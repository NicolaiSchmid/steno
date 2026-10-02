//! `SecretStore` implementations: the platform keyring (the Keychain, the
//! Windows credential store, the kernel keyring on Linux) through the
//! `keyring` crate, and the 0600 JSON file the Swift CLI used where no
//! keyring is reachable (`STENO_LLM_API_KEY` wins over the file).
//! Swift: `KeychainSecretStore`, `FileSecretStore`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use steno_core::{SecretKey, SecretStore, async_trait, protocols::BoundaryResult};

/// The service name every Steno entry is filed under.
pub const KEYRING_SERVICE: &str = "uno.schmid.steno";

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

    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        let entry = Self::entry(key)?;
        match value {
            Some(value) => entry.set_password(value)?,
            None => match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(error) => return Err(Box::new(error)),
            },
        }
        Ok(())
    }
}

/// A JSON object of secrets in one file, written with mode 0600 where the
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
        if let Some(cached) = self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref() {
            return Ok(cached.clone());
        }
        let map = match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error),
        };
        *self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(map.clone());
        Ok(map)
    }

    fn save(&self, map: &BTreeMap<String, String>) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_vec_pretty(map).map_err(std::io::Error::other)?;
        std::fs::write(&self.path, data)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        *self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(map.clone());
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
            BTreeMap::from([(FileSecretStore::API_KEY_VARIABLE.to_owned(), "sk-env".to_owned())]),
        );
        assert_eq!(env.secret(&key).await.unwrap().as_deref(), Some("sk-env"));
    }
}
