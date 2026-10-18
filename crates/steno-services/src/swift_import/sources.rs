//! Where the import reads from: the Swift app's defaults domain and its
//! keychain items, each behind a trait so the tests use fakes. Only the Mac
//! implements them.

/// The Swift app's `UserDefaults` domain.
pub trait SwiftDefaults: Send + Sync {
    /// The domain as a property list, in any of the formats the `plist`
    /// crate reads; an empty dictionary when the domain does not exist.
    fn export(&self) -> Result<Vec<u8>, String>;
}

/// Where the import's own marker ([`IMPORT_DONE_ENTRY`](super::IMPORT_DONE_ENTRY))
/// is written.
pub trait ImportMarker: Send + Sync {
    /// Adds the marker (an item of this app's own, so macOS asks nothing);
    /// one already there is left as it is.
    fn mark_import_done(&self) -> Result<(), String>;
}

/// The login keychain items the import reads: the Swift app's handover
/// identity and API key, a desktop-id build's `handover-identity` entry, and
/// the import's own marker.
pub trait SwiftKeychain: ImportMarker {
    /// The DER of every certificate labelled
    /// [`SWIFT_IDENTITY_LABEL`](super::SWIFT_IDENTITY_LABEL), in the
    /// keychain's order. There may be several, one per keychain in the
    /// search list. Reads no secret, so macOS asks nothing.
    fn swift_certificates(&self) -> Result<Vec<Vec<u8>>, String>;
    /// Who owns the API key item under
    /// [`KEYRING_SERVICE`](crate::secrets::KEYRING_SERVICE). Asks nothing.
    fn api_key_item(&self) -> Result<ApiKeyItem, String>;
    /// Whether a `handover-identity` entry exists. Asks nothing.
    fn has_stored_identity(&self) -> Result<bool, String>;
    /// Whether the import marker exists. Asks nothing.
    fn import_done(&self) -> Result<bool, String>;
    /// The API key; macOS may ask for the login password once. `None` when
    /// the item is gone.
    fn read_api_key(&self) -> Result<Option<String>, KeychainRefusal>;
    /// The identity of `certificate_der` as PKCS#12 under `passphrase`;
    /// macOS asks for the login password once.
    fn export_identity(
        &self,
        certificate_der: &[u8],
        passphrase: &str,
    ) -> Result<Vec<u8>, KeychainRefusal>;
}

/// The API key item in the keychain, by who stored it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyItem {
    /// There is none.
    Missing,
    /// The Swift app's.
    Swift,
    /// Another app's (a desktop-id build's), whose read may prompt.
    Other,
}

/// A keychain read that did not happen: the user denied it, or it failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}{}", if *.denied { "denied: " } else { "" }, .detail)]
pub struct KeychainRefusal {
    /// The user, or a disabled prompt, refused it.
    pub denied: bool,
    /// What failed, for the log.
    pub detail: String,
}

#[cfg(target_os = "macos")]
pub use mac::{DefaultsCommand, LoginKeychain};

#[cfg(target_os = "macos")]
mod mac {
    use security_framework::certificate::SecCertificate;
    use security_framework::identity::SecIdentity;
    use security_framework::item::{ItemClass, ItemSearchOptions, Limit, Reference, SearchResult};
    use security_framework::os::macos::identity::SecIdentityExt as _;
    use security_framework::os::macos::keychain::SecKeychain;
    use steno_core::SecretKey;
    use steno_handover::HandoverIdentity;

    use super::{ApiKeyItem, ImportMarker, KeychainRefusal, SwiftDefaults, SwiftKeychain};
    use crate::secrets::KEYRING_SERVICE;
    use crate::swift_import::{
        IMPORT_DONE_ENTRY, SWIFT_API_KEY_LABEL, SWIFT_DEFAULTS_DOMAIN, SWIFT_IDENTITY_LABEL,
    };

    /// `/usr/bin/defaults export uno.schmid.steno.mac -`.
    #[derive(Debug, Default)]
    pub struct DefaultsCommand;

    impl SwiftDefaults for DefaultsCommand {
        fn export(&self) -> Result<Vec<u8>, String> {
            let output = std::process::Command::new("/usr/bin/defaults")
                .args(["export", SWIFT_DEFAULTS_DOMAIN, "-"])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .map_err(|error| format!("defaults export did not run: {error}"))?;
            if !output.status.success() {
                return Err(format!("defaults export ended with {}", output.status));
            }
            Ok(output.stdout)
        }
    }

    /// The login keychain, or the keychains a test names.
    #[derive(Default)]
    pub struct LoginKeychain {
        /// The search list; empty for the user's default list.
        pub keychains: Vec<SecKeychain>,
    }

    impl std::fmt::Debug for LoginKeychain {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("LoginKeychain")
                .field("keychains", &self.keychains.len())
                .finish()
        }
    }

    impl LoginKeychain {
        fn search(&self) -> ItemSearchOptions {
            let mut options = ItemSearchOptions::new();
            if !self.keychains.is_empty() {
                options.keychains(&self.keychains);
            }
            options
        }

        /// The certificates labelled [`SWIFT_IDENTITY_LABEL`]. Identity
        /// queries ignore the label (#88), so a second attribute-only query
        /// checks the label of everything returned.
        fn certificates(&self) -> Result<Vec<SecCertificate>, String> {
            let refs = match self
                .search()
                .class(ItemClass::certificate())
                .label(SWIFT_IDENTITY_LABEL)
                .load_refs(true)
                .limit(Limit::All)
                .search()
            {
                Ok(results) => results,
                Err(error) if error.code() == ITEM_NOT_FOUND => return Ok(Vec::new()),
                Err(error) => return Err(format!("the certificate query failed: {error}")),
            };
            let labels = self
                .search()
                .class(ItemClass::certificate())
                .label(SWIFT_IDENTITY_LABEL)
                .load_attributes(true)
                .limit(Limit::All)
                .search()
                .map_err(|error| format!("the certificate label query failed: {error}"))?;
            let foreign = labels
                .iter()
                .any(|result| label(result).as_deref() != Some(SWIFT_IDENTITY_LABEL));
            if foreign {
                return Err(format!(
                    "the keychain returned a certificate not labelled \"{SWIFT_IDENTITY_LABEL}\""
                ));
            }
            Ok(refs
                .into_iter()
                .filter_map(|result| match result {
                    SearchResult::Ref(Reference::Certificate(certificate)) => Some(certificate),
                    _ => None,
                })
                .collect())
        }

        /// The attributes of the generic passwords under [`KEYRING_SERVICE`]
        /// and `account`; no data is read, so macOS asks nothing.
        fn passwords(&self, account: &str) -> Result<Vec<SearchResult>, String> {
            match self
                .search()
                .class(ItemClass::generic_password())
                .service(KEYRING_SERVICE)
                .account(account)
                .load_attributes(true)
                .limit(Limit::All)
                .search()
            {
                Ok(results) => Ok(results),
                Err(error) if error.code() == ITEM_NOT_FOUND => Ok(Vec::new()),
                Err(error) => Err(format!("the {account} query failed: {error}")),
            }
        }
    }

    /// `errSecItemNotFound`.
    const ITEM_NOT_FOUND: i32 = -25300;
    /// `errSecDuplicateItem`.
    const DUPLICATE_ITEM: i32 = -25299;

    fn label(result: &SearchResult) -> Option<String> {
        result
            .simplify_dict()
            .and_then(|attributes| attributes.get("labl").cloned())
    }

    /// A read that failed without the user refusing it.
    fn failed(detail: String) -> KeychainRefusal {
        KeychainRefusal {
            denied: false,
            detail,
        }
    }

    impl SwiftKeychain for LoginKeychain {
        fn swift_certificates(&self) -> Result<Vec<Vec<u8>>, String> {
            Ok(self
                .certificates()?
                .iter()
                .map(SecCertificate::to_der)
                .collect())
        }

        fn api_key_item(&self) -> Result<ApiKeyItem, String> {
            let items = self.passwords(SecretKey::LLM_API_KEY)?;
            Ok(
                if items
                    .iter()
                    .any(|item| label(item).as_deref() == Some(SWIFT_API_KEY_LABEL))
                {
                    ApiKeyItem::Swift
                } else if items.is_empty() {
                    ApiKeyItem::Missing
                } else {
                    ApiKeyItem::Other
                },
            )
        }

        fn has_stored_identity(&self) -> Result<bool, String> {
            Ok(!self.passwords(HandoverIdentity::SECRET_KEY)?.is_empty())
        }

        fn import_done(&self) -> Result<bool, String> {
            Ok(!self.passwords(IMPORT_DONE_ENTRY)?.is_empty())
        }

        fn read_api_key(&self) -> Result<Option<String>, KeychainRefusal> {
            let entry = keyring::Entry::new(KEYRING_SERVICE, SecretKey::LLM_API_KEY)
                .map_err(|error| failed(error.to_string()))?;
            match entry.get_password() {
                Ok(key) => Ok(Some(key)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(error) => Err(KeychainRefusal {
                    // keyring wraps the status in text only; assume refusal.
                    denied: true,
                    detail: error.to_string(),
                }),
            }
        }

        fn export_identity(
            &self,
            certificate_der: &[u8],
            passphrase: &str,
        ) -> Result<Vec<u8>, KeychainRefusal> {
            let certificate = self
                .certificates()
                .map_err(failed)?
                .into_iter()
                .find(|certificate| certificate.to_der() == certificate_der)
                .ok_or_else(|| {
                    failed(format!(
                        "no certificate labelled \"{SWIFT_IDENTITY_LABEL}\" is the one chosen"
                    ))
                })?;
            // The search list for the private key.
            let identity =
                SecIdentity::with_certificate(&self.keychains, &certificate).map_err(|error| {
                    failed(format!("SecIdentityCreateWithCertificate failed: {error}"))
                })?;
            steno_macos::keychain::export_pkcs12(&identity, passphrase).map_err(|error| {
                KeychainRefusal {
                    denied: error.is_denied(),
                    detail: error.to_string(),
                }
            })
        }
    }

    impl ImportMarker for LoginKeychain {
        fn mark_import_done(&self) -> Result<(), String> {
            // The search list's first keychain, else the default one.
            let keychain = match self.keychains.first() {
                Some(keychain) => keychain.clone(),
                None => SecKeychain::default()
                    .map_err(|error| format!("no default keychain: {error}"))?,
            };
            match keychain.add_generic_password(KEYRING_SERVICE, IMPORT_DONE_ENTRY, b"1") {
                Ok(()) => Ok(()),
                Err(error) if error.code() == DUPLICATE_ITEM => Ok(()),
                Err(error) => Err(format!(
                    "the {IMPORT_DONE_ENTRY} entry was not added: {error}"
                )),
            }
        }
    }
}
