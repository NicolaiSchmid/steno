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
//! in a [`FingerprintRecord`]), and adopts one found without a record only
//! when every paired phone can know it
//! ([`HandoverIdentity::known_by_every_phone`]). Only the user's Pair again
//! mints over one ([`HandoverIdentity::mint_anew`]); one that stopped is
//! finished by the next load.

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
    /// fingerprint (an upgrade from a build without the record, or a lost
    /// record) has it recorded again only when every paired phone was paired
    /// at or after the identity's certificate starts
    /// ([`Self::known_by_every_phone`]), as a phone paired before it cannot
    /// know it. Otherwise the load is [`Unavailability::Replaced`], or
    /// [`Unavailability::PartlyReplaced`] when only some phones predate it,
    /// and records nothing; a start that cannot be read is
    /// [`IdentityError::Malformed`], paired phones that cannot be read
    /// [`IdentityError::Store`]. Only with no phone paired, no fingerprint
    /// and no identity is one minted.
    ///
    /// An unfinished Pair again ([`HandoverIdentity::mint_anew`] stopped or
    /// crashed after it noted the next fingerprint, so its note,
    /// [`FingerprintRecord::next`], is still in `record`) is decided here,
    /// so every read applies the same rule: the launch, a plain handover's
    /// Try again, the import's gate opening and the reread after an unlock.
    /// Behind the import's waiting gate, Try again runs the import instead;
    /// its store records the Swift identity, which ends the note like every
    /// record.
    ///
    /// | The secret store holds | Case | The load |
    /// |------------------------|------|----------|
    /// | the identity whose fingerprint is the next one | finished | removes the paired phones, records the fingerprint (which drops the note) and answers the identity; no mint, the admission ledger untouched |
    /// | the identity whose fingerprint is the recorded one, or no identity | abandoned | drops the note, then reads as it does without one (the identity, [`Unavailability::Missing`], or a mint where the guard allows) |
    /// | any other identity, which is every identity but the next one while nothing is recorded | unknown | keeps the note and refuses it as [`Unavailability::Replaced`]; writes nothing |
    /// | cannot be read | undecided | keeps the note and refuses as [`Unavailability::Unreadable`]; writes nothing |
    ///
    /// Where each stop of a Pair again lands:
    /// - A stop before the write, or a write the secret store refused, is
    ///   abandoned. It is unknown instead where nothing is recorded and an
    ///   identity is stored, as after Pair again set an unreadable record
    ///   aside.
    /// - A write reported as refused that landed after all, and every stop
    ///   after the write, is finished.
    ///
    /// Undecided is the read's error, abandoned with no identity is the
    /// half before the match, and without a note the guard above applies.
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
        match (next, recorded) {
            // Finished.
            (Some(next), _) if next == fingerprint => {
                remove_paired_phones(store)?;
                identity.record_fingerprint(record)?;
            }
            // The recorded identity: abandoned when noted.
            (next, Some(recorded)) if recorded == fingerprint => {
                if next.is_some() {
                    drop_the_note(record);
                }
            }
            // Unknown when noted, replaced when not.
            (Some(_), None) | (_, Some(_)) => return Err(Unavailability::Replaced.into()),
            // Found without a record: recorded again where every phone
            // knows it.
            (None, None) => {
                let paired: Vec<_> = store
                    .paired_devices()?
                    .into_iter()
                    .map(|device| device.paired_at)
                    .collect();
                Self::known_by_every_phone(&[identity.not_valid_before()?], &paired)?;
                identity.record_fingerprint(record)?;
            }
        }
        Ok(identity)
    }

    /// The pairing-time rule for identities found with nothing recorded:
    /// of the identities whose certificates start at `starts`
    /// ([`Self::not_valid_before`]), the index of the one every phone
    /// paired at `paired` knows. A phone knows the newest identity whose
    /// start is at or before its pairing, as a phone paired before an
    /// identity's start cannot know it; one paired at that instant counts
    /// as paired after it. Both times come from this computer's clock, and
    /// a minted certificate starts a minute before its mint
    /// ([`Self::mint`]), the margin for a pairing right after it. With no
    /// phone paired, the newest identity.
    ///
    /// [`Unavailability::Replaced`] when no phone knows any of them (and
    /// with no identity at all), [`Unavailability::PartlyReplaced`] when
    /// some phones know one and the others another one or none. Two
    /// identities that start in the same second are no newest one: a phone
    /// whose newest start they share knows neither, so nothing is chosen
    /// silently. Swift: `HandoverIdentityGuard.adoptable` in
    /// `Sources/StenoHandover/Identity/IdentityGuard.swift`.
    ///
    /// ```
    /// use chrono::{TimeZone as _, Utc};
    /// use steno_handover::{HandoverIdentity, Unavailability};
    ///
    /// let at = |seconds| Utc.timestamp_opt(seconds, 0).unwrap();
    /// let (old, new) = (at(1_000), at(2_000));
    /// // Both phones paired between the two starts know the old identity.
    /// assert_eq!(
    ///     HandoverIdentity::known_by_every_phone(&[new, old], &[at(1_500), at(1_999)]).unwrap(),
    ///     1
    /// );
    /// // One between them and one after the new start: no one identity.
    /// assert!(matches!(
    ///     HandoverIdentity::known_by_every_phone(&[old, new], &[at(1_500), at(2_000)]),
    ///     Err(Unavailability::PartlyReplaced)
    /// ));
    /// ```
    pub fn known_by_every_phone(
        starts: &[DateTime<Utc>],
        paired: &[DateTime<Utc>],
    ) -> Result<usize, Unavailability> {
        // The index of the one newest start at or before `instant`.
        let newest = |instant: DateTime<Utc>| {
            let start = starts.iter().filter(|start| **start <= instant).max()?;
            let mut at = (0..starts.len()).filter(|index| starts[*index] == *start);
            match (at.next(), at.next()) {
                (Some(index), None) => Some(index),
                _ => None,
            }
        };
        let known: Vec<Option<usize>> = if paired.is_empty() {
            vec![newest(DateTime::<Utc>::MAX_UTC)]
        } else {
            paired.iter().map(|instant| newest(*instant)).collect()
        };
        match known[0] {
            Some(only) if known.iter().all(|index| *index == Some(only)) => Ok(only),
            _ if known.iter().any(Option::is_some) => Err(Unavailability::PartlyReplaced),
            _ => Err(Unavailability::Replaced),
        }
    }

    /// When the certificate starts to be valid (`notValidBefore`), on this
    /// computer's clock: a minute before its mint ([`Self::mint`]; the
    /// Swift app's mint back-dates it the same), so a phone paired before
    /// it cannot know this identity. Swift:
    /// `HandoverIdentity.notValidBefore()`.
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
/// rewrites. Not a secret. No Swift counterpart.
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
    /// recorded, or, with nothing recorded, every paired phone was paired
    /// before it ([`HandoverIdentity::known_by_every_phone`]). Not adopted,
    /// as the phones would refuse it.
    #[error(
        "the secret store holds another identity than the one this computer's phones paired \
         with; a new identity would make every phone pair again"
    )]
    Replaced,
    /// With nothing recorded, some paired phones know the identity found
    /// and the others another one or none
    /// ([`HandoverIdentity::known_by_every_phone`]). Not adopted, as those
    /// phones could not connect; a [`Unavailability::Replaced`] in all but
    /// its words. Swift: `IdentityUnavailability.partlyReplaced`.
    #[error(
        "some of this computer's phones were paired before the identity the secret store holds; \
         a new identity would make every phone pair again"
    )]
    PartlyReplaced,
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

    /// The pairing-time rule over several identities: each phone knows
    /// the newest identity that starts at or before its pairing.
    #[test]
    fn every_phone_knows_the_newest_identity_started_before_its_pairing() {
        use chrono::TimeZone as _;
        let at = |seconds| Utc.timestamp_opt(seconds, 0).unwrap();
        let (a, b, c) = (at(1_000), at(2_000), at(3_000));
        let known = |starts: &[DateTime<Utc>], paired: &[i64]| {
            let paired: Vec<_> = paired.iter().map(|seconds| at(*seconds)).collect();
            HandoverIdentity::known_by_every_phone(starts, &paired)
        };
        let replaced = |result| matches!(result, Err(Unavailability::Replaced));
        let partly = |result| matches!(result, Err(Unavailability::PartlyReplaced));
        for starts in [[a, b, c], [c, a, b], [b, c, a]] {
            let index = |start| starts.iter().position(|s| *s == start).unwrap();
            // No phone: the newest.
            assert_eq!(known(&starts, &[]).unwrap(), index(c));
            // Every phone between A and B, at A's start included: A.
            assert_eq!(known(&starts, &[1_000, 1_999]).unwrap(), index(a));
            // Every phone between B and C: B, not the newest.
            assert_eq!(known(&starts, &[2_000, 2_500]).unwrap(), index(b));
            assert_eq!(known(&starts, &[3_000, 9_000]).unwrap(), index(c));
            // A phone before every start.
            assert!(replaced(known(&starts, &[999])));
            assert!(replaced(known(&starts, &[0, 999])));
            // One between A and B, one after B: no one identity.
            assert!(partly(known(&starts, &[1_500, 2_500])));
            assert!(partly(known(&starts, &[999, 1_500])));
            assert!(partly(known(&starts, &[999, 3_500])));
        }
        // No identity at all: no phone knows one.
        assert!(replaced(known(&[], &[])));
        assert!(replaced(known(&[], &[1_000])));
        // Two that start in the same second are no newest one.
        assert!(replaced(known(&[a, a], &[])));
        assert!(replaced(known(&[a, a], &[1_500])));
        assert_eq!(known(&[a, a, b], &[2_500]).unwrap(), 2);
        assert!(partly(known(&[a, a, b], &[1_500, 2_500])));
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
