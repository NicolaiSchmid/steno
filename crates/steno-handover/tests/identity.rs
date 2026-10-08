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

use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{TimeZone as _, Utc};
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

#[derive(Default)]
struct MemorySecrets {
    values: Mutex<Vec<(SecretKey, String)>>,
    reads: Mutex<u32>,
}

#[steno_core::async_trait]
impl SecretStore for MemorySecrets {
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
        *self.reads.lock().unwrap() += 1;
        Ok(self
            .values
            .lock()
            .unwrap()
            .iter()
            .find(|(stored, _)| stored == key)
            .map(|(_, value)| value.clone()))
    }

    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        let mut values = self.values.lock().unwrap();
        values.retain(|(stored, _)| stored != key);
        if let Some(value) = value {
            values.push((key.clone(), value.to_owned()));
        }
        Ok(())
    }
}

/// A fingerprint record in memory.
#[derive(Default)]
struct MemoryRecord(Mutex<Option<String>>);

impl MemoryRecord {
    fn get(&self) -> Option<String> {
        self.0.lock().unwrap().clone()
    }
}

impl FingerprintRecord for MemoryRecord {
    fn recorded(&self) -> BoundaryResult<Option<String>> {
        Ok(self.get())
    }

    fn record(&self, fingerprint: &str) -> BoundaryResult<()> {
        *self.0.lock().unwrap() = Some(fingerprint.to_owned());
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
        HandoverIdentity::load_or_create(
            &self.secrets,
            &self.record,
            self.store(),
            name,
            Utc::now(),
        )
        .await
    }

    fn pair_a_phone(&self) {
        let device = PairedDevice {
            id: uuid::Uuid::from_u128(1),
            name: "Phone".to_owned(),
            paired_at: Utc::now(),
            last_seen_at: None,
        };
        self.store()
            .save_paired_device(&device, &[1u8; 32])
            .unwrap();
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
        places.record.get(),
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
    assert_eq!(places.record.get(), None);
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
    assert_eq!(places.record.get().as_deref(), Some("ab12"));
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
    assert_eq!(places.record.get(), Some(hex(&paired.fingerprint())));
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
    assert_eq!(places.record.get(), Some(hex(&identity.fingerprint())));
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
    assert_eq!(places.record.get(), None);
}

#[tokio::test]
async fn store_writes_the_identity_and_records_its_fingerprint() {
    let places = Places::new();
    let identity = HandoverIdentity::mint("x", Utc::now()).unwrap();
    identity
        .store(&places.secrets, &places.record)
        .await
        .unwrap();
    assert_eq!(places.record.get(), Some(hex(&identity.fingerprint())));
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
    assert_eq!(places.record.get(), Some(hex(&replacement.fingerprint())));
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
    assert_eq!(places.record.get(), None);
}
