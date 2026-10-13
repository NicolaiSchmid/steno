//! The import's keychain half against a real file keychain (macOS only,
//! opt-in with `STENO_KEYCHAIN_TESTS=1`): a throwaway keychain opened by
//! path with user interaction disabled holds the committed test identity,
//! stored with the calls Swift's `IdentityKeychain.store` makes (the SEC1
//! key through `SecItemImport`, the certificate, both relabelled with
//! `SecItemUpdate`); the import finds it by label, exports it as PKCS#12
//! through `kSecMatchSearchList` with that keychain as
//! `SecIdentityCreateWithCertificate`'s search list, and decodes it to the
//! fingerprint and Mac id `IdentityFixtureTests` (Swift) computes from the
//! same fixture. A second test puts a second Swift identity into a second
//! throwaway keychain, as an old login keychain added back beside a new one
//! does: the lookup lists both certificates, and the export hands over the
//! one asked for. Nothing touches the default keychain.
//!
//! With `STENO_WRITE_EXPORT_FIXTURE=<path>` the test also writes the
//! export under the passphrase the platform-independent decode test uses
//! (`swift_import::tests`), which is how
//! `tests/fixtures/swift-import/test-identity.exported.p12` was made.

#![cfg(target_os = "macos")]

use std::path::Path;

use security_framework::certificate::SecCertificate;
use security_framework::item::{
    AddRef, ItemAddOptions, ItemAddValue, ItemClass, ItemSearchOptions, Limit, Location, Reference,
    SearchResult,
};
use security_framework::os::macos::keychain::{CreateOptions, SecKeychain};
use steno_handover::identity::hex;
use steno_macos::keychain::fixture;
use steno_services::swift_import::{
    ApiKeyItem, ImportMarker, LoginKeychain, SWIFT_API_KEY_LABEL, SWIFT_IDENTITY_LABEL,
    SwiftKeychain, decode_pkcs12,
};

/// `HandoverIdentity.fingerprint` of `test-identity.der`, as
/// `IdentityFixtureTests` computes it.
const FINGERPRINT: &str = "76ac0c0b28f976f25589214d5b2c614d983df9de081b06ec1fe9247bf1e40d92";
/// `HandoverIdentity.macID` of the same, as `IdentityFixtureTests`
/// computes it.
const MAC_ID: &str = "FCF0D2A1-D2CE-48F7-BAFF-E17FD2E9C814";
/// The passphrase of the committed export.
const EXPORTED_PASSPHRASE: &str = "steno-export-fixture";

fn handover_fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../Tests/Fixtures/handover")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Deletes the keychain (file and search list entry) when the test ends,
/// passed or not.
struct Throwaway(Option<SecKeychain>);

impl Drop for Throwaway {
    fn drop(&mut self) {
        if let Some(keychain) = self.0.take() {
            fixture::delete_keychain(&keychain).expect("the test keychain is deleted");
        }
    }
}

fn stored_certificate(keychain: &SecKeychain, der: &[u8]) -> SecCertificate {
    ItemSearchOptions::new()
        .keychains(std::slice::from_ref(keychain))
        .class(ItemClass::certificate())
        .load_refs(true)
        .limit(Limit::All)
        .search()
        .unwrap()
        .into_iter()
        .find_map(|result| match result {
            SearchResult::Ref(Reference::Certificate(certificate))
                if certificate.to_der() == der =>
            {
                Some(certificate)
            }
            _ => None,
        })
        .expect("the certificate is in the test keychain")
}

/// What `IdentityKeychain.store` does, into `keychain`.
fn store_as_swift_does(keychain: &SecKeychain, certificate_der: &[u8], sec1: &[u8]) {
    let key = fixture::import_sec1_private_key(sec1, keychain)
        .unwrap()
        .expect("a fresh keychain does not hold the key yet");
    fixture::label_key(&key, SWIFT_IDENTITY_LABEL).unwrap();
    let certificate = SecCertificate::from_der(certificate_der).unwrap();
    let mut add = ItemAddOptions::new(ItemAddValue::Ref(AddRef::Certificate(certificate)));
    add.set_location(Location::FileKeychain(keychain.clone()));
    add.add().unwrap();
    fixture::label_certificate(
        &stored_certificate(keychain, certificate_der),
        SWIFT_IDENTITY_LABEL,
    )
    .unwrap();
}

#[test]
fn the_swift_identity_exports_from_a_keychain_and_keeps_its_fingerprint_and_mac_id() {
    if std::env::var("STENO_KEYCHAIN_TESTS").as_deref() != Ok("1") {
        eprintln!("set STENO_KEYCHAIN_TESTS=1 to run the keychain test");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (opened, _delete) = throwaway_keychain(&dir.path().join("steno-import-test.keychain-db"));
    // Any prompt now fails the call instead of waiting for a person.
    let _no_prompts = SecKeychain::disable_user_interaction().unwrap();

    let certificate_der = handover_fixture("test-identity.der");
    store_as_swift_does(
        &opened,
        &certificate_der,
        &handover_fixture("test-identity.key.der"),
    );

    let swift = LoginKeychain {
        keychains: vec![opened.clone()],
    };
    assert_eq!(
        swift.swift_certificates().unwrap(),
        std::slice::from_ref(&certificate_der),
        "found by label"
    );
    assert_eq!(
        swift.api_key_item().unwrap(),
        ApiKeyItem::Missing,
        "no key item yet"
    );
    assert!(!swift.has_stored_identity().unwrap(), "no entry yet");
    // The import's marker: added into the test's keychain (an item of this
    // process's own, so nothing prompts), found by its attributes, and a
    // second add leaves it as it is.
    assert!(!swift.import_done().unwrap(), "no marker yet");
    swift.mark_import_done().unwrap();
    assert!(swift.import_done().unwrap(), "the marker is found");
    swift.mark_import_done().unwrap();
    assert!(swift.import_done().unwrap());

    let passphrase = "a passphrase for this export";
    let exported = swift.export_identity(&certificate_der, passphrase).unwrap();
    let (identity, _bundle) = decode_pkcs12(&exported, passphrase, &certificate_der).unwrap();
    assert_eq!(hex(&identity.fingerprint()), FINGERPRINT);
    assert_eq!(
        steno_core::json::uuid_string(identity.mac_id()),
        MAC_ID,
        "the Mac id the paired phones know"
    );

    if let Some(target) = std::env::var_os("STENO_WRITE_EXPORT_FIXTURE") {
        let committed = swift
            .export_identity(&certificate_der, EXPORTED_PASSPHRASE)
            .unwrap();
        std::fs::write(target, committed).unwrap();
    }

    // The items a desktop-id build leaves (no label) and the Swift API
    // key item (labelled): told apart by their attributes, without reading
    // them, so no prompt comes up while user interaction is disabled.
    let add_password = |account: &str, label: Option<&str>| {
        let mut item = ItemAddOptions::new(ItemAddValue::Data {
            class: ItemClass::generic_password(),
            data: core_foundation::data::CFData::from_buffer(b"secret"),
        });
        item.set_service(steno_services::secrets::KEYRING_SERVICE)
            .set_account_name(account)
            .set_location(Location::FileKeychain(opened.clone()));
        if let Some(label) = label {
            item.set_label(label);
        }
        item.add().unwrap();
    };
    add_password(steno_handover::HandoverIdentity::SECRET_KEY, None);
    assert!(swift.has_stored_identity().unwrap());
    add_password(steno_core::SecretKey::LLM_API_KEY, None);
    assert_eq!(swift.api_key_item().unwrap(), ApiKeyItem::Other);
    ItemSearchOptions::new()
        .keychains(std::slice::from_ref(&opened))
        .class(ItemClass::generic_password())
        .service(steno_services::secrets::KEYRING_SERVICE)
        .account(steno_core::SecretKey::LLM_API_KEY)
        .delete()
        .unwrap();
    add_password(
        steno_core::SecretKey::LLM_API_KEY,
        Some(SWIFT_API_KEY_LABEL),
    );
    assert_eq!(swift.api_key_item().unwrap(), ApiKeyItem::Swift);
}

/// A throwaway keychain at `path`, unlocked, deleted when the returned
/// guard drops.
fn throwaway_keychain(path: &Path) -> (SecKeychain, Throwaway) {
    let password = "steno-keychain-test";
    let mut keychain = CreateOptions::new()
        .password(password)
        .prompt_user(false)
        .create(path)
        .unwrap();
    let delete = Throwaway(Some(keychain.clone()));
    let mut opened = SecKeychain::open(path).unwrap();
    opened.unlock(Some(password)).unwrap();
    keychain.unlock(Some(password)).unwrap();
    (opened, delete)
}

/// The P-256 key of a minted identity as SEC1 (RFC 5915) DER with its
/// curve named, the form `IdentityKeychain.store` imports: the private
/// scalar and the public point taken from ring's PKCS#8, whose layout is
/// fixed.
fn sec1_of(identity: &steno_handover::HandoverIdentity) -> Vec<u8> {
    use base64::Engine as _;
    let pem = identity.to_pem().unwrap();
    let body: String = pem
        .split("-----BEGIN PRIVATE KEY-----")
        .nth(1)
        .and_then(|rest| rest.split("-----END PRIVATE KEY-----").next())
        .unwrap()
        .split_whitespace()
        .collect();
    let pkcs8 = base64::engine::general_purpose::STANDARD
        .decode(body)
        .unwrap();
    let after = |marker: &[u8], length: usize| {
        let at = pkcs8
            .windows(marker.len())
            .position(|window| window == marker)
            .unwrap()
            + marker.len();
        pkcs8[at..at + length].to_vec()
    };
    let scalar = after(&[0x02, 0x01, 0x01, 0x04, 0x20], 32);
    let point = after(&[0xa1, 0x44, 0x03, 0x42, 0x00], 65);
    let mut sec1 = vec![0x30, 0x77, 0x02, 0x01, 0x01, 0x04, 0x20];
    sec1.extend(scalar);
    sec1.extend([
        0xa0, 0x0a, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0xa1, 0x44, 0x03,
        0x42, 0x00,
    ]);
    sec1.extend(point);
    sec1
}

/// Two Swift identities, one per keychain in the search list (an old
/// login keychain added back beside a new one), in either order: the
/// lookup lists both certificates, which the step chooses among
/// (`swift_import::choose_certificate`), and the export of either hands
/// over that one, with its key found in its own keychain.
#[test]
fn two_swift_identities_in_two_keychains_are_both_listed_and_each_exports() {
    if std::env::var("STENO_KEYCHAIN_TESTS").as_deref() != Ok("1") {
        eprintln!("set STENO_KEYCHAIN_TESTS=1 to run the keychain test");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (new, _delete_new) = throwaway_keychain(&dir.path().join("steno-new.keychain-db"));
    let (old, _delete_old) = throwaway_keychain(&dir.path().join("steno-old.keychain-db"));
    let _no_prompts = SecKeychain::disable_user_interaction().unwrap();

    let fixture_der = handover_fixture("test-identity.der");
    store_as_swift_does(
        &old,
        &fixture_der,
        &handover_fixture("test-identity.key.der"),
    );
    let minted = steno_handover::HandoverIdentity::mint(
        "Steno test identity, minted",
        chrono::DateTime::from_timestamp(1_790_000_000, 0).unwrap(),
    )
    .unwrap();
    let minted_der = minted.certificate_der().to_vec();
    store_as_swift_does(&new, &minted_der, &sec1_of(&minted));

    for (keychains, order) in [
        (vec![new.clone(), old.clone()], [&minted_der, &fixture_der]),
        (vec![old.clone(), new.clone()], [&fixture_der, &minted_der]),
    ] {
        let swift = LoginKeychain { keychains };
        let mut listed = swift.swift_certificates().unwrap();
        listed.sort();
        let mut both = order.map(Clone::clone).to_vec();
        both.sort();
        assert_eq!(listed, both, "both listed");
        for der in order {
            let passphrase = "a passphrase for this export";
            let exported = swift.export_identity(der, passphrase).unwrap();
            let (identity, _) = decode_pkcs12(&exported, passphrase, der).unwrap();
            assert_eq!(identity.certificate_der(), &der[..], "the one asked for");
        }
    }
}
