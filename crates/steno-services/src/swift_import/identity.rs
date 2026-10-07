//! The exported Swift identity on its way into the secret store: the
//! PKCS#12 file `SecItemExport` wrote, decoded into the PEM bundle
//! `steno-handover` reads, checked, and stored with its fingerprint.

use base64::Engine as _;
use p12_keystore::{KeyStore, KeyStoreEntry, Pkcs12ImportPolicy};
use steno_core::SecretStore;
use steno_handover::HandoverIdentity;

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
}

/// The identity in `pkcs12`, a file `SecItemExport` wrote under
/// `passphrase` (3DES key bag, 40-bit RC2 certificate bag): its private
/// key (PKCS#8) and the certificate whose DER is `certificate_der`, the
/// one the keychain query found by label, as the PEM bundle
/// [`HandoverIdentity::from_pem`] reads. Refused when the file holds
/// another certificate, or a key that does not sign for it.
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

/// Stores an imported identity: the PEM bundle under
/// [`HandoverIdentity::SECRET_KEY`], replacing whatever identity was there
/// (a desktop-id build's, D5). The one place the import writes the
/// identity. Once the handover's fingerprint record lands (#221), this
/// becomes `HandoverIdentity::store(secrets, &record)`, which records the
/// new fingerprint with it, so the guard accepts the Swift identity.
pub async fn store_imported_identity(
    secrets: &dyn SecretStore,
    bundle: &str,
) -> Result<(), ImportedIdentityError> {
    secrets
        .set_secret(&HandoverIdentity::secret_key(), Some(bundle))
        .await
        .map_err(|error| ImportedIdentityError::Store(error.to_string()))
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
