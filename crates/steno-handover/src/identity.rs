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
//! Losing the identity means re-pairing every phone, on both sides.

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
use steno_core::{SecretKey, SecretStore};
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
    /// with `common_name` and stored. A stored secret that does not parse is
    /// an error, not a reason to mint (which would re-pair every phone).
    pub async fn load_or_create(
        secrets: &dyn SecretStore,
        common_name: &str,
        now: DateTime<Utc>,
    ) -> Result<Self, IdentityError> {
        let key = Self::secret_key();
        if let Some(stored) = secrets.secret(&key).await.map_err(IdentityError::Secrets)? {
            return Self::from_pem(&stored);
        }
        let minted = Self::mint(common_name, now)?;
        secrets
            .set_secret(&key, Some(&minted.to_pem()?))
            .await
            .map_err(IdentityError::Secrets)?;
        Ok(minted)
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
    #[error("the secret store: {0}")]
    Secrets(steno_core::BoxError),
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
