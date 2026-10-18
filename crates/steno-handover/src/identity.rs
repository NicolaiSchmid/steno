//! The TLS identity the listener presents: a self-signed P-256 leaf
//! certificate and its key, kept as one PEM bundle secret. Losing it means
//! re-pairing every phone, so the loader never mints one while phones are
//! paired or after one was in use; [`decision::decide`] holds the rule. Only
//! the user's Pair again mints over an identity.

pub mod decision;

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

use decision::{Caller, Found, Identities, Observations, Outcome, Phones, Reason, Record, Write};

/// The leaf certificate (for the pinned fingerprint and the computer id)
/// and the private key rustls terminates TLS with.
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

    /// Mints a fresh identity: P-256, self-signed, ten years, `CN=<common_name>`,
    /// not a CA, `digitalSignature` only, server authentication, one DNS SAN.
    /// Backdated a minute to absorb clock skew.
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

    /// The certificate and the key as one PEM bundle; a key this crate cannot
    /// write is an error.
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

    /// The host's entry point: the stored identity, or a fresh one minted and
    /// stored. Reads `record`, the secret store and the paired phones, decides
    /// at `now` ([`decision::decide`]), applies the decision's writes in order,
    /// then serves, mints or waits. A wait is [`IdentityError::Unavailable`] or
    /// the error of the read that failed. Every write is idempotent, so after a
    /// failed write the next load decides the same and finishes the rest,
    /// including an unfinished Pair again.
    pub async fn load_or_create(
        secrets: &dyn SecretStore,
        record: &dyn FingerprintRecord,
        store: &Store,
        common_name: &str,
        now: DateTime<Utc>,
    ) -> Result<Self, IdentityError> {
        let (record_seen, record_error) = observe_record(record);
        let mut failed = ReadErrors {
            record: record_error,
            ..ReadErrors::default()
        };
        let (identities, identity) = match secrets.secret(&Self::secret_key()).await {
            Err(error) => {
                failed.secret = Some(error);
                (Identities::Unreadable, None)
            }
            Ok(None) => (Identities::Found(Vec::new()), None),
            Ok(Some(pem)) => match Self::from_pem(&pem) {
                Ok(identity) => {
                    let start = identity
                        .not_valid_before()
                        .map_err(|error| failed.malformed = Some(error))
                        .ok();
                    let found = Found {
                        fingerprint: hex(&identity.fingerprint()),
                        start,
                    };
                    (Identities::Found(vec![found]), Some(identity))
                }
                Err(error) => {
                    failed.malformed = Some(error);
                    (Identities::Malformed, None)
                }
            },
        };
        let phones = match store.paired_devices() {
            Ok(devices) => Phones::Paired(devices.iter().map(|device| device.paired_at).collect()),
            Err(error) => {
                failed.phones = Some(error);
                Phones::Unreadable
            }
        };
        let decided = decision::decide(&Observations {
            caller: Caller::Load,
            identities,
            record: record_seen,
            phones,
            now,
        });
        for write in &decided.writes {
            match write {
                Write::DropNote => drop_the_note(record),
                Write::RemovePhones => remove_paired_phones(store)?,
                Write::Record(fingerprint) => {
                    record.record(fingerprint).map_err(IdentityError::Record)?;
                }
            }
        }
        match decided.outcome {
            Outcome::Serve(_) => Ok(identity.expect("the load serves only the identity it found")),
            Outcome::Mint => {
                let minted = Self::mint(common_name, now)?;
                minted.store(secrets, record).await?;
                Ok(minted)
            }
            Outcome::Wait(reason) => Err(failed.into_error(reason)),
        }
    }

    /// When the certificate starts to be valid, on this computer's clock: a
    /// minute before its mint, so a phone paired before it cannot know this
    /// identity.
    pub fn not_valid_before(&self) -> Result<DateTime<Utc>, IdentityError> {
        Self::not_valid_before_of_der(&self.certificate_der)
    }

    /// [`Self::not_valid_before`] of a certificate's DER;
    /// [`IdentityError::Malformed`] when it does not parse.
    pub fn not_valid_before_of_der(der: &[u8]) -> Result<DateTime<Utc>, IdentityError> {
        let (_, certificate) = x509_parser::parse_x509_certificate(der)
            .map_err(|error| IdentityError::Malformed(format!("certificate: {error}")))?;
        let seconds = certificate.validity().not_before.timestamp();
        DateTime::from_timestamp(seconds, 0).ok_or_else(|| {
            IdentityError::Malformed(format!("the certificate's start ({seconds} s) is no date"))
        })
    }

    /// Pair again's identity step, when the identity the phones know is
    /// unreadable or gone. Mints a new identity, then:
    /// 1. notes its fingerprint as the next one in `record`;
    /// 2. writes it to `secrets`;
    /// 3. removes every paired phone from `store`;
    /// 4. records its fingerprint, dropping the note.
    ///
    /// A step that fails stops the rest and leaves the note; the next load
    /// abandons a write that did not land and finishes one that did.
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
            .map_err(IdentityError::Note)?;
        minted.write_secret(secrets).await?;
        remove_paired_phones(store)?;
        minted.record_fingerprint(record)?;
        Ok(minted)
    }

    /// Stores the identity in `secrets`, then records its fingerprint, over
    /// whatever either held. A failed record leaves the next load to record it
    /// again, or to refuse it as [`Unavailability::Replaced`]; never a mint.
    pub async fn store(
        &self,
        secrets: &dyn SecretStore,
        record: &dyn FingerprintRecord,
    ) -> Result<(), IdentityError> {
        self.write_secret(secrets).await?;
        self.record_fingerprint(record)
    }

    /// Writes the identity to `secrets`; every write of the secret goes here.
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

    /// SHA-256 of a certificate's DER, the same bytes the phone hashes.
    #[must_use]
    pub fn fingerprint_of_der(der: &[u8]) -> [u8; 32] {
        Sha256::digest(der).into()
    }

    /// The computer id in the Bonjour TXT record, the QR payload and
    /// `/v1/hello`: a UUID from the first 16 bytes of
    /// SHA-256(`"steno-mac-id"` || fingerprint), version 4 and variant 1 bits set.
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

    /// A rustls server configuration: TLS 1.3 only, this identity, no client
    /// authentication.
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

    /// A DNS-safe SAN label: the phone never checks it, but some tooling needs
    /// one.
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

/// Removes every paired phone from `store`.
fn remove_paired_phones(store: &Store) -> Result<(), IdentityError> {
    for device in store.paired_devices()? {
        store.delete_paired_device(device.id)?;
    }
    Ok(())
}

/// Drops the note of a Pair again that changed nothing; a failure is logged,
/// and the next load tries again.
fn drop_the_note(record: &dyn FingerprintRecord) {
    if let Err(error) = record.note_next(None) {
        tracing::warn!(%error, "the note of an unfinished Pair again was not dropped");
    }
}

/// Where the identity's fingerprint (lowercase hex SHA-256 of the leaf DER)
/// is recorded, plus the next one while a Pair again is unfinished. Outside
/// the secret store, which may be what lost the identity. Not a secret.
pub trait FingerprintRecord: Send + Sync {
    /// The recorded fingerprint, `None` when nothing is recorded.
    fn recorded(&self) -> Result<Option<String>, BoxError>;
    /// Records `fingerprint` and drops the note in one write; a record that
    /// does not parse is kept aside first.
    fn record(&self, fingerprint: &str) -> Result<(), BoxError>;
    /// The next fingerprint noted by an unfinished Pair again, if any.
    fn next(&self) -> Result<Option<String>, BoxError>;
    /// Pair again's note. `Some` notes the next fingerprint (over an unparsable
    /// record, a new one is started and the old one kept aside); `None` drops
    /// the note.
    fn note_next(&self, fingerprint: Option<&str>) -> Result<(), BoxError>;
}

/// Why the identity is unavailable rather than minted.
#[derive(Debug, Error)]
pub enum Unavailability {
    /// The secret store's read failed (locked or prompting).
    #[error("this computer's phone pairing could not be read ({0})")]
    Unreadable(BoxError),
    /// No identity stored, though a fingerprint is recorded or a phone is
    /// paired; a new one would make every phone pair again.
    #[error(
        "the identity this computer's phones paired with is not in the secret store; a new \
         identity would make every phone pair again"
    )]
    Missing,
    /// The stored identity is not the recorded one, or no paired phone knows
    /// it; the phones would refuse it.
    #[error(
        "the secret store holds another identity than the one this computer's phones paired \
         with; a new identity would make every phone pair again"
    )]
    Replaced,
    /// Nothing recorded, and the paired phones do not all know one identity.
    #[error(
        "some of this computer's phones were paired before the identity the secret store holds; \
         a new identity would make every phone pair again"
    )]
    PartlyReplaced,
    /// Nothing recorded, and a pairing time or certificate start lies after
    /// now by more than [`decision::CLOCK_SLACK`], so which identity the phones
    /// know cannot be told. Try again decides once the clock has passed both.
    #[error(
        "this computer's clock reads earlier than a phone's pairing or the identity's start, so \
         which identity the phones know cannot be told"
    )]
    ClockBehind,
}

/// What `record` holds, with the error of a failed read: an
/// [`std::io::Error`] is [`Record::NotOpened`], anything else
/// [`Record::DoesNotParse`].
pub fn observe_record(record: &dyn FingerprintRecord) -> (Record, Option<BoxError>) {
    let read = record
        .recorded()
        .and_then(|current| Ok((current, record.next()?)));
    match read {
        Ok((current, pending)) => (Record::Read { current, pending }, None),
        Err(error) if error.is::<std::io::Error>() => (Record::NotOpened, Some(error)),
        Err(error) => (Record::DoesNotParse, Some(error)),
    }
}

/// The errors of the load's failed reads.
#[derive(Default)]
struct ReadErrors {
    record: Option<BoxError>,
    secret: Option<BoxError>,
    malformed: Option<IdentityError>,
    phones: Option<StoreError>,
}

impl ReadErrors {
    /// The load's error for waiting with `reason`.
    fn into_error(self, reason: Reason) -> IdentityError {
        let failed = "the decision waits on a read only where that read failed";
        match reason {
            Reason::RecordNotOpened | Reason::RecordDoesNotParse => {
                IdentityError::Record(self.record.expect(failed))
            }
            Reason::SecretUnreadable => {
                Unavailability::Unreadable(self.secret.expect(failed)).into()
            }
            Reason::Malformed => self.malformed.expect(failed),
            Reason::PhonesUnreadable => IdentityError::Store(self.phones.expect(failed)),
            Reason::Missing => Unavailability::Missing.into(),
            Reason::Replaced => Unavailability::Replaced.into(),
            Reason::PartlyReplaced => Unavailability::PartlyReplaced.into(),
            Reason::ClockBehind => Unavailability::ClockBehind.into(),
        }
    }
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
    /// Pair again could not note the next fingerprint, so it wrote nothing.
    #[error("noting the next fingerprint in the identity's record: {0}")]
    Note(BoxError),
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
