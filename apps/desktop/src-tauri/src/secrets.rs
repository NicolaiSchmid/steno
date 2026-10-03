//! Where the API key lives: the platform keyring through the `keyring`
//! crate (the login Keychain, the Credential Manager, the Secret Service
//! over D-Bus), one entry per `SecretKey` under the app's service name.
//! `Settings` never carries the key; this is the only place it is stored.
//!
//! The `SecretStore` trait here is the shape of
//! `steno_core::protocols::SecretStore` (#162) with blocking methods: the
//! keyring crate blocks, so the host wraps a call in
//! `tauri::async_runtime::spawn_blocking` (`secret_blocking`); when #162
//! merges this trait becomes an `impl steno_core::SecretStore for
//! KeyringSecretStore` with the same bodies.
//!
//! Swift: `KeychainSecretStore.swift` (service `uno.schmid.steno.mac`,
//! account the key's raw value). The shell uses its own identifier as the
//! service until WP9 moves it to the Swift one, so the cutover reads the
//! key the Swift app stored.
//!
//! Nothing in the shell reads a secret yet: the LLM client does, through
//! the host (`WP6b`, WP7). The module is the host's to take, so the dead-code
//! lint is off for it.
#![allow(dead_code)]

use std::{collections::HashMap, fmt, sync::Mutex};

use keyring::Entry;

pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// A secret's name; the keyring account.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SecretKey(pub String);

impl SecretKey {
    /// The language model service's API key.
    pub const LLM_API_KEY: &'static str = "llm-api-key";

    pub fn llm_api_key() -> Self {
        Self(Self::LLM_API_KEY.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub trait SecretStore: Send + Sync {
    fn secret(&self, key: &SecretKey) -> Result<Option<String>, BoxError>;

    /// `None` or an empty value removes the secret.
    fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> Result<(), BoxError>;
}

/// The platform keyring under one service name.
#[derive(Debug, Clone)]
pub struct KeyringSecretStore {
    service: String,
}

impl KeyringSecretStore {
    /// The service the shell stores under; WP9 changes it to the Swift
    /// app's `uno.schmid.steno.mac`.
    pub const DEFAULT_SERVICE: &'static str = "uno.schmid.steno.desktop";

    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    fn entry(&self, key: &SecretKey) -> Result<Entry, BoxError> {
        Ok(Entry::new(&self.service, key.as_str())?)
    }
}

impl Default for KeyringSecretStore {
    fn default() -> Self {
        Self::new(Self::DEFAULT_SERVICE)
    }
}

impl SecretStore for KeyringSecretStore {
    fn secret(&self, key: &SecretKey) -> Result<Option<String>, BoxError> {
        match self.entry(key)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> Result<(), BoxError> {
        let entry = self.entry(key)?;
        match value {
            Some(value) if !value.is_empty() => Ok(entry.set_password(value)?),
            _ => match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(error) => Err(error.into()),
            },
        }
    }
}

/// Secrets in memory: the tests' and previews' store.
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    secrets: Mutex<HashMap<SecretKey, String>>,
}

impl SecretStore for MemorySecretStore {
    fn secret(&self, key: &SecretKey) -> Result<Option<String>, BoxError> {
        Ok(self.secrets.lock().expect("secrets").get(key).cloned())
    }

    fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> Result<(), BoxError> {
        let mut secrets = self.secrets.lock().expect("secrets");
        match value {
            Some(value) if !value.is_empty() => {
                secrets.insert(key.clone(), value.to_owned());
            }
            _ => {
                secrets.remove(key);
            }
        }
        Ok(())
    }
}

/// Reads a secret off the async runtime: the keyring blocks (and the
/// Secret Service client must not be driven from an async context), so
/// the call runs on the blocking pool.
pub async fn secret_blocking<S: SecretStore + 'static>(
    store: std::sync::Arc<S>,
    key: SecretKey,
) -> Result<Option<String>, BoxError> {
    tauri::async_runtime::spawn_blocking(move || store.secret(&key))
        .await
        .map_err(|error| -> BoxError { Box::new(error) })?
}

#[cfg(test)]
mod tests {
    use std::sync::Once;

    use super::*;

    /// Routes the keyring crate to in-memory mock credentials, once per
    /// process, so the tests touch no real keychain, and records the
    /// service and user every entry is built with. A mock credential lives
    /// as long as its `Entry`, and the store makes a new entry per call, so
    /// the mock proves the error mapping and the names, not persistence;
    /// `the_platform_keyring_round_trips_a_secret` proves that on a machine
    /// with a keyring.
    fn use_mock_keyring() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            keyring::set_default_credential_builder(Box::new(RecordingBuilder));
        });
    }

    /// `(service, user)` of every entry built since the tests began.
    static BUILT: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

    struct RecordingBuilder;

    impl keyring::credential::CredentialBuilderApi for RecordingBuilder {
        fn build(
            &self,
            _target: Option<&str>,
            service: &str,
            user: &str,
        ) -> keyring::Result<Box<keyring::Credential>> {
            BUILT
                .lock()
                .expect("built")
                .push((service.to_owned(), user.to_owned()));
            Ok(Box::new(keyring::mock::MockCredential::default()))
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn persistence(&self) -> keyring::credential::CredentialPersistence {
            keyring::credential::CredentialPersistence::EntryOnly
        }
    }

    fn round_trip(store: &dyn SecretStore) {
        let key = SecretKey::llm_api_key();
        assert_eq!(store.secret(&key).unwrap(), None);
        store.set_secret(&key, Some("sk-test")).unwrap();
        assert_eq!(store.secret(&key).unwrap().as_deref(), Some("sk-test"));
        store.set_secret(&key, Some("sk-next")).unwrap();
        assert_eq!(store.secret(&key).unwrap().as_deref(), Some("sk-next"));
        // An empty value removes, as the Swift store does; removing twice
        // is fine.
        store.set_secret(&key, Some("")).unwrap();
        assert_eq!(store.secret(&key).unwrap(), None);
        store.set_secret(&key, None).unwrap();
        assert_eq!(store.secret(&key).unwrap(), None);
    }

    #[test]
    fn the_keyring_store_maps_no_entry_to_none_and_tolerates_a_missing_delete() {
        use_mock_keyring();
        let store = KeyringSecretStore::new("uno.schmid.steno.test");
        assert_eq!(store.service(), "uno.schmid.steno.test");
        let key = SecretKey::llm_api_key();
        assert_eq!(store.secret(&key).unwrap(), None);
        store.set_secret(&key, Some("sk-test")).unwrap();
        store.set_secret(&key, Some("")).unwrap();
        store.set_secret(&key, None).unwrap();
    }

    /// Against the real keyring of the machine running the tests; ignored
    /// in CI, where there is none (`cargo test -- --ignored` runs it).
    #[test]
    #[ignore = "needs the platform keyring"]
    fn the_platform_keyring_round_trips_a_secret() {
        round_trip(&KeyringSecretStore::new("uno.schmid.steno.test"));
    }

    #[test]
    fn the_memory_store_round_trips_a_secret() {
        round_trip(&MemorySecretStore::default());
    }

    #[test]
    fn keys_are_the_swift_accounts() {
        assert_eq!(SecretKey::llm_api_key().as_str(), "llm-api-key");
        assert_eq!(SecretKey::llm_api_key().to_string(), "llm-api-key");
        assert_eq!(
            KeyringSecretStore::default().service(),
            "uno.schmid.steno.desktop"
        );
    }

    /// The keyring entry is `(service, account)` in that order, as the
    /// Swift store names it: the store's service, the key's raw value.
    #[test]
    fn entries_are_named_by_service_then_key() {
        use_mock_keyring();
        let store = KeyringSecretStore::new("uno.schmid.steno.names");
        let key = SecretKey("other".into());
        store.secret(&key).unwrap();
        let built = BUILT.lock().expect("built");
        assert!(
            built.contains(&("uno.schmid.steno.names".into(), "other".into())),
            "{built:?}"
        );
        assert!(!built.contains(&("other".into(), "uno.schmid.steno.names".into())));
    }
}
