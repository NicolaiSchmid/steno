//! Where the import reads from: the Swift app's defaults domain and its
//! keychain items, each behind a trait so the tests use fakes. The real
//! sources are the Mac's (`DefaultsCommand`, `LoginKeychain`); off the Mac
//! nothing implements them, as nothing is imported there.

/// The Swift app's `UserDefaults` domain.
pub trait SwiftDefaults: Send + Sync {
    /// The domain as a property list, in any of the formats the `plist`
    /// crate reads; an empty dictionary when the domain does not exist.
    fn export(&self) -> Result<Vec<u8>, String>;
}

/// The Swift app's two keychain items in the login keychain: the handover
/// identity (a certificate and a key item, `IdentityKeychain`) and the API
/// key (a generic password, `KeychainSecretStore`).
pub trait SwiftKeychain: Send + Sync {
    /// The DER of the certificate labelled
    /// [`SWIFT_IDENTITY_LABEL`](super::SWIFT_IDENTITY_LABEL), or `None`.
    /// A certificate query reads no secret, so macOS asks nothing.
    fn swift_certificate(&self) -> Result<Option<Vec<u8>>, String>;
    /// Whose API key item is filed under
    /// [`KEYRING_SERVICE`](crate::secrets::KEYRING_SERVICE) and the key's
    /// account: the Swift app's when it is labelled
    /// [`SWIFT_API_KEY_LABEL`](super::SWIFT_API_KEY_LABEL). An attribute
    /// query, which asks nothing.
    fn api_key_item(&self) -> Result<ApiKeyItem, String>;
    /// Whether a `handover-identity` entry is filed under
    /// [`KEYRING_SERVICE`](crate::secrets::KEYRING_SERVICE). An attribute
    /// query, which asks nothing.
    fn has_stored_identity(&self) -> Result<bool, String>;
    /// The API key; macOS asks for the login password once unless this
    /// app is already on the item's access list. `None` when the item is
    /// gone.
    fn read_api_key(&self) -> Result<Option<String>, KeychainRefusal>;
    /// The identity of the certificate whose DER is `certificate_der` (the
    /// one `swift_certificate` found), as PKCS#12 under `passphrase`;
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
    /// The Swift app's, labelled
    /// [`SWIFT_API_KEY_LABEL`](super::SWIFT_API_KEY_LABEL).
    Swift,
    /// Another app's under the same service and account: a desktop-id
    /// build's, whose read may prompt.
    Other,
}

/// A keychain read that did not happen: the user chose Deny, or the item
/// could not be read.
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

    use super::{ApiKeyItem, KeychainRefusal, SwiftDefaults, SwiftKeychain};
    use crate::secrets::KEYRING_SERVICE;
    use crate::swift_import::{SWIFT_API_KEY_LABEL, SWIFT_DEFAULTS_DOMAIN, SWIFT_IDENTITY_LABEL};

    /// `/usr/bin/defaults export uno.schmid.steno.mac -`: the domain read
    /// by name, whatever this app's own identifier is.
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

    /// The login keychain, or the keychains a test names, through
    /// `security-framework`'s safe wrappers and `steno-macos`'s export.
    #[derive(Default)]
    pub struct LoginKeychain {
        /// The search list; empty for the user's default list (the login
        /// keychain), a throwaway keychain in the keychain test.
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

        /// The certificates labelled [`SWIFT_IDENTITY_LABEL`], in the
        /// keychain's order. Certificate queries honour the label (identity
        /// queries do not, #88), and a second, attribute-only query checks
        /// the label of everything the first one could have returned, as
        /// `IdentityKeychain` checks it on the returned attributes.
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

        /// The attributes of the generic passwords filed under
        /// [`KEYRING_SERVICE`] and `account`; no item's data is read, so
        /// macOS asks nothing.
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

    /// The label (`labl`) of an attribute query's result.
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
        fn swift_certificate(&self) -> Result<Option<Vec<u8>>, String> {
            Ok(self.certificates()?.first().map(SecCertificate::to_der))
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

        fn read_api_key(&self) -> Result<Option<String>, KeychainRefusal> {
            let entry = keyring::Entry::new(KEYRING_SERVICE, SecretKey::LLM_API_KEY)
                .map_err(|error| failed(error.to_string()))?;
            match entry.get_password() {
                Ok(key) => Ok(Some(key)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(error) => Err(KeychainRefusal {
                    // The keyring crate wraps the status in text only; a
                    // refused read is what a failed one almost always is.
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
                        "no certificate labelled \"{SWIFT_IDENTITY_LABEL}\" is the one found"
                    ))
                })?;
            // The first argument is the search list for the private key:
            // the default list in the product, the test's keychain there.
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
}
