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
//! loader never mints one while phones are paired or after one was in use,
//! over the identity's fingerprint in a [`FingerprintRecord`]. Which
//! identity is served, minted or waited for is one pure decision
//! ([`decision::decide`]), whose truth table states the rule. Only the
//! user's Pair again mints over an identity ([`HandoverIdentity::mint_anew`]);
//! one that stopped is finished by the next load.

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
    /// with `common_name` and stored ([`HandoverIdentity::store`]). It reads
    /// `record` ([`observe_record`]), the secret store and the phones paired
    /// in `store`, decides at `now` ([`decision::decide`], whose truth table
    /// gives every case), applies the decision's writes in order, then
    /// serves, mints or waits. A wait is [`IdentityError::Unavailable`] with
    /// the [`Unavailability`], or the error of the read that failed:
    /// [`IdentityError::Record`], [`IdentityError::Malformed`] for a secret
    /// or a certificate start that does not parse, [`IdentityError::Store`]
    /// for phones that cannot be read. A write that fails stops the rest;
    /// as every write is idempotent, the next read decides the same and
    /// writes what is left. A note that cannot be dropped is logged and
    /// left.
    ///
    /// Every read decides an unfinished Pair again
    /// ([`HandoverIdentity::mint_anew`] stopped after it noted the next
    /// fingerprint) the same way: the launch, a plain handover's Try again,
    /// the import's gate opening and the reread after an unlock. Behind the
    /// import's waiting gate, Try again runs the import instead; its store
    /// records the Swift identity, which ends the note like every record.
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

    /// When the certificate starts to be valid (`notValidBefore`), on this
    /// computer's clock: a minute before its mint ([`Self::mint`]; the
    /// Swift app's mint back-dates it the same), so a phone paired before
    /// it cannot know this identity.
    pub fn not_valid_before(&self) -> Result<DateTime<Utc>, IdentityError> {
        Self::not_valid_before_of_der(&self.certificate_der)
    }

    /// [`Self::not_valid_before`] of a certificate's DER: a certificate
    /// that does not parse, or a start this computer's clock cannot hold,
    /// is [`IdentityError::Malformed`].
    pub fn not_valid_before_of_der(der: &[u8]) -> Result<DateTime<Utc>, IdentityError> {
        let (_, certificate) = x509_parser::parse_x509_certificate(der)
            .map_err(|error| IdentityError::Malformed(format!("certificate: {error}")))?;
        let seconds = certificate.validity().not_before.timestamp();
        DateTime::from_timestamp(seconds, 0).ok_or_else(|| {
            IdentityError::Malformed(format!("the certificate's start ({seconds} s) is no date"))
        })
    }

    /// Pair again's identity step, the user's choice when the identity the
    /// paired phones know cannot be read or is gone (stable plan S6): a new
    /// identity is minted with `common_name`. Then:
    /// 1. Its fingerprint is noted as the next one in `record`. Over a
    ///    record that does not parse, the record is set aside first
    ///    ([`FingerprintRecord::note_next`]).
    /// 2. The identity is written to `secrets`.
    /// 3. Every paired phone is removed from `store`.
    /// 4. Its fingerprint is recorded, which drops the note in the same
    ///    write.
    ///
    /// Every phone must pair again. This is the one mint over an identity
    /// that may exist. A step that fails stops the rest and leaves the note
    /// where it is. A note that cannot be written stops before the write
    /// ([`IdentityError::Note`]). Every later stop, a refused write
    /// included, is decided by the next load
    /// ([`HandoverIdentity::load_or_create`]):
    /// - a write that did not land is abandoned there;
    /// - a write that landed, even one the store reported as refused, and
    ///   every stop after it, is finished;
    /// - this holds whether or not a fingerprint was recorded before.
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

/// Drops the note where the Pair again it stood for changed nothing (the
/// load's abandoned case); one that cannot be dropped is logged and left,
/// as the next load finds it abandoned again.
fn drop_the_note(record: &dyn FingerprintRecord) {
    if let Err(error) = record.note_next(None) {
        tracing::warn!(%error, "the note of an unfinished Pair again was not dropped");
    }
}

/// Where the identity's fingerprint (SHA-256 of the leaf DER, lowercase
/// hex) is recorded, and, while a Pair again is unfinished, the next one:
/// outside the secret store, which may be the thing that lost the
/// identity, and outside the settings, which the Swift app's save
/// rewrites. Not a secret.
pub trait FingerprintRecord: Send + Sync {
    /// The recorded fingerprint, `None` when nothing is recorded.
    fn recorded(&self) -> Result<Option<String>, BoxError>;
    /// Records `fingerprint`, replacing what was recorded, and drops the
    /// note in the same write. Over a record that does not parse, it keeps
    /// the old one where the user can find it first, as `note_next(Some)`
    /// does.
    fn record(&self, fingerprint: &str) -> Result<(), BoxError>;
    /// The next fingerprint, kept in the note of an unfinished Pair again:
    /// that of the identity it is writing ([`HandoverIdentity::mint_anew`]);
    /// `None` when there is no note.
    fn next(&self) -> Result<Option<String>, BoxError>;
    /// Pair again's note. `Some` notes the next fingerprint beside the
    /// recorded one. Over a record that does not parse, it starts a new
    /// record and keeps the old one where the user can find it, so nothing
    /// is recorded after it. `None` drops the note and never replaces
    /// anything.
    fn note_next(&self, fingerprint: Option<&str>) -> Result<(), BoxError>;
}

/// Why the identity is unavailable rather than minted
/// ([`decision::decide`] says when).
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
    /// recorded, or, with nothing recorded, no paired phone knows it. Not
    /// adopted, as the phones would refuse it.
    #[error(
        "the secret store holds another identity than the one this computer's phones paired \
         with; a new identity would make every phone pair again"
    )]
    Replaced,
    /// With nothing recorded, the paired phones do not all know one
    /// identity, but some know one. Not adopted, as the others could not
    /// connect; a [`Unavailability::Replaced`] in all but its words.
    #[error(
        "some of this computer's phones were paired before the identity the secret store holds; \
         a new identity would make every phone pair again"
    )]
    PartlyReplaced,
    /// With nothing recorded, a pairing time or a certificate's start is
    /// after now on this computer's clock, by more than
    /// [`decision::CLOCK_SLACK`]: the clock reads earlier than when it was
    /// set, so which identity the phones know cannot be told. Not adopted
    /// and not minted; Try again decides once the clock has passed both.
    #[error(
        "this computer's clock reads earlier than a phone's pairing or the identity's start, so \
         which identity the phones know cannot be told"
    )]
    ClockBehind,
}

/// What `record` holds, as [`decision::decide`] reads it, with the error
/// of a read that failed: one that cannot open the file (an
/// [`std::io::Error`], as [`FingerprintRecord`]'s file reports it) is
/// [`Record::NotOpened`], any other [`Record::DoesNotParse`].
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

/// The errors of the load's reads that failed, for the wait the decision
/// names.
#[derive(Default)]
struct ReadErrors {
    record: Option<BoxError>,
    secret: Option<BoxError>,
    malformed: Option<IdentityError>,
    phones: Option<StoreError>,
}

impl ReadErrors {
    /// The load's error for waiting with `reason`: the unavailability, or
    /// the error of the read the decision waits on.
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
    /// Pair again could not note the next fingerprint, so it stopped
    /// before it wrote anything ([`HandoverIdentity::mint_anew`]).
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
