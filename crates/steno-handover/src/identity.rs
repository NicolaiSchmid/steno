//! The TLS identity the listener presents: a self-signed P-256 leaf
//! certificate and its key. Read it as mint ([`HandoverIdentity::mint`]),
//! store and load ([`HandoverIdentity::load_or_create`] through the
//! [`SecretStore`]), serve ([`HandoverIdentity::server_config`]).
//! Swift: `Identity/MintedIdentity.swift`, `Identity/HandoverIdentity.swift`
//! and `Identity/IdentityKeychain.swift`.
//!
//! The Swift app keeps the identity in the login keychain as a certificate
//! and a key item. Here the whole identity is one secret, a PEM bundle
//! (certificate, then PKCS#8 key) under [`HandoverIdentity::SECRET_KEY`], so
//! the platform keyring, a `0600` file or memory all hold it the same way.
//! Losing the identity means re-pairing every phone, on both sides, so the
//! loader never mints one while phones are paired or after one was in use
//! ([`HandoverIdentity::load_or_create`], over the identity's fingerprint
//! in a [`FingerprintRecord`]). Only the user's Pair again mints over one
//! ([`HandoverIdentity::mint_anew`]); one that stopped is finished by the
//! next load.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
};
use rustls::ServerConfig;
use rustls_pki_types::pem::PemObject as _;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest as _, Sha256};
use steno_core::{BoxError, SecretKey, SecretStore, Store, StoreError};
use thiserror::Error;
use uuid::Uuid;

/// The identity: the leaf certificate (for the fingerprint the phone pins
/// and the computer id derived from it) and the private key rustls
/// terminates TLS with.
pub struct HandoverIdentity {
    certificate_der: Vec<u8>,
    private_key: PrivateKeyDer<'static>,
}

impl std::fmt::Debug for HandoverIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandoverIdentity")
            .field("fingerprint", &hex(&self.fingerprint()))
            .finish_non_exhaustive()
    }
}

impl HandoverIdentity {
    /// The secret the identity lives under.
    pub const SECRET_KEY: &'static str = "handover-identity";

    /// Ten years, the whole life of the identity (rotation is a non-goal).
    pub const VALIDITY_SECONDS: i64 = 10 * 365 * 24 * 60 * 60;

    /// [`SecretKey`] for [`HandoverIdentity::SECRET_KEY`].
    #[must_use]
    pub fn secret_key() -> SecretKey {
        SecretKey::from(Self::SECRET_KEY)
    }

    /// Mints a fresh identity: P-256, self-signed, ten years,
    /// `CN=<common_name>` (`Steno on <computer name>` in the product), not a
    /// CA, `digitalSignature` only (RFC 5480 §3: an EC key signs), server
    /// authentication, one DNS SAN. `now` is the start of validity; a
    /// minute of clock skew is absorbed by backdating.
    pub fn mint(common_name: &str, now: DateTime<Utc>) -> Result<Self, IdentityError> {
        let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
        let mut params = CertificateParams::new(vec![Self::san_label(common_name)])?;
        let mut name = DistinguishedName::new();
        name.push(DnType::CommonName, common_name);
        params.distinguished_name = name;
        let not_before = now.timestamp() - 60;
        params.not_before = time::OffsetDateTime::from_unix_timestamp(not_before)?;
        params.not_after =
            time::OffsetDateTime::from_unix_timestamp(not_before + Self::VALIDITY_SECONDS)?;
        params.is_ca = IsCa::ExplicitNoCa;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let certificate = params.self_signed(&key_pair)?;
        Ok(HandoverIdentity {
            certificate_der: certificate.der().to_vec(),
            private_key: PrivatePkcs8KeyDer::from(key_pair.serialize_der()).into(),
        })
    }

    /// The identity from the PEM bundle [`HandoverIdentity::to_pem`] wrote.
    pub fn from_pem(bundle: &str) -> Result<Self, IdentityError> {
        let certificate = CertificateDer::from_pem_slice(bundle.as_bytes())
            .map_err(|error| IdentityError::Malformed(format!("certificate: {error}")))?;
        let private_key = PrivateKeyDer::from_pem_slice(bundle.as_bytes())
            .map_err(|error| IdentityError::Malformed(format!("private key: {error}")))?;
        Ok(HandoverIdentity {
            certificate_der: certificate.to_vec(),
            private_key,
        })
    }

    /// The certificate and the key as one PEM bundle. A key of a kind this
    /// crate cannot write is an error, not a bundle without a key.
    pub fn to_pem(&self) -> Result<String, IdentityError> {
        let certificate = pem_block("CERTIFICATE", &self.certificate_der);
        let key = match &self.private_key {
            PrivateKeyDer::Pkcs8(key) => pem_block("PRIVATE KEY", key.secret_pkcs8_der()),
            PrivateKeyDer::Sec1(key) => pem_block("EC PRIVATE KEY", key.secret_sec1_der()),
            PrivateKeyDer::Pkcs1(key) => pem_block("RSA PRIVATE KEY", key.secret_pkcs1_der()),
            _ => return Err(IdentityError::UnsupportedKey),
        };
        Ok(certificate + &key)
    }

    /// The host's entry point: the stored identity, or a fresh one minted
    /// with `common_name` and stored ([`HandoverIdentity::store`]). A stored
    /// secret that does not parse is an error, not a reason to mint (which
    /// would re-pair every phone).
    ///
    /// The guard against a silent re-pair, on every platform: the
    /// identity's fingerprint is recorded in `record` from the first load or
    /// mint on, and the database (`store`) lists the paired phones. The
    /// load is [`IdentityError::Unavailable`], and mints nothing, when the
    /// secret store cannot be read; when it holds no identity while a
    /// fingerprint is recorded or a phone is paired; and when the
    /// identity's fingerprint is not the recorded one. The paired phones are the
    /// anchor a rollback keeps: an identity found without a recorded
    /// fingerprint has it recorded again. Only with no phone paired, no
    /// fingerprint and no identity is one minted.
    ///
    /// An unfinished Pair again ([`HandoverIdentity::mint_anew`] stopped or
    /// crashed after it noted the next fingerprint) is decided here, so
    /// every read applies the same rule (the launch, Try again, the import's
    /// gate opening and the reread after an unlock):
    ///
    /// | The secret store holds | Case | The load |
    /// |------------------------|------|----------|
    /// | the identity whose fingerprint is the next one | finished | removes the paired phones, records the fingerprint (which drops the note) and answers the identity; no mint, the admission ledger untouched |
    /// | the identity whose fingerprint is the recorded one, or no identity | abandoned | drops the note, then reads as it does without one (the identity, [`Unavailability::Missing`], or a mint where the guard allows) |
    /// | another identity | unknown | keeps the note and refuses it as [`Unavailability::Replaced`]; writes nothing |
    /// | cannot be read | undecided | keeps the note and refuses as [`Unavailability::Unreadable`]; writes nothing, the next read decides |
    ///
    /// A note that cannot be dropped in the abandoned case is logged and
    /// left: the next read finds the same case.
    pub async fn load_or_create(
        secrets: &dyn SecretStore,
        record: &dyn FingerprintRecord,
        store: &Store,
        common_name: &str,
        now: DateTime<Utc>,
    ) -> Result<Self, IdentityError> {
        let recorded = record.recorded().map_err(IdentityError::Record)?;
        let next = record.next().map_err(IdentityError::Record)?;
        let stored = secrets
            .secret(&Self::secret_key())
            .await
            .map_err(Unavailability::Unreadable)?;
        let Some(stored) = stored else {
            if next.is_some() {
                drop_the_note(record);
            }
            if recorded.is_some() || !store.paired_devices()?.is_empty() {
                return Err(Unavailability::Missing.into());
            }
            let minted = Self::mint(common_name, now)?;
            minted.store(secrets, record).await?;
            return Ok(minted);
        };
        let identity = Self::from_pem(&stored)?;
        let fingerprint = hex(&identity.fingerprint());
        if let Some(next) = next {
            if next == fingerprint {
                remove_paired_phones(store)?;
                identity.record_fingerprint(record)?;
                return Ok(identity);
            }
            if recorded.as_ref() != Some(&fingerprint) {
                return Err(Unavailability::Replaced.into());
            }
            drop_the_note(record);
        }
        match recorded {
            Some(recorded) if recorded != fingerprint => Err(Unavailability::Replaced.into()),
            Some(_) => Ok(identity),
            None => {
                identity.record_fingerprint(record)?;
                Ok(identity)
            }
        }
    }

    /// Pair again's identity step, the user's choice when the identity the
    /// paired phones know cannot be read or is gone (stable plan S6): a new
    /// identity is minted with `common_name`, its fingerprint noted as the
    /// next one in `record`, the identity written to `secrets`, every
    /// paired phone removed from `store`, then the fingerprint recorded,
    /// which drops the note in the same write. Every phone must pair again.
    /// The one mint over an identity that may exist. A step that fails
    /// stops the rest, and the order keeps what it leaves safe: a note that
    /// cannot be written stops before the write; a write that `secrets`
    /// refuses changes nothing (the note is dropped again, and the phones,
    /// the fingerprint and the old identity stay); a stop or a crash after
    /// the write, before or after the phones go, is finished by the next
    /// load ([`HandoverIdentity::load_or_create`]), whether or not a
    /// fingerprint was recorded before.
    pub async fn mint_anew(
        secrets: &dyn SecretStore,
        record: &dyn FingerprintRecord,
        store: &Store,
        common_name: &str,
        now: DateTime<Utc>,
    ) -> Result<Self, IdentityError> {
        let minted = Self::mint(common_name, now)?;
        record
            .note_next(Some(&hex(&minted.fingerprint())))
            .map_err(IdentityError::Record)?;
        if let Err(error) = minted.write_secret(secrets).await {
            drop_the_note(record);
            return Err(error);
        }
        remove_paired_phones(store)?;
        minted.record_fingerprint(record)?;
        Ok(minted)
    }

    /// Stores the identity in `secrets` and records its fingerprint in
    /// `record`, over whatever either held: the mint and an identity
    /// brought over from elsewhere; [`HandoverIdentity::mint_anew`] does
    /// the same two writes with the phones removed between them. The
    /// secret is written first, so a write that fails records nothing; a
    /// record that fails after it leaves the next load to record it again,
    /// or to refuse it as [`Unavailability::Replaced`] where another
    /// fingerprint is recorded; never a mint. Like every record, it ends
    /// an unfinished Pair again.
    pub async fn store(
        &self,
        secrets: &dyn SecretStore,
        record: &dyn FingerprintRecord,
    ) -> Result<(), IdentityError> {
        self.write_secret(secrets).await?;
        self.record_fingerprint(record)
    }

    /// Writes the identity to `secrets` under [`HandoverIdentity::SECRET_KEY`],
    /// over what it held: every write of the secret goes through here.
    async fn write_secret(&self, secrets: &dyn SecretStore) -> Result<(), IdentityError> {
        secrets
            .set_secret(&Self::secret_key(), Some(&self.to_pem()?))
            .await
            .map_err(IdentityError::Secrets)
    }

    fn record_fingerprint(&self, record: &dyn FingerprintRecord) -> Result<(), IdentityError> {
        record
            .record(&hex(&self.fingerprint()))
            .map_err(IdentityError::Record)
    }

    /// The leaf certificate's DER bytes.
    #[must_use]
    pub fn certificate_der(&self) -> &[u8] {
        &self.certificate_der
    }

    /// SHA-256 of the leaf DER, the value the phone pins.
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 32] {
        Self::fingerprint_of_der(&self.certificate_der)
    }

    /// The stable id of this computer, derived from the certificate: a new
    /// identity is a new computer to every phone.
    #[must_use]
    pub fn mac_id(&self) -> Uuid {
        Self::mac_id_for_fingerprint(&self.fingerprint())
    }

    /// SHA-256 of a certificate's DER, the same bytes the phone hashes
    /// (`PinnedTrustEvaluator.fingerprint(of:)`).
    #[must_use]
    pub fn fingerprint_of_der(der: &[u8]) -> [u8; 32] {
        Sha256::digest(der).into()
    }

    /// The computer id in the Bonjour TXT record, the QR payload and
    /// `/v1/hello`: a UUID from the first 16 bytes of
    /// SHA-256(`"steno-mac-id"` || fingerprint) with version 4 and variant 1
    /// bits set.
    #[must_use]
    pub fn mac_id_for_fingerprint(fingerprint: &[u8]) -> Uuid {
        let mut hasher = Sha256::new();
        hasher.update(b"steno-mac-id");
        hasher.update(fingerprint);
        let digest = hasher.finalize();
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        bytes[6] = (bytes[6] & 0x0F) | 0x40;
        bytes[8] = (bytes[8] & 0x3F) | 0x80;
        Uuid::from_bytes(bytes)
    }

    /// A rustls server configuration that terminates TLS 1.3 only with this
    /// identity and asks the client for nothing.
    pub fn server_config(&self) -> Result<Arc<ServerConfig>, IdentityError> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(self.certificate_der.clone())],
                self.private_key.clone_key(),
            )?;
        Ok(Arc::new(config))
    }

    /// A DNS-safe label for the SAN: the phone never checks the name, but a
    /// certificate without one trips some tooling.
    #[must_use]
    pub fn san_label(common_name: &str) -> String {
        let label: String = common_name
            .to_lowercase()
            .chars()
            .map(|character| {
                if character.is_ascii_lowercase() || character.is_ascii_digit() {
                    character
                } else {
                    '-'
                }
            })
            .collect();
        let trimmed = label.trim_matches('-');
        let label = if trimmed.is_empty() {
            "steno"
        } else {
            &trimmed[..trimmed.len().min(63)]
        };
        format!("{label}.local")
    }
}

fn pem_block(tag: &str, der: &[u8]) -> String {
    use base64::Engine as _;
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

/// Removes every paired phone from `store`: Pair again's step, and the
/// load that finishes one.
fn remove_paired_phones(store: &Store) -> Result<(), IdentityError> {
    for device in store.paired_devices()? {
        store.delete_paired_device(device.id)?;
    }
    Ok(())
}

/// Drops the next fingerprint where the Pair again it stood for changed
/// nothing; one that cannot be dropped is logged and left, as the next
/// load finds it abandoned again.
fn drop_the_note(record: &dyn FingerprintRecord) {
    if let Err(error) = record.note_next(None) {
        tracing::warn!(%error, "the note of an unfinished Pair again was not dropped");
    }
}

/// Where the identity's fingerprint (SHA-256 of the leaf DER, lowercase
/// hex) is recorded, and, while a Pair again is unfinished, the next one:
/// outside the secret store, which may be the thing that lost the
/// identity, and outside the settings, which the Swift app's save
/// rewrites. Not a secret. No Swift counterpart.
pub trait FingerprintRecord: Send + Sync {
    /// The recorded fingerprint, `None` when nothing is recorded.
    fn recorded(&self) -> Result<Option<String>, BoxError>;
    /// Records `fingerprint`, replacing what was recorded, and drops the
    /// next fingerprint in the same write.
    fn record(&self, fingerprint: &str) -> Result<(), BoxError>;
    /// The next fingerprint: that of the identity an unfinished Pair again
    /// is writing ([`HandoverIdentity::mint_anew`]); `None` when none is.
    fn next(&self) -> Result<Option<String>, BoxError>;
    /// `Some` notes the next fingerprint and keeps the recorded one;
    /// `None` drops it.
    fn note_next(&self, fingerprint: Option<&str>) -> Result<(), BoxError>;
}

/// Why the identity is unavailable rather than minted. No Swift
/// counterpart.
#[derive(Debug, Error)]
pub enum Unavailability {
    /// The secret store's read failed (a locked keyring, a keyring still
    /// asking the user); the error says why.
    #[error("this computer's phone pairing could not be read ({0})")]
    Unreadable(BoxError),
    /// The secret store holds no identity, though a fingerprint is
    /// recorded or a phone is paired. Not minted, as a new identity would
    /// make every phone pair again.
    #[error(
        "the identity this computer's phones paired with is not in the secret store; a new \
         identity would make every phone pair again"
    )]
    Missing,
    /// The secret store's identity is not the one whose fingerprint is
    /// recorded. Not adopted, as the phones would refuse it.
    #[error(
        "the secret store holds another identity than the one this computer's phones paired \
         with; a new identity would make every phone pair again"
    )]
    Replaced,
}

/// Lowercase hex, for logs and tests.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("minting the identity: {0}")]
    Mint(#[from] rcgen::Error),
    #[error("the validity dates do not fit a certificate: {0}")]
    Validity(#[from] time::error::ComponentRange),
    #[error("the TLS configuration rejected the identity: {0}")]
    Tls(#[from] rustls::Error),
    #[error("reading or writing the secret store: {0}")]
    Secrets(steno_core::BoxError),
    #[error("reading the paired phones: {0}")]
    Store(#[from] StoreError),
    #[error("the identity's fingerprint record: {0}")]
    Record(BoxError),
    /// Not minted ([`Unavailability`] says why).
    #[error(transparent)]
    Unavailable(#[from] Unavailability),
    #[error("the stored identity is malformed: {0}")]
    Malformed(String),
    #[error("the private key is not PKCS#8, SEC1 or PKCS#1, so it cannot be written")]
    UnsupportedKey,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn san_label_is_dns_safe() {
        assert_eq!(
            HandoverIdentity::san_label("Steno on Nicolai's MacBook Pro"),
            "steno-on-nicolai-s-macbook-pro.local"
        );
        assert_eq!(HandoverIdentity::san_label("---"), "steno.local");
    }

    #[test]
    fn pem_round_trips() {
        let minted = HandoverIdentity::mint("A", Utc::now()).unwrap();
        let bundle = minted.to_pem().unwrap();
        assert!(bundle.starts_with("-----BEGIN CERTIFICATE-----\n"));
        assert!(bundle.contains("-----BEGIN PRIVATE KEY-----\n"));
        let loaded = HandoverIdentity::from_pem(&bundle).unwrap();
        assert_eq!(loaded.certificate_der(), minted.certificate_der());
        assert_eq!(loaded.fingerprint(), minted.fingerprint());
        assert_eq!(loaded.private_key, minted.private_key);
        assert!(HandoverIdentity::from_pem("nothing").is_err());
    }
}
