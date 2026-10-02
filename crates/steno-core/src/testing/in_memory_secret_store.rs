//! A `SecretStore` over a map, for tests and the CLI's dry runs. The Swift
//! package has a 0600 file store instead; that one belongs to the CLI crate.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;

use super::lock;
use crate::{BoundaryResult, SecretKey, SecretStore};

#[derive(Debug, Default)]
pub struct InMemorySecretStore {
    secrets: Mutex<BTreeMap<String, String>>,
}

impl InMemorySecretStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A store holding `secrets` from the start.
    #[must_use]
    pub fn with(secrets: impl IntoIterator<Item = (SecretKey, String)>) -> Self {
        InMemorySecretStore {
            secrets: Mutex::new(
                secrets
                    .into_iter()
                    .map(|(key, value)| (key.0, value))
                    .collect(),
            ),
        }
    }

    /// Every stored key, sorted.
    #[must_use]
    pub fn keys(&self) -> Vec<SecretKey> {
        lock(&self.secrets).keys().cloned().map(SecretKey).collect()
    }
}

#[async_trait]
impl SecretStore for InMemorySecretStore {
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
        Ok(lock(&self.secrets).get(key.as_str()).cloned())
    }

    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        let mut secrets = lock(&self.secrets);
        match value {
            Some(value) => secrets.insert(key.0.clone(), value.to_owned()),
            None => secrets.remove(key.as_str()),
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn secrets_are_set_read_and_removed() {
        let store = InMemorySecretStore::new();
        let key = SecretKey::llm_api_key();
        assert_eq!(store.secret(&key).await.unwrap(), None);
        store.set_secret(&key, Some("sk-test")).await.unwrap();
        assert_eq!(
            store.secret(&key).await.unwrap().as_deref(),
            Some("sk-test")
        );
        assert_eq!(store.keys(), vec![SecretKey::from("llm-api-key")]);
        store.set_secret(&key, None).await.unwrap();
        assert_eq!(store.secret(&key).await.unwrap(), None);
        assert_eq!(store.keys(), Vec::new());
    }
}
