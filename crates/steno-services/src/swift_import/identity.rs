//! The exported Swift identity on its way into the secret store: the
//! PKCS#12 file decoded into the PEM bundle `steno-handover` reads, checked,
//! and stored with its fingerprint recorded.

use base64::Engine as _;
use p12_keystore::{KeyStore, KeyStoreEntry, Pkcs12ImportPolicy};
use steno_core::SecretStore;
use steno_handover::{FingerprintRecord, HandoverIdentity, IdentityError};

/// What went wrong between the export and the store.
#[derive(Debug, thiserror::Error)]
pub enum ImportedIdentityError {
    #[error("the exported identity does not decode: {0}")]
    Decode(String),
    #[error("the exported identity is not the certificate the keychain labelled")]
    OtherCertificate,
    #[error("the exported identity is unusable: {0}")]
    Unusable(String),
    #[error("the identity could not be stored: {0}")]
    Store(String),
    /// The identity is stored but its fingerprint is not recorded, so the
    /// handover refuses it until a store records it.
    #[error("the identity was stored, but its fingerprint could not be recorded: {0}")]
    Record(String),
}

/// The identity in `pkcs12` (written by `SecItemExport` under `passphrase`)
/// as the PEM bundle [`HandoverIdentity::from_pem`] reads. Refused when the
/// file holds another certificate than `certificate_der`, or a key that
/// does not sign for it.
pub fn decode_pkcs12(
    pkcs12: &[u8],
    passphrase: &str,
    certificate_der: &[u8],
) -> Result<(HandoverIdentity, String), ImportedIdentityError> {
    let store = KeyStore::from_pkcs12(pkcs12, passphrase, Pkcs12ImportPolicy::Relaxed)
        .map_err(|error| ImportedIdentityError::Decode(error.to_string()))?;
    let chain = store
        .entries()
        .find_map(|(_, entry)| match entry {
            KeyStoreEntry::PrivateKeyChain(chain)
                if chain
                    .certs()
                    .first()
                    .is_some_and(|leaf| leaf.as_der() == certificate_der) =>
            {
                Some(chain)
            }
            _ => None,
        })
        .ok_or(ImportedIdentityError::OtherCertificate)?;
    let bundle =
        pem_block("CERTIFICATE", certificate_der) + &pem_block("PRIVATE KEY", chain.key().as_der());
    let identity = HandoverIdentity::from_pem(&bundle)
        .map_err(|error| ImportedIdentityError::Unusable(error.to_string()))?;
    // rustls refuses a key whose public half is not the certificate's.
    identity
        .server_config()
        .map_err(|error| ImportedIdentityError::Unusable(error.to_string()))?;
    Ok((identity, bundle))
}

/// Stores an imported identity through [`HandoverIdentity::store`],
/// replacing whatever identity was there, then records its fingerprint so
/// the handover's guard accepts it at the next load. A record that fails
/// after the secret was written is [`ImportedIdentityError::Record`].
///
/// Replacing an identity destroys it, so this warns first with both
/// fingerprints (read from the record, so no keychain prompt); a rerun over
/// the same fingerprint says nothing.
pub async fn store_imported_identity(
    secrets: &dyn SecretStore,
    record: &dyn FingerprintRecord,
    bundle: &str,
) -> Result<(), ImportedIdentityError> {
    let identity = HandoverIdentity::from_pem(bundle)
        .map_err(|error| ImportedIdentityError::Unusable(error.to_string()))?;
    let imported = steno_handover::identity::hex(&identity.fingerprint());
    if let Ok(Some(recorded)) = record.recorded()
        && recorded != imported
    {
        tracing::warn!(
            %recorded,
            %imported,
            "the Swift import replaces the handover identity recorded here (a desktop-id build's)"
        );
    }
    identity
        .store(secrets, record)
        .await
        .map_err(|error| match error {
            IdentityError::Record(error) => ImportedIdentityError::Record(error.to_string()),
            error => ImportedIdentityError::Store(error.to_string()),
        })
}

fn pem_block(tag: &str, der: &[u8]) -> String {
    use std::fmt::Write as _;
    let body = base64::engine::general_purpose::STANDARD.encode(der);
    let mut text = format!("-----BEGIN {tag}-----\n");
    for line in body.as_bytes().chunks(64) {
        text.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        text.push('\n');
    }
    let _ = writeln!(text, "-----END {tag}-----");
    text
}
