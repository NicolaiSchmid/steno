//! Where the API key lives.
//! Swift: `Sources/StenoCore/Protocols/SecretStore.swift`.

use std::fmt;

use async_trait::async_trait;

use super::BoundaryResult;

/// A secret's name. The app stores secrets in the platform keyring, the
/// CLI and tests in a 0600 file, the environment or memory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SecretKey(pub String);

impl SecretKey {
    /// The language model service's API key.
    pub const LLM_API_KEY: &'static str = "llm-api-key";

    #[must_use]
    pub fn llm_api_key() -> Self {
        SecretKey(Self::LLM_API_KEY.to_owned())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for SecretKey {
    fn from(text: &str) -> Self {
        SecretKey(text.to_owned())
    }
}

impl fmt::Display for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a store keeps its secrets, for the settings' wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretPlace {
    /// The platform keyring: the Keychain, the Windows credential store,
    /// the Secret Service.
    Keyring,
    /// A file only the user can read.
    File,
}

/// Where the API key lives: read, set, remove.
#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>>;

    /// `None` removes the secret.
    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()>;

    /// Where the secrets are kept; `None` when the store does not say or
    /// has not decided yet. No Swift counterpart (the Swift app keeps them
    /// in the Keychain only).
    fn place(&self) -> Option<SecretPlace> {
        None
    }
}
