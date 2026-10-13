//! Mint, fingerprint, computer id, the committed Swift test identity and
//! the secret store round trip: the values the phone pins and shows.

#![allow(
    clippy::assert_is_empty,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::large_futures,
    clippy::too_many_lines
)]

mod common;

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{TimeZone as _, Utc};
use common::MemoryRecord;
use steno_core::{BoundaryResult, PairedDevice, SecretKey, SecretStore, Store};
use steno_handover::identity::hex;
use steno_handover::{FingerprintRecord, HandoverIdentity, IdentityError, Unavailability};
use x509_parser::prelude::*;

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

#[test]
fn mint_yields_a_self_signed_p256_certificate_for_ten_years() {
    let now = Utc.timestamp_opt(1_790_000_000, 0).unwrap();
    let minted = HandoverIdentity::mint("Steno on Test Mac", now).unwrap();
    let (rest, certificate) = X509Certificate::from_der(minted.certificate_der()).unwrap();
    assert_eq!(rest, b"");

    assert_eq!(certificate.subject().to_string(), "CN=Steno on Test Mac");
    assert_eq!(certificate.issuer(), certificate.subject());
    assert_eq!(
        certificate.signature_algorithm.algorithm,
        oid_registry::OID_SIG_ECDSA_WITH_SHA256
    );
    assert_eq!(
        certificate.public_key().algorithm.algorithm,
        oid_registry::OID_KEY_TYPE_EC_PUBLIC_KEY
    );
    let not_before = certificate.validity().not_before.timestamp();
    let not_after = certificate.validity().not_after.timestamp();
    assert!(not_before <= now.timestamp());
    assert_eq!(now.timestamp() - not_before, 60, "a minute of backdating");
    assert_eq!((not_after - not_before) / (365 * 24 * 3600), 10);
    assert!(
        certificate.verify_signature(None).is_ok(),
        "the certificate's own key signed it"
    );
    let constraints = certificate.basic_constraints().unwrap().unwrap();
    assert!(constraints.critical);
    assert!(!constraints.value.ca);
    // RFC 5480 §3: keyEncipherment is not a use an EC key has; a strict
    // validator rejects a critical KeyUsage that claims it.
    let usage = certificate.key_usage().unwrap().unwrap();
    assert!(usage.critical);
    assert!(usage.value.digital_signature());
    assert!(!usage.value.key_encipherment());
    let extended = certificate.extended_key_usage().unwrap().unwrap();
    assert!(extended.value.server_auth);
    let names = certificate.subject_alternative_name().unwrap().unwrap();
    assert_eq!(
        names.value.general_names,
        vec![GeneralName::DNSName("steno-on-test-mac.local")]
    );
}

#[test]
fn fingerprint_is_stable_for_one_der_and_differs_between_mints() {
    let a = HandoverIdentity::mint("A", Utc::now()).unwrap();
    let b = HandoverIdentity::mint("A", Utc::now()).unwrap();
    assert_eq!(a.fingerprint().len(), 32);
    assert_eq!(
        a.fingerprint(),
        HandoverIdentity::fingerprint_of_der(a.certificate_der())
    );
    assert_ne!(a.fingerprint(), b.fingerprint());
    assert_ne!(a.certificate_der(), b.certificate_der());
}

#[test]
fn mac_id_derives_from_the_fingerprint_deterministically() {
    let fingerprint = [0xABu8; 32];
    let first = HandoverIdentity::mac_id_for_fingerprint(&fingerprint);
    let second = HandoverIdentity::mac_id_for_fingerprint(&fingerprint);
    assert_eq!(first, second);
    assert_ne!(
        first,
        HandoverIdentity::mac_id_for_fingerprint(&[0xACu8; 32])
    );
    // Version 4, variant 1, so it never collides with random ids by shape.
    assert_eq!(first.get_version_num(), 4);
    assert_eq!(first.get_variant(), uuid::Variant::RFC4122);
}

/// The committed Swift test identity (`Tests/Fixtures/handover/`): the
/// fingerprint recorded when it was generated, so the hashing here is the
/// hashing the Swift tests and the phone do.
#[test]
fn the_swift_test_identity_hashes_to_its_recorded_fingerprint() {
    let der =
        std::fs::read(repository().join("Tests/Fixtures/handover/test-identity.der")).unwrap();
    assert_eq!(
        hex(&HandoverIdentity::fingerprint_of_der(&der)),
        "76ac0c0b28f976f25589214d5b2c614d983df9de081b06ec1fe9247bf1e40d92"
    );
    let (_, certificate) = X509Certificate::from_der(&der).unwrap();
    assert_eq!(certificate.subject().to_string(), "CN=Steno test identity");
    assert_eq!(
        certificate.public_key().algorithm.algorithm,
        oid_registry::OID_KEY_TYPE_EC_PUBLIC_KEY
    );
}

/// A secret store in memory that refuses reads or writes while
/// `refuse_reads` or `refuse_writes` is set; while `keep_and_refuse` is,
/// it takes a write and then reports it refused, as a reply lost after the
/// save does.
#[derive(Default)]
struct MemorySecrets {
    values: Mutex<Vec<(SecretKey, String)>>,
    reads: Mutex<u32>,
    refuse_reads: AtomicBool,
    refuse_writes: AtomicBool,
    keep_and_refuse: AtomicBool,
}

#[steno_core::async_trait]
impl SecretStore for MemorySecrets {
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
        *self.reads.lock().unwrap() += 1;
        if self.refuse_reads.load(Ordering::SeqCst) {
            return Err("the keyring is locked".into());
        }
        Ok(self
            .values
            .lock()
            .unwrap()
            .iter()
            .find(|(stored, _)| stored == key)
            .map(|(_, value)| value.clone()))
    }

    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        if self.refuse_writes.load(Ordering::SeqCst) {
            return Err("the keyring refused the write".into());
        }
        let mut values = self.values.lock().unwrap();
        values.retain(|(stored, _)| stored != key);
        if let Some(value) = value {
            values.push((key.clone(), value.to_owned()));
        }
        if self.keep_and_refuse.load(Ordering::SeqCst) {
            return Err("the keyring did not answer".into());
        }
        Ok(())
    }
}

/// The three inputs of `load_or_create`, empty.
#[derive(Default)]
struct Places {
    secrets: MemorySecrets,
    record: MemoryRecord,
    store: Option<Store>,
}

impl Places {
    fn new() -> Self {
        Places {
            store: Some(Store::in_memory().unwrap()),
            ..Places::default()
        }
    }

    fn store(&self) -> &Store {
        self.store.as_ref().unwrap()
    }

    async fn load(&self, name: &str) -> Result<HandoverIdentity, IdentityError> {
        self.load_at(name, Utc::now()).await
    }

    /// [`Places::load`] with this computer's clock at `now`.
    async fn load_at(
        &self,
        name: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<HandoverIdentity, IdentityError> {
        HandoverIdentity::load_or_create(&self.secrets, &self.record, self.store(), name, now).await
    }

    fn pair_a_phone(&self) {
        self.pair_phones(1);
    }

    fn pair_phones(&self, count: usize) {
        self.pair_phones_at(&vec![Utc::now(); count]);
    }

    /// Pairs one phone at each of `times`, as the database records it.
    fn pair_phones_at(&self, times: &[chrono::DateTime<Utc>]) {
        for (id, paired_at) in (1u128..).zip(times) {
            let device = PairedDevice {
                id: uuid::Uuid::from_u128(id),
                name: format!("Phone {id}"),
                paired_at: *paired_at,
                last_seen_at: None,
            };
            self.store()
                .save_paired_device(&device, &[id as u8; 32])
                .unwrap();
        }
    }

    /// Keeps the last paired phone whatever deletes it, as a disk that
    /// refuses the write would, until [`Places::let_the_last_phone_go`]:
    /// Pair again stops right after the write with one phone paired, and
    /// after the first phone with two.
    fn keep_the_last_phone(&self) {
        common::execute_batch(
            self.store(),
            "CREATE TRIGGER keep_the_last_phone BEFORE DELETE ON pairedDevice \
             WHEN (SELECT COUNT(*) FROM pairedDevice) = 1 \
             BEGIN SELECT RAISE(ABORT, 'the disk refused the write'); END;",
        );
    }

    /// Lets the phone [`Places::keep_the_last_phone`] kept go again.
    fn let_the_last_phone_go(&self) {
        common::execute_batch(self.store(), "DROP TRIGGER keep_the_last_phone;");
    }

    /// The stored identity, the recorded fingerprint, the next fingerprint
    /// and the paired phones' ids, as they stand.
    fn places(
        &self,
    ) -> (
        Option<String>,
        Option<String>,
        Option<String>,
        Vec<uuid::Uuid>,
    ) {
        (
            self.stored_pem(),
            self.record.get_recorded(),
            self.record.get_next(),
            self.store()
                .paired_devices()
                .unwrap()
                .iter()
                .map(|device| device.id)
                .collect(),
        )
    }

    async fn set_stored(&self, identity: Option<&HandoverIdentity>) {
        let pem = identity.map(|identity| identity.to_pem().unwrap());
        self.secrets
            .set_secret(&HandoverIdentity::secret_key(), pem.as_deref())
            .await
            .unwrap();
    }

    async fn mint_anew(&self, name: &str) -> Result<HandoverIdentity, IdentityError> {
        HandoverIdentity::mint_anew(&self.secrets, &self.record, self.store(), name, Utc::now())
            .await
    }

    fn stored_pem(&self) -> Option<String> {
        self.secrets
            .values
            .lock()
            .unwrap()
            .iter()
            .find(|(key, _)| *key == HandoverIdentity::secret_key())
            .map(|(_, value)| value.clone())
    }
}

#[tokio::test]
async fn load_or_create_mints_once_and_reloads_the_same_identity() {
    let places = Places::new();
    let first = places.load("Steno on Test Mac").await.unwrap();
    let stored = places.stored_pem().expect("the identity was stored");
    assert!(stored.starts_with("-----BEGIN CERTIFICATE-----"));
    assert_eq!(HandoverIdentity::secret_key().as_str(), "handover-identity");
    assert_eq!(
        places.record.get_recorded(),
        Some(hex(&first.fingerprint())),
        "the mint records the fingerprint"
    );

    let second = places.load("Another name").await.unwrap();
    assert_eq!(
        second.fingerprint(),
        first.fingerprint(),
        "loaded, not minted"
    );
    assert_eq!(second.mac_id(), first.mac_id());
    assert_eq!(*places.secrets.reads.lock().unwrap(), 2);

    // A stored secret that does not parse is an error, never a re-mint
    // (which would re-pair every phone).
    places
        .secrets
        .set_secret(&HandoverIdentity::secret_key(), Some("garbage"))
        .await
        .unwrap();
    let error = places.load("x").await.unwrap_err();
    assert!(error.to_string().contains("certificate"), "{error}");
}

#[tokio::test]
async fn with_a_phone_paired_a_missing_identity_is_unavailable_not_minted() {
    let places = Places::new();
    places.pair_a_phone();
    let error = places.load("x").await.unwrap_err();
    assert!(
        matches!(error, IdentityError::Unavailable(Unavailability::Missing)),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .ends_with("a new identity would make every phone pair again"),
        "the reason says what a mint would cost: {error}"
    );
    assert_eq!(places.stored_pem(), None, "nothing minted");
    assert_eq!(places.record.get_recorded(), None);
}

#[tokio::test]
async fn with_a_fingerprint_recorded_a_missing_identity_is_unavailable_not_minted() {
    let places = Places::new();
    places.record.record("ab12").unwrap();
    let error = places.load("x").await.unwrap_err();
    assert!(
        matches!(error, IdentityError::Unavailable(Unavailability::Missing)),
        "{error}"
    );
    assert_eq!(places.stored_pem(), None, "nothing minted");
    assert_eq!(places.record.get_recorded().as_deref(), Some("ab12"));
}

#[tokio::test]
async fn an_identity_with_another_fingerprint_is_unavailable_and_both_stay() {
    let places = Places::new();
    let paired = places.load("x").await.unwrap();
    let other_pem = HandoverIdentity::mint("y", Utc::now())
        .unwrap()
        .to_pem()
        .unwrap();
    places
        .secrets
        .set_secret(&HandoverIdentity::secret_key(), Some(&other_pem))
        .await
        .unwrap();
    let error = places.load("x").await.unwrap_err();
    assert!(
        matches!(error, IdentityError::Unavailable(Unavailability::Replaced)),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .ends_with("a new identity would make every phone pair again"),
        "{error}"
    );
    assert_eq!(
        places.record.get_recorded(),
        Some(hex(&paired.fingerprint()))
    );
    assert_eq!(places.stored_pem(), Some(other_pem), "neither is replaced");
}

/// A rollback that lost the record keeps the paired phones and the
/// identity: the next load keeps the identity and records it again.
#[tokio::test]
async fn an_identity_found_without_a_record_is_kept_and_recorded_again() {
    let places = Places::new();
    let identity = HandoverIdentity::mint("x", Utc::now()).unwrap();
    places
        .secrets
        .set_secret(
            &HandoverIdentity::secret_key(),
            Some(&identity.to_pem().unwrap()),
        )
        .await
        .unwrap();
    places.pair_a_phone();
    let loaded = places.load("x").await.unwrap();
    assert_eq!(loaded.fingerprint(), identity.fingerprint());
    assert_eq!(
        places.record.get_recorded(),
        Some(hex(&identity.fingerprint()))
    );
}

/// A secret store that cannot be read (a locked keyring) is unavailable,
/// and nothing is minted, whatever else holds.
#[tokio::test]
async fn an_unreadable_secret_store_is_unavailable_not_minted() {
    struct Locked;
    #[steno_core::async_trait]
    impl SecretStore for Locked {
        async fn secret(&self, _key: &SecretKey) -> BoundaryResult<Option<String>> {
            Err("the keyring is locked".into())
        }
        async fn set_secret(&self, _key: &SecretKey, _value: Option<&str>) -> BoundaryResult<()> {
            panic!("nothing is written");
        }
    }
    let places = Places::new();
    let error =
        HandoverIdentity::load_or_create(&Locked, &places.record, places.store(), "x", Utc::now())
            .await
            .unwrap_err();
    assert!(
        matches!(
            error,
            IdentityError::Unavailable(Unavailability::Unreadable(_))
        ),
        "{error}"
    );
    assert!(error.to_string().contains("locked"), "{error}");
    assert_eq!(places.record.get_recorded(), None);
}

#[tokio::test]
async fn store_writes_the_identity_and_records_its_fingerprint() {
    let places = Places::new();
    let identity = HandoverIdentity::mint("x", Utc::now()).unwrap();
    identity
        .store(&places.secrets, &places.record)
        .await
        .unwrap();
    assert_eq!(
        places.record.get_recorded(),
        Some(hex(&identity.fingerprint()))
    );
    assert_eq!(places.stored_pem(), Some(identity.to_pem().unwrap()));
    places.pair_a_phone();
    let loaded = places.load("x").await.unwrap();
    assert_eq!(loaded.fingerprint(), identity.fingerprint());
}

/// `store` over another identity (an identity brought over from the Swift
/// app replaces the minted one): the secret and the record both take the
/// new one, and the next load finds it.
#[tokio::test]
async fn store_replaces_an_identity_and_a_fingerprint_recorded_before() {
    let places = Places::new();
    let first = places.load("x").await.unwrap();
    let replacement = HandoverIdentity::mint("y", Utc::now()).unwrap();
    replacement
        .store(&places.secrets, &places.record)
        .await
        .unwrap();
    assert_ne!(first.fingerprint(), replacement.fingerprint());
    assert_eq!(
        places.record.get_recorded(),
        Some(hex(&replacement.fingerprint()))
    );
    assert_eq!(places.stored_pem(), Some(replacement.to_pem().unwrap()));
    let loaded = places.load("x").await.unwrap();
    assert_eq!(loaded.fingerprint(), replacement.fingerprint());
}

/// A record that cannot be read is an error, never "nothing recorded":
/// nothing is minted over it.
#[tokio::test]
async fn a_record_that_cannot_be_read_mints_nothing() {
    struct Damaged;
    impl FingerprintRecord for Damaged {
        fn recorded(&self) -> BoundaryResult<Option<String>> {
            Err("the record does not parse".into())
        }
        fn record(&self, _fingerprint: &str) -> BoundaryResult<()> {
            panic!("nothing is recorded");
        }
        fn next(&self) -> BoundaryResult<Option<String>> {
            Err("the record does not parse".into())
        }
        fn note_next(&self, _fingerprint: Option<&str>) -> BoundaryResult<()> {
            panic!("nothing is noted");
        }
    }
    let places = Places::new();
    let error = HandoverIdentity::load_or_create(
        &places.secrets,
        &Damaged,
        places.store(),
        "x",
        Utc::now(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, IdentityError::Record(_)), "{error}");
    assert_eq!(places.stored_pem(), None, "nothing minted");
}

/// A mint whose write fails records nothing, so the next load may mint
/// again instead of finding a fingerprint without its identity.
#[tokio::test]
async fn a_mint_whose_write_fails_records_nothing() {
    struct Refusing;
    #[steno_core::async_trait]
    impl SecretStore for Refusing {
        async fn secret(&self, _key: &SecretKey) -> BoundaryResult<Option<String>> {
            Ok(None)
        }
        async fn set_secret(&self, _key: &SecretKey, _value: Option<&str>) -> BoundaryResult<()> {
            Err("the keyring refused the write".into())
        }
    }
    let places = Places::new();
    let error = HandoverIdentity::load_or_create(
        &Refusing,
        &places.record,
        places.store(),
        "x",
        Utc::now(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, IdentityError::Secrets(_)), "{error}");
    assert_eq!(places.record.get_recorded(), None);
}

/// Pair again replaces the identity the phones know, which a load never
/// does: the new identity is written, the paired phones are removed, and
/// its fingerprint replaces the old one; the next load finds it.
#[tokio::test]
async fn pair_again_removes_the_phones_and_mints_a_new_identity() {
    let places = Places::new();
    let old = places.load("x").await.unwrap();
    places.pair_a_phone();
    let new = places.mint_anew("y").await.unwrap();
    assert_ne!(new.fingerprint(), old.fingerprint());
    assert!(places.store().paired_devices().unwrap().is_empty());
    assert_eq!(places.record.get_recorded(), Some(hex(&new.fingerprint())));
    assert_eq!(places.stored_pem(), Some(new.to_pem().unwrap()));
    let loaded = places.load("x").await.unwrap();
    assert_eq!(loaded.fingerprint(), new.fingerprint());
}

/// A Pair again whose write the secret store refuses changes nothing the
/// phones rely on, for each reason a load refuses the identity: the paired
/// phone, the recorded fingerprint and the stored identity stay, and the
/// next load refuses it for the same reason. The note stays for that load
/// to decide: it drops it where no identity is stored (abandoned) and
/// keeps it where the read is refused (undecided) or another identity is
/// stored (unknown).
#[tokio::test]
async fn a_refused_write_keeps_the_note_and_the_next_load_decides_it() {
    for reason in ["unreadable", "missing", "replaced"] {
        let places = Places::new();
        places.load("x").await.unwrap();
        places.pair_a_phone();
        let key = HandoverIdentity::secret_key();
        match reason {
            "missing" => places.secrets.set_secret(&key, None).await.unwrap(),
            "replaced" => {
                let other = HandoverIdentity::mint("z", Utc::now()).unwrap();
                let pem = other.to_pem().unwrap();
                places.secrets.set_secret(&key, Some(&pem)).await.unwrap();
            }
            _ => places.secrets.refuse_reads.store(true, Ordering::SeqCst),
        }
        let (stored, recorded, _, phones) = places.places();
        places.secrets.refuse_writes.store(true, Ordering::SeqCst);
        let error = places.mint_anew("y").await.unwrap_err();
        assert!(
            matches!(error, IdentityError::Secrets(_)),
            "{reason}: {error}"
        );
        let after = places.places();
        assert_eq!(
            (&after.0, &after.1, &after.3),
            (&stored, &recorded, &phones),
            "{reason}: nothing the phones rely on changed"
        );
        let note = after.2;
        assert!(note.is_some(), "{reason}: the note stays");
        let next = places.load("x").await.unwrap_err();
        let same = match reason {
            "unreadable" => matches!(
                next,
                IdentityError::Unavailable(Unavailability::Unreadable(_))
            ),
            "missing" => matches!(next, IdentityError::Unavailable(Unavailability::Missing)),
            _ => matches!(next, IdentityError::Unavailable(Unavailability::Replaced)),
        };
        assert!(same, "{reason}: {next}");
        let note = if reason == "missing" { None } else { note };
        assert_eq!(
            places.places(),
            (stored, recorded, note, phones),
            "{reason}: the load minted nothing and decided the note"
        );
    }
}

/// A write the secret store reports refused although it kept the identity
/// leaves the note, so the next load finishes the Pair again like any stop
/// after the write, whether or not a fingerprint was recorded before: the
/// phone goes, the new fingerprint is recorded and nothing is minted.
#[tokio::test]
async fn a_write_reported_refused_that_landed_is_finished_by_the_next_load() {
    for recorded_before in [true, false] {
        let places = Places::new();
        places.load("x").await.unwrap();
        places.set_stored(None).await;
        if !recorded_before {
            *places.record.recorded.lock().unwrap() = None;
        }
        places.pair_a_phone();
        places.secrets.keep_and_refuse.store(true, Ordering::SeqCst);
        let error = places.mint_anew("y").await.unwrap_err();
        assert!(matches!(error, IdentityError::Secrets(_)), "{error}");
        places
            .secrets
            .keep_and_refuse
            .store(false, Ordering::SeqCst);
        let (stored, recorded, next, phones) = places.places();
        let new = HandoverIdentity::from_pem(stored.as_deref().unwrap()).unwrap();
        assert_eq!(recorded.is_some(), recorded_before);
        assert_eq!(next, Some(hex(&new.fingerprint())), "the note stays");
        assert_eq!(phones.len(), 1);

        let loaded = places.load("x").await.unwrap();
        assert_eq!(loaded.fingerprint(), new.fingerprint());
        assert_eq!(
            places.places(),
            (stored, Some(hex(&new.fingerprint())), None, Vec::new()),
            "recorded before {recorded_before}: finished, nothing minted"
        );
    }
}

/// A Pair again that stops after the new identity was written (the phones
/// still paired, one of two removed, or all removed and the record
/// refused) is finished by the next load, whether or not a fingerprint was
/// recorded before (none is on the first launch after the Swift app): the
/// phones go, the new fingerprint is recorded and the note dropped, and
/// nothing is minted. No phone stays listed beside an identity it does not
/// know.
#[tokio::test]
async fn a_pair_again_stopped_after_the_write_is_finished_by_the_next_load() {
    for recorded_before in [true, false] {
        for stop in ["after the write", "after phone 1 of 2", "after the phones"] {
            let case = format!("recorded before {recorded_before}, {stop}");
            let places = Places::new();
            if recorded_before {
                places.load("x").await.unwrap();
                places.set_stored(None).await;
            }
            let phones = if stop == "after phone 1 of 2" { 2 } else { 1 };
            places.pair_phones(phones);
            let old = places.record.get_recorded();
            assert_eq!(old.is_some(), recorded_before, "{case}");
            assert!(
                matches!(
                    places.load("x").await.unwrap_err(),
                    IdentityError::Unavailable(Unavailability::Missing)
                ),
                "{case}: waits as missing"
            );

            let refuse_record = stop == "after the phones";
            if refuse_record {
                places.record.refuse.store(true, Ordering::SeqCst);
            } else {
                places.keep_the_last_phone();
            }
            let error = places.mint_anew("y").await.unwrap_err();
            if refuse_record {
                assert!(matches!(error, IdentityError::Record(_)), "{case}: {error}");
                places.record.refuse.store(false, Ordering::SeqCst);
            } else {
                assert!(matches!(error, IdentityError::Store(_)), "{case}: {error}");
                places.let_the_last_phone_go();
            }
            let (stored, recorded, next, listed) = places.places();
            let new = HandoverIdentity::from_pem(stored.as_deref().unwrap()).unwrap();
            assert_eq!(recorded, old, "{case}: the old record stays");
            assert_eq!(next, Some(hex(&new.fingerprint())), "{case}: the note");
            assert_eq!(listed.len(), usize::from(!refuse_record), "{case}");

            let loaded = places.load("x").await.unwrap();
            assert_eq!(loaded.fingerprint(), new.fingerprint(), "{case}");
            assert_eq!(
                places.places(),
                (stored, Some(hex(&new.fingerprint())), None, Vec::new()),
                "{case}: finished, nothing minted"
            );
            let again = places.load("x").await.unwrap();
            assert_eq!(again.fingerprint(), new.fingerprint(), "{case}: settled");
        }
    }
}

/// The next fingerprint is noted before the write, so a note that cannot
/// be written stops Pair again with nothing written.
#[tokio::test]
async fn a_note_that_cannot_be_written_stops_pair_again_before_the_write() {
    let places = Places::new();
    places.load("x").await.unwrap();
    places.pair_a_phone();
    let before = places.places();
    places.record.refuse_note.store(true, Ordering::SeqCst);
    let error = places.mint_anew("y").await.unwrap_err();
    assert!(matches!(error, IdentityError::Note(_)), "{error}");
    assert_eq!(places.places(), before, "nothing written");
}

/// What the load does with the note of an unfinished Pair again when the
/// secret store does not hold the next identity, by what it holds: the
/// recorded identity or none (the Pair again changed nothing, so the note
/// goes and the load reads as without it), another identity (the note
/// stays and the load waits as replaced, also with nothing recorded), or
/// nothing it can read (the note stays and the load waits as unreadable).
/// None of them mints or removes a phone.
#[tokio::test]
async fn an_unfinished_pair_again_is_decided_by_what_the_secret_store_holds() {
    let next = hex(&HandoverIdentity::mint("y", Utc::now())
        .unwrap()
        .fingerprint());
    for case in [
        "abandoned: the recorded identity",
        "abandoned: no identity, a fingerprint recorded",
        "abandoned: no identity, nothing recorded",
        "unknown: another identity",
        "unknown: another identity, nothing recorded",
        "undecided: unreadable",
    ] {
        let places = Places::new();
        let old = places.load("x").await.unwrap();
        places.pair_a_phone();
        let other = HandoverIdentity::mint("z", Utc::now()).unwrap();
        if case.starts_with("abandoned: no identity") {
            places.set_stored(None).await;
        }
        if case.starts_with("unknown") {
            places.set_stored(Some(&other)).await;
        }
        if case.ends_with("nothing recorded") {
            *places.record.recorded.lock().unwrap() = None;
        }
        places.record.note_next(Some(&next)).unwrap();
        if case.starts_with("undecided") {
            places.secrets.refuse_reads.store(true, Ordering::SeqCst);
        }
        let before = places.places();

        let loaded = places.load("x").await;
        let (stored, recorded, noted, phones) = places.places();
        assert_eq!(
            (&stored, &recorded, &phones),
            (&before.0, &before.1, &before.3),
            "{case}: nothing minted or removed"
        );
        match case.split(':').next().unwrap() {
            "abandoned" => assert_eq!(noted, None, "{case}: the note goes"),
            _ => assert_eq!(noted, Some(next.clone()), "{case}: the note stays"),
        }
        match case {
            "abandoned: the recorded identity" => {
                assert_eq!(loaded.unwrap().fingerprint(), old.fingerprint());
            }
            "undecided: unreadable" => assert!(
                matches!(
                    loaded,
                    Err(IdentityError::Unavailable(Unavailability::Unreadable(_)))
                ),
                "{case}"
            ),
            _ if case.starts_with("unknown") => assert!(
                matches!(
                    loaded,
                    Err(IdentityError::Unavailable(Unavailability::Replaced))
                ),
                "{case}"
            ),
            _ => assert!(
                matches!(
                    loaded,
                    Err(IdentityError::Unavailable(Unavailability::Missing))
                ),
                "{case}"
            ),
        }
    }
}

/// The mint time of the identities below; their certificates start a
/// minute before it.
const MINT: i64 = 1_790_000_000;

/// An identity minted at [`MINT`] and stored with nothing recorded (an
/// upgrade from a build without the record, or a lost record), with phones
/// paired at `seconds` after the mint (negative: before).
async fn unrecorded_identity_with_phones(seconds: &[i64]) -> (Places, HandoverIdentity) {
    let places = Places::new();
    let identity = HandoverIdentity::mint("x", common::date(MINT)).unwrap();
    places.set_stored(Some(&identity)).await;
    let times: Vec<_> = seconds
        .iter()
        .map(|offset| common::date(MINT + offset))
        .collect();
    places.pair_phones_at(&times);
    (places, identity)
}

/// The ids [`Places::pair_phones_at`] gives `count` phones.
fn phone_ids(count: usize) -> Vec<uuid::Uuid> {
    (1..=count as u128).map(uuid::Uuid::from_u128).collect()
}

/// With nothing recorded, the identity found is recorded when every phone
/// was paired after its certificate starts, and when no phone is paired.
#[tokio::test]
async fn an_unrecorded_identity_every_phone_knows_is_recorded() {
    for phones in [&[][..], &[0, 3600, 86_400][..]] {
        let (places, identity) = unrecorded_identity_with_phones(phones).await;
        let loaded = places.load("x").await.unwrap();
        assert_eq!(loaded.fingerprint(), identity.fingerprint(), "{phones:?}");
        assert_eq!(
            places.record.get_recorded(),
            Some(hex(&identity.fingerprint())),
            "{phones:?}"
        );
        assert_eq!(
            places.store().paired_devices().unwrap().len(),
            phones.len(),
            "{phones:?}: the phones stay"
        );
    }
}

/// An identity every paired phone predates, as a silent mint after a
/// keychain reset left it, is no identity they know: the load waits as
/// replaced, records nothing and mints nothing, and the phones and the
/// identity stay for the user's Pair again.
#[tokio::test]
async fn an_unrecorded_identity_every_phone_predates_is_replaced() {
    let (places, identity) = unrecorded_identity_with_phones(&[-86_400, -3600]).await;
    let error = places.load("x").await.unwrap_err();
    assert!(
        matches!(error, IdentityError::Unavailable(Unavailability::Replaced)),
        "{error}"
    );
    assert_eq!(
        places.places(),
        (
            Some(identity.to_pem().unwrap()),
            None,
            None,
            vec![uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(2)]
        ),
        "nothing written"
    );
}

/// Some phones paired before the identity and some after it, among them
/// one a second before its start beside one at it: partly replaced,
/// nothing recorded or minted.
#[tokio::test]
async fn an_unrecorded_identity_some_phones_predate_is_partly_replaced() {
    for phones in [[-86_400, 3600], [-61, -60]] {
        let (places, identity) = unrecorded_identity_with_phones(&phones).await;
        let error = places.load("x").await.unwrap_err();
        assert!(
            matches!(
                error,
                IdentityError::Unavailable(Unavailability::PartlyReplaced)
            ),
            "{phones:?}: {error}"
        );
        assert_eq!(places.record.get_recorded(), None, "{phones:?}");
        assert_eq!(places.stored_pem(), Some(identity.to_pem().unwrap()));
        assert_eq!(places.store().paired_devices().unwrap().len(), 2);
    }
}

/// A minted certificate starts a minute before its mint, so a phone paired
/// 59 or 60 seconds before the mint counts as paired after the start and
/// the identity is recorded, and one paired 61 seconds before predates it.
#[tokio::test]
async fn a_phone_paired_at_the_identity_s_start_counts_as_paired_after_it() {
    for (before_the_mint, recorded) in [(59, true), (60, true), (61, false)] {
        let (places, identity) = unrecorded_identity_with_phones(&[-before_the_mint]).await;
        assert_eq!(
            identity.not_valid_before().unwrap(),
            common::date(MINT - 60),
            "back-dated a minute"
        );
        let loaded = places.load("x").await;
        if recorded {
            assert_eq!(
                loaded.unwrap().fingerprint(),
                identity.fingerprint(),
                "{before_the_mint} s"
            );
            assert_eq!(
                places.record.get_recorded(),
                Some(hex(&identity.fingerprint()))
            );
        } else {
            assert!(
                matches!(
                    loaded,
                    Err(IdentityError::Unavailable(Unavailability::Replaced))
                ),
                "{before_the_mint} s"
            );
            assert_eq!(places.record.get_recorded(), None);
        }
    }
}

/// With nothing recorded, a certificate whose start cannot be read and a
/// pairing time that cannot be read are no reason to record the identity,
/// nor to mint: the load fails, and nothing is written.
#[tokio::test]
async fn an_unrecorded_identity_whose_times_cannot_be_read_is_not_recorded() {
    use base64::Engine as _;
    // A certificate that does not parse, beside a key that does.
    let places = Places::new();
    let key = HandoverIdentity::mint("x", common::date(MINT))
        .unwrap()
        .to_pem()
        .unwrap();
    let key = &key[key.find("-----BEGIN PRIVATE KEY-----").unwrap()..];
    let broken = format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n{key}",
        base64::engine::general_purpose::STANDARD.encode([0x30, 0x00])
    );
    places
        .secrets
        .set_secret(&HandoverIdentity::secret_key(), Some(&broken))
        .await
        .unwrap();
    places.pair_a_phone();
    let error = places.load("x").await.unwrap_err();
    assert!(matches!(error, IdentityError::Malformed(_)), "{error}");
    assert_eq!(places.record.get_recorded(), None);
    assert_eq!(places.stored_pem(), Some(broken), "nothing minted");

    // A pairing time the database holds but cannot read.
    let (places, identity) = unrecorded_identity_with_phones(&[3600]).await;
    common::execute_batch(
        places.store(),
        "UPDATE pairedDevice SET pairedAt = 'yesterday';",
    );
    let error = places.load("x").await.unwrap_err();
    assert!(matches!(error, IdentityError::Store(_)), "{error}");
    assert_eq!(places.record.get_recorded(), None);
    assert_eq!(places.stored_pem(), Some(identity.to_pem().unwrap()));
}

/// A recorded identity is adopted without the pairing-time rule, even when
/// every phone (or some) predates its start or a time is after now, as the
/// Swift import may record one the Swift app's record named: it loads,
/// writes nothing and keeps the phones.
#[tokio::test]
async fn a_recorded_identity_the_phones_predate_loads_without_the_rule() {
    for (phones, now) in [
        (&[-86_400, -3600][..], MINT + 86_400),
        (&[-86_400, 3600][..], MINT + 86_400),
        (&[-86_400][..], MINT - 3600),
        (&[3600][..], MINT),
    ] {
        let case = format!("{phones:?} at {now}");
        let (places, identity) = unrecorded_identity_with_phones(phones).await;
        places.record.record(&hex(&identity.fingerprint())).unwrap();
        let loaded = places.load_at("x", common::date(now)).await.unwrap();
        assert_eq!(loaded.fingerprint(), identity.fingerprint(), "{case}");
        assert_eq!(
            places.places(),
            (
                Some(identity.to_pem().unwrap()),
                Some(hex(&identity.fingerprint())),
                None,
                phone_ids(phones.len())
            ),
            "{case}: nothing written, the phones stay"
        );
    }
}

/// With nothing recorded, a certificate that starts after now, or a phone
/// paired after now (by more than a second, the database's rounding),
/// means this computer's clock went back: the load waits
/// for the clock, with phones or without, records and mints nothing; once
/// the clock has passed both, the rule decides.
#[tokio::test]
async fn an_unrecorded_identity_or_pairing_after_now_waits_for_the_clock() {
    let start = MINT - 60;
    for (phones, now, recorded_later) in [
        // The certificate starts after now, no phone paired.
        (&[][..], start - 2, true),
        // The certificate starts after now, a phone paired since.
        (&[-3600][..], MINT - 7200, false),
        // A phone paired after now (the clock was fast when it paired).
        (&[86_400][..], MINT + 3600, true),
        (&[3600, 86_400][..], MINT + 7200, true),
        (&[-86_400, 86_400][..], MINT + 7200, false),
    ] {
        let case = format!("{phones:?} at {now}");
        let (places, identity) = unrecorded_identity_with_phones(phones).await;
        let error = places.load_at("x", common::date(now)).await.unwrap_err();
        assert!(
            matches!(
                error,
                IdentityError::Unavailable(Unavailability::ClockBehind)
            ),
            "{case}: {error}"
        );
        assert_eq!(
            places.places(),
            (
                Some(identity.to_pem().unwrap()),
                None,
                None,
                phone_ids(phones.len())
            ),
            "{case}: nothing written"
        );
        // Once the clock is past every time, the rule decides.
        let later = places.load_at("x", common::date(MINT + 2 * 86_400)).await;
        if recorded_later {
            assert_eq!(
                later.unwrap().fingerprint(),
                identity.fingerprint(),
                "{case}"
            );
            assert_eq!(
                places.record.get_recorded(),
                Some(hex(&identity.fingerprint())),
                "{case}"
            );
        } else {
            assert!(
                matches!(
                    later,
                    Err(IdentityError::Unavailable(
                        Unavailability::Replaced | Unavailability::PartlyReplaced
                    ))
                ),
                "{case}"
            );
            assert_eq!(places.record.get_recorded(), None, "{case}");
        }
    }
}
