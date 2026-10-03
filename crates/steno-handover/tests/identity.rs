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
use std::sync::{Arc, Mutex};

use chrono::{TimeZone as _, Utc};
use steno_core::{BoundaryResult, SecretKey, SecretStore};
use steno_handover::HandoverIdentity;
use steno_handover::identity::hex;
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

#[tokio::test]
async fn load_or_create_mints_once_and_reloads_the_same_identity() {
    let secrets = Arc::new(MemorySecrets::default());
    let now = Utc::now();
    let first = HandoverIdentity::load_or_create(secrets.as_ref(), "Steno on Test Mac", now)
        .await
        .unwrap();
    let stored = secrets
        .secret(&HandoverIdentity::secret_key())
        .await
        .unwrap()
        .expect("the identity was stored");
    assert!(stored.starts_with("-----BEGIN CERTIFICATE-----"));
    assert_eq!(HandoverIdentity::secret_key().as_str(), "handover-identity");

    let second = HandoverIdentity::load_or_create(secrets.as_ref(), "Another name", now)
        .await
        .unwrap();
    assert_eq!(
        second.fingerprint(),
        first.fingerprint(),
        "loaded, not minted"
    );
    assert_eq!(second.mac_id(), first.mac_id());
    assert_eq!(*secrets.reads.lock().unwrap(), 3);

    // A stored secret that does not parse is an error, never a re-mint
    // (which would re-pair every phone).
    secrets
        .set_secret(&HandoverIdentity::secret_key(), Some("garbage"))
        .await
        .unwrap();
    let error = HandoverIdentity::load_or_create(secrets.as_ref(), "x", now)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("certificate"), "{error}");
}
