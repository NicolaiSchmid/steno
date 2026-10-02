//! BCP-47 language tags.
//! Swift: `Sources/StenoCore/Model/LanguageTag.swift`.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A BCP-47 language tag (`de`, `en-US`, `zh-Hant-TW`), stored and encoded
/// as its string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LanguageTag(pub String);

impl LanguageTag {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for LanguageTag {
    fn from(text: String) -> Self {
        LanguageTag(text)
    }
}

impl From<&str> for LanguageTag {
    fn from(text: &str) -> Self {
        LanguageTag(text.to_owned())
    }
}

impl fmt::Display for LanguageTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
