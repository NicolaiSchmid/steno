//! The identity the phone pins: the minted certificate, the fingerprint
//! and computer id, the load that never mints over a pairing the phones
//! know, Pair again, and the decision against its truth table
//! (`tests/fixtures/identity-decisions.json`).

#![allow(clippy::large_futures, clippy::too_many_lines)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, TimeDelta, TimeZone as _, Utc};
use serde_json::Value;
use steno_core::{BoundaryResult, PairedDevice, SecretKey, SecretStore, Store};
use steno_handover::identity::decision::{
    Caller, Decision, Found, Identities, Observations, Outcome, Phones, Reason, Record, Write,
    decide,
};
use steno_handover::identity::hex;
use steno_handover::{FingerprintRecord, HandoverIdentity, IdentityError, Unavailability};
use x509_parser::prelude::*;

#[test]
fn mint_yields_a_self_signed_p256_server_certificate_for_ten_years() {
    let now = Utc.timestamp_opt(1_790_000_000, 0).unwrap();
    let minted = HandoverIdentity::mint("Steno on Test Mac", now).unwrap();
    let (rest, certificate) = X509Certificate::from_der(minted.certificate_der()).unwrap();
    assert_eq!(rest, b"");
    assert_eq!(certificate.subject().to_string(), "CN=Steno on Test Mac");
    assert_eq!(certificate.issuer(), certificate.subject());
    assert!(certificate.verify_signature(None).is_ok(), "self-signed");
    assert_eq!(
        certificate.signature_algorithm.algorithm,
        oid_registry::OID_SIG_ECDSA_WITH_SHA256
    );
    let not_before = certificate.validity().not_before.timestamp();
    let not_after = certificate.validity().not_after.timestamp();
    assert_eq!(now.timestamp() - not_before, 60, "a minute of backdating");
    assert_eq!((not_after - not_before) / (365 * 24 * 3600), 10);
    assert_eq!(minted.not_valid_before().unwrap().timestamp(), not_before);
    assert!(!certificate.basic_constraints().unwrap().unwrap().value.ca);
    let usage = certificate.key_usage().unwrap().unwrap().value;
    assert!(usage.digital_signature() && !usage.key_encipherment());
    assert!(
        certificate
            .extended_key_usage()
            .unwrap()
            .unwrap()
            .value
            .server_auth
    );
    let names = certificate.subject_alternative_name().unwrap().unwrap();
    assert_eq!(
        names.value.general_names,
        vec![GeneralName::DNSName("steno-on-test-mac.local")]
    );

    let again = HandoverIdentity::mint("Steno on Test Mac", now).unwrap();
    assert_ne!(
        again.fingerprint(),
        minted.fingerprint(),
        "a new key each mint"
    );
    assert_ne!(
        again.mac_id(),
        minted.mac_id(),
        "a new identity is a new computer"
    );
    let id = HandoverIdentity::mac_id_for_fingerprint(&minted.fingerprint());
    assert_eq!(id, minted.mac_id());
    assert_eq!(
        (id.get_version_num(), id.get_variant()),
        (4, uuid::Variant::RFC4122)
    );
}

/// The committed test identity, so the hashing here is the hashing the
/// phone does.
#[test]
fn the_fingerprint_is_the_sha256_of_the_leaf_der_the_phone_pins() {
    let der = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../Tests/Fixtures/handover/test-identity.der"),
    )
    .unwrap();
    assert_eq!(
        hex(&HandoverIdentity::fingerprint_of_der(&der)),
        "76ac0c0b28f976f25589214d5b2c614d983df9de081b06ec1fe9247bf1e40d92"
    );
}

/// A secret store in memory that can refuse reads or writes, or keep a write
/// and report it refused.
#[derive(Default)]
struct Secrets {
    value: Mutex<Option<String>>,
    refuse_reads: AtomicBool,
    refuse_writes: AtomicBool,
    keep_and_refuse: AtomicBool,
}

#[steno_core::async_trait]
impl SecretStore for Secrets {
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
        assert_eq!(*key, HandoverIdentity::secret_key());
        if self.refuse_reads.load(Ordering::SeqCst) {
            return Err("the keyring is locked".into());
        }
        Ok(self.value.lock().unwrap().clone())
    }

    async fn set_secret(&self, _key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        if self.refuse_writes.load(Ordering::SeqCst) {
            return Err("the keyring refused the write".into());
        }
        *self.value.lock().unwrap() = value.map(str::to_owned);
        if self.keep_and_refuse.load(Ordering::SeqCst) {
            return Err("the keyring did not answer".into());
        }
        Ok(())
    }
}

/// The fingerprint record in memory.
#[derive(Default)]
struct Memory {
    recorded: Mutex<Option<String>>,
    next: Mutex<Option<String>>,
    refuse: AtomicBool,
}

impl FingerprintRecord for Memory {
    fn recorded(&self) -> BoundaryResult<Option<String>> {
        Ok(self.recorded.lock().unwrap().clone())
    }

    fn record(&self, fingerprint: &str) -> BoundaryResult<()> {
        if self.refuse.load(Ordering::SeqCst) {
            return Err("No space left on device".into());
        }
        *self.recorded.lock().unwrap() = Some(fingerprint.to_owned());
        *self.next.lock().unwrap() = None;
        Ok(())
    }

    fn next(&self) -> BoundaryResult<Option<String>> {
        Ok(self.next.lock().unwrap().clone())
    }

    fn note_next(&self, fingerprint: Option<&str>) -> BoundaryResult<()> {
        *self.next.lock().unwrap() = fingerprint.map(str::to_owned);
        Ok(())
    }
}

/// What `load_or_create` reads.
struct Places {
    secrets: Secrets,
    record: Memory,
    store: Store,
}

/// The stored identity, the recorded and noted fingerprints, the phones.
type Seen = (Option<String>, Option<String>, Option<String>, usize);

impl Places {
    fn new() -> Self {
        Places {
            secrets: Secrets::default(),
            record: Memory::default(),
            store: Store::in_memory().unwrap(),
        }
    }

    async fn load(&self) -> Result<HandoverIdentity, IdentityError> {
        HandoverIdentity::load_or_create(&self.secrets, &self.record, &self.store, "x", Utc::now())
            .await
    }

    async fn mint_anew(&self) -> Result<HandoverIdentity, IdentityError> {
        HandoverIdentity::mint_anew(&self.secrets, &self.record, &self.store, "y", Utc::now()).await
    }

    fn pair_a_phone(&self) {
        let device = PairedDevice {
            id: uuid::Uuid::new_v4(),
            name: "Phone".to_owned(),
            paired_at: Utc::now(),
            last_seen_at: None,
        };
        self.store.save_paired_device(&device, &[1; 32]).unwrap();
    }

    fn seen(&self) -> Seen {
        (
            self.secrets.value.lock().unwrap().clone(),
            self.record.recorded.lock().unwrap().clone(),
            self.record.next.lock().unwrap().clone(),
            self.store.paired_devices().unwrap().len(),
        )
    }
}

fn unavailable(error: &IdentityError) -> Option<&Unavailability> {
    match error {
        IdentityError::Unavailable(why) => Some(why),
        _ => None,
    }
}

#[tokio::test]
async fn the_load_mints_once_then_serves_the_stored_identity() {
    let places = Places::new();
    let first = places.load().await.unwrap();
    let (stored, recorded, ..) = places.seen();
    assert!(stored.unwrap().starts_with("-----BEGIN CERTIFICATE-----"));
    assert_eq!(recorded, Some(hex(&first.fingerprint())));
    places.pair_a_phone();
    assert_eq!(
        places.load().await.unwrap().fingerprint(),
        first.fingerprint()
    );

    // Found but not recorded (a rollback lost the record): kept, recorded again.
    *places.record.recorded.lock().unwrap() = None;
    assert_eq!(
        places.load().await.unwrap().fingerprint(),
        first.fingerprint()
    );
    assert_eq!(places.seen().1, Some(hex(&first.fingerprint())));
}

/// Losing the identity makes every phone pair again, so the load waits
/// instead of minting while anything says a phone knows the old one.
#[tokio::test]
async fn the_load_never_mints_over_an_identity_the_phones_know() {
    let missing = Places::new();
    missing.pair_a_phone();
    let error = missing.load().await.unwrap_err();
    assert!(
        matches!(unavailable(&error), Some(Unavailability::Missing)),
        "{error}"
    );

    let recorded = Places::new();
    recorded.record.record("ab12").unwrap();
    assert!(matches!(
        unavailable(&recorded.load().await.unwrap_err()),
        Some(Unavailability::Missing)
    ));

    let replaced = Places::new();
    replaced.load().await.unwrap();
    let other = HandoverIdentity::mint("y", Utc::now())
        .unwrap()
        .to_pem()
        .unwrap();
    *replaced.secrets.value.lock().unwrap() = Some(other);
    assert!(matches!(
        unavailable(&replaced.load().await.unwrap_err()),
        Some(Unavailability::Replaced)
    ));

    let locked = Places::new();
    locked.secrets.refuse_reads.store(true, Ordering::SeqCst);
    let error = locked.load().await.unwrap_err();
    assert!(
        matches!(unavailable(&error), Some(Unavailability::Unreadable(_))),
        "{error}"
    );

    let garbage = Places::new();
    *garbage.secrets.value.lock().unwrap() = Some("garbage".to_owned());
    assert!(matches!(
        garbage.load().await.unwrap_err(),
        IdentityError::Malformed(_)
    ));

    for (places, phones) in [(&missing, 1), (&recorded, 0), (&locked, 0), (&garbage, 0)] {
        let (stored, _, _, paired) = places.seen();
        assert!(
            stored.is_none_or(|stored| stored == "garbage"),
            "nothing minted"
        );
        assert_eq!(paired, phones);
    }
}

#[tokio::test]
async fn pair_again_mints_a_new_identity_and_removes_every_phone() {
    let places = Places::new();
    let old = places.load().await.unwrap();
    places.pair_a_phone();
    let new = places.mint_anew().await.unwrap();
    assert_ne!(new.fingerprint(), old.fingerprint());
    assert_eq!(
        places.seen(),
        (
            Some(new.to_pem().unwrap()),
            Some(hex(&new.fingerprint())),
            None,
            0
        )
    );
    assert_eq!(
        places.load().await.unwrap().fingerprint(),
        new.fingerprint()
    );

    // A refused write changes nothing the phones rely on.
    places.pair_a_phone();
    let before = places.seen();
    places.secrets.refuse_writes.store(true, Ordering::SeqCst);
    assert!(matches!(
        places.mint_anew().await.unwrap_err(),
        IdentityError::Secrets(_)
    ));
    places.secrets.refuse_writes.store(false, Ordering::SeqCst);
    assert_eq!(
        places.load().await.unwrap().fingerprint(),
        new.fingerprint()
    );
    assert_eq!(
        places.seen(),
        before,
        "the note of the refused write is dropped"
    );
}

/// Every write is idempotent, so a Pair again that stopped part way is
/// finished by the next load, and nothing is minted.
#[tokio::test]
async fn the_next_load_finishes_a_pair_again_that_stopped_after_its_write() {
    for stop in ["the secret store's answer", "the record"] {
        let places = Places::new();
        places.load().await.unwrap();
        places.pair_a_phone();
        if stop == "the record" {
            places.record.refuse.store(true, Ordering::SeqCst);
        } else {
            places.secrets.keep_and_refuse.store(true, Ordering::SeqCst);
        }
        assert!(places.mint_anew().await.is_err(), "{stop}");
        places.record.refuse.store(false, Ordering::SeqCst);
        places
            .secrets
            .keep_and_refuse
            .store(false, Ordering::SeqCst);
        let (stored, _, next, _) = places.seen();
        let new = HandoverIdentity::from_pem(stored.as_deref().unwrap()).unwrap();
        assert_eq!(
            next,
            Some(hex(&new.fingerprint())),
            "{stop}: the note stays"
        );

        assert_eq!(
            places.load().await.unwrap().fingerprint(),
            new.fingerprint(),
            "{stop}"
        );
        assert_eq!(
            places.seen(),
            (stored, Some(hex(&new.fingerprint())), None, 0),
            "{stop}"
        );
    }
}

// The decision's truth table.

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_790_000_000, 0).unwrap()
}

/// An offset from [`now`]: a sign, a number and a unit (`ms`, `s`, `m`, `h`, `d`).
fn offset(text: &str) -> DateTime<Utc> {
    let sign = if text.starts_with('-') { -1 } else { 1 };
    let rest = text.trim_start_matches(['-', '+']);
    let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap();
    let number: i64 = rest[..digits].parse().unwrap();
    let delta = match &rest[digits..] {
        "ms" => TimeDelta::milliseconds(number),
        "s" => TimeDelta::seconds(number),
        "m" => TimeDelta::minutes(number),
        "h" => TimeDelta::hours(number),
        "d" => TimeDelta::days(number),
        unit => panic!("{text:?}: no unit {unit:?}"),
    };
    now() + delta * sign
}

fn text(value: &Value) -> &str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("no string: {value}"))
}

/// One row: its name, what it observes and what it expects.
fn row(value: &Value) -> (String, Observations, Decision) {
    let name = text(&value["name"]).to_owned();
    let caller = match text(&value["caller"]) {
        "load" => Caller::Load,
        "import" => Caller::Import,
        "move" => Caller::Move,
        other => panic!("{name}: no caller {other:?}"),
    };
    let start = |found: &str| {
        let start = value["starts"].get(found);
        let start = start.unwrap_or_else(|| panic!("{name}: no start for {found}"));
        (!start.is_null()).then(|| offset(text(start)))
    };
    let identities = match &value["found"] {
        Value::String(found) if found == "unreadable" => Identities::Unreadable,
        Value::String(found) if found == "malformed" => Identities::Malformed,
        Value::Array(found) => Identities::Found(
            found
                .iter()
                .map(|fingerprint| Found {
                    fingerprint: text(fingerprint).to_owned(),
                    start: start(text(fingerprint)),
                })
                .collect(),
        ),
        other => panic!("{name}: found is {other}"),
    };
    let record = match &value["record"] {
        Value::Null => Record::EMPTY,
        Value::String(record) if record == "notOpened" => Record::NotOpened,
        Value::String(record) if record == "doesNotParse" => Record::DoesNotParse,
        Value::Object(record) => Record::Read {
            current: record.get("current").map(|value| text(value).to_owned()),
            pending: record.get("pending").map(|value| text(value).to_owned()),
        },
        other => panic!("{name}: record is {other}"),
    };
    let phones = match &value["phones"] {
        Value::Null => Phones::Paired(Vec::new()),
        Value::String(phones) if phones == "unreadable" => Phones::Unreadable,
        Value::Array(phones) => Phones::Paired(phones.iter().map(|at| offset(text(at))).collect()),
        other => panic!("{name}: phones is {other}"),
    };
    let expect = &value["expect"];
    let outcome = if let Some(served) = expect.get("serve") {
        Outcome::Serve(text(served).to_owned())
    } else if expect.get("mint") == Some(&Value::Bool(true)) {
        Outcome::Mint
    } else {
        Outcome::Wait(match text(&expect["wait"]) {
            "recordNotOpened" => Reason::RecordNotOpened,
            "recordDoesNotParse" => Reason::RecordDoesNotParse,
            "secretUnreadable" => Reason::SecretUnreadable,
            "malformed" => Reason::Malformed,
            "phonesUnreadable" => Reason::PhonesUnreadable,
            "missing" => Reason::Missing,
            "replaced" => Reason::Replaced,
            "partlyReplaced" => Reason::PartlyReplaced,
            "clockBehind" => Reason::ClockBehind,
            other => panic!("{name}: no reason {other:?}"),
        })
    };
    let writes = expect["writes"]
        .as_array()
        .map(|writes| {
            writes
                .iter()
                .map(|write| match write {
                    Value::String(write) if write == "dropNote" => Write::DropNote,
                    Value::String(write) if write == "removePhones" => Write::RemovePhones,
                    write => Write::Record(text(&write["record"]).to_owned()),
                })
                .collect()
        })
        .unwrap_or_default();
    let observed = Observations {
        caller,
        identities,
        record,
        phones,
        now: now(),
    };
    (name, observed, Decision { outcome, writes })
}

fn rows() -> Vec<(String, Observations, Decision)> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/identity-decisions.json"
    );
    let table: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    table["rows"].as_array().unwrap().iter().map(row).collect()
}

/// `observed` after `writes` landed.
fn after(observed: &Observations, writes: &[Write]) -> Observations {
    let mut observed = observed.clone();
    for write in writes {
        match (write, &mut observed.record) {
            (Write::DropNote, Record::Read { pending, .. }) => *pending = None,
            (Write::RemovePhones, _) => observed.phones = Phones::Paired(Vec::new()),
            (Write::Record(fingerprint), record) => {
                *record = Record::Read {
                    current: Some(fingerprint.clone()),
                    pending: None,
                };
            }
            (Write::DropNote, _) => panic!("a note dropped from a record that cannot be read"),
        }
    }
    observed
}

/// `found` in every rotation, and reversed in each.
fn orders(found: &[Found]) -> Vec<Vec<Found>> {
    let mut orders = Vec::new();
    for turn in 0..found.len().max(1) {
        let mut order = found.to_vec();
        order.rotate_left(turn);
        orders.push(order.clone());
        order.reverse();
        orders.push(order);
    }
    orders
}

/// Every row decides as the table says, in any order of the identities found.
#[test]
fn every_row_of_the_truth_table_decides_as_it_says() {
    let rows = rows();
    assert!(rows.len() > 90, "{} rows", rows.len());
    for (name, observed, expected) in &rows {
        let Identities::Found(found) = &observed.identities else {
            assert_eq!(decide(observed), *expected, "{name}");
            continue;
        };
        for order in orders(found) {
            let reordered = Observations {
                identities: Identities::Found(order),
                ..observed.clone()
            };
            assert_eq!(decide(&reordered), *expected, "{name}");
        }
    }
}

/// The input space: every caller, and identities, records and phones in
/// every readable and unreadable state around now.
fn every_observation() -> Vec<Observations> {
    let starts = [
        Some(offset("-30d")),
        Some(offset("-10d")),
        Some(offset("+1s")),
        Some(offset("+1d")),
        None,
    ];
    let found = |name: &str, start| Found {
        fingerprint: name.to_owned(),
        start,
    };
    let mut identities = vec![
        Identities::Unreadable,
        Identities::Malformed,
        Identities::Found(Vec::new()),
    ];
    for first in ["A", "B", "C"] {
        let second = if first == "A" { "B" } else { "A" };
        for first_start in starts {
            identities.push(Identities::Found(vec![found(first, first_start)]));
            identities.push(Identities::Found(vec![
                found(first, first_start),
                found(first, first_start),
            ]));
            for second_start in starts {
                identities.push(Identities::Found(vec![
                    found(first, first_start),
                    found(second, second_start),
                ]));
            }
        }
    }
    let mut records = vec![Record::NotOpened, Record::DoesNotParse];
    let maybe = [None, Some("A"), Some("B"), Some("C")];
    for current in maybe {
        for pending in maybe {
            records.push(Record::Read {
                current: current.map(str::to_owned),
                pending: pending.map(str::to_owned),
            });
        }
    }
    let at = |offsets: &[&str]| Phones::Paired(offsets.iter().map(|text| offset(text)).collect());
    let phones = [
        Phones::Unreadable,
        at(&[]),
        at(&["-40d"]),
        at(&["-20d"]),
        at(&["-1d"]),
        at(&["-20d", "-1d"]),
        at(&["-40d", "-1d"]),
        at(&["+1d"]),
        at(&["-10d"]),
        at(&["+500ms"]),
    ];
    let mut every = Vec::new();
    for caller in [Caller::Load, Caller::Import, Caller::Move] {
        for identities in &identities {
            for record in &records {
                for phones in &phones {
                    every.push(Observations {
                        caller,
                        identities: identities.clone(),
                        record: record.clone(),
                        phones: phones.clone(),
                        now: now(),
                    });
                }
            }
        }
    }
    every
}

/// Over the whole input space:
/// - nothing mints silently: only the load mints, and only with no identity,
///   nothing recorded, no phone paired and a readable record (a note beside
///   it is dropped first);
/// - no pairing the phones know is lost: phones go only where the load
///   finishes a Pair again, only the served fingerprint is recorded, and
///   nothing is served from nothing;
/// - no decision destroys a stored identity: the import writes nothing, the
///   move records at most the file's identity;
/// - a stop after any write decides the same next time;
/// - the order the identities were found in never decides.
#[test]
fn every_decision_keeps_the_rules_of_the_truth_table() {
    for observed in every_observation() {
        let decided = decide(&observed);
        let found: &[Found] = match &observed.identities {
            Identities::Found(found) => found,
            _ => &[],
        };
        let (current, pending) = match &observed.record {
            Record::Read { current, pending } => (current.as_deref(), pending.as_deref()),
            _ => (None, None),
        };

        if decided.outcome == Outcome::Mint {
            assert_eq!(observed.caller, Caller::Load, "{observed:?}");
            assert_eq!(
                observed.identities,
                Identities::Found(Vec::new()),
                "{observed:?}"
            );
            assert_eq!(observed.phones, Phones::Paired(Vec::new()), "{observed:?}");
            assert!(
                matches!(observed.record, Record::Read { current: None, .. }),
                "{observed:?}"
            );
            let writes = if pending.is_some() {
                vec![Write::DropNote]
            } else {
                Vec::new()
            };
            assert_eq!(decided.writes, writes, "{observed:?}");
        }
        for write in &decided.writes {
            assert!(
                matches!(observed.record, Record::Read { .. }),
                "{observed:?}"
            );
            match write {
                Write::RemovePhones => {
                    assert_eq!(observed.caller, Caller::Load, "{observed:?}");
                    let [identity] = found else {
                        panic!("phones removed over {found:?}: {observed:?}");
                    };
                    assert_eq!(pending, Some(identity.fingerprint.as_str()), "{observed:?}");
                    assert_eq!(
                        decided.outcome,
                        Outcome::Serve(identity.fingerprint.clone()),
                        "{observed:?}"
                    );
                }
                Write::Record(fingerprint) => {
                    assert_eq!(
                        decided.outcome,
                        Outcome::Serve(fingerprint.clone()),
                        "{observed:?}"
                    );
                }
                Write::DropNote => assert!(pending.is_some(), "{observed:?}"),
            }
        }
        if let Outcome::Serve(served) = &decided.outcome {
            let recorded = current == Some(served.as_str());
            assert!(
                found.iter().any(|identity| identity.fingerprint == *served)
                    || (observed.caller == Caller::Move && recorded),
                "served from nothing: {observed:?}"
            );
        }
        match (observed.caller, decided.writes.as_slice()) {
            (Caller::Load, _) | (_, []) => {}
            (Caller::Move, [Write::Record(fingerprint)]) => assert!(
                matches!(found, [file] if file.fingerprint == *fingerprint),
                "the move recorded another than the file's: {observed:?}"
            ),
            (caller, other) => panic!("{caller:?} writes {other:?}: {observed:?}"),
        }

        let end = after(&observed, &decided.writes);
        for landed in 0..=decided.writes.len() {
            let stopped = after(&observed, &decided.writes[..landed]);
            let again = decide(&stopped);
            assert_eq!(again.outcome, decided.outcome, "{landed} of {observed:?}");
            assert_eq!(
                after(&stopped, &again.writes),
                end,
                "{landed} of {observed:?}"
            );
        }

        if let Identities::Found(found) = &observed.identities {
            for order in orders(found) {
                let reordered = Observations {
                    identities: Identities::Found(order),
                    ..observed.clone()
                };
                assert_eq!(decide(&reordered), decided, "{observed:?}");
            }
        }
    }
}

/// The kind of a decision for `caller`: its outcome and its writes.
fn signature(caller: Caller, decided: &Decision) -> String {
    let outcome = match &decided.outcome {
        Outcome::Serve(_) => "serve".to_owned(),
        Outcome::Mint => "mint".to_owned(),
        Outcome::Wait(reason) => format!("wait {reason:?}"),
    };
    let writes: Vec<String> = decided
        .writes
        .iter()
        .map(|write| match write {
            Write::Record(_) => "record".to_owned(),
            other => format!("{other:?}"),
        })
        .collect();
    format!("{caller:?} {outcome} [{}]", writes.join(", "))
}

/// Every kind of decision the input space reaches has a row, except two no
/// caller observes, and every row's kind is reached.
#[test]
fn the_table_has_a_row_for_every_kind_of_decision_reached() {
    let unobserved = [
        "Import wait SecretUnreadable []",
        "Move wait SecretUnreadable []",
    ];
    let tabled: BTreeSet<String> = rows()
        .iter()
        .map(|(_, observed, expected)| signature(observed.caller, expected))
        .collect();
    let reached: BTreeSet<String> = every_observation()
        .iter()
        .map(|observed| signature(observed.caller, &decide(observed)))
        .collect();
    for kind in &reached {
        assert!(
            tabled.contains(kind) || unobserved.contains(&kind.as_str()),
            "no row for {kind}"
        );
    }
    for kind in unobserved {
        assert!(reached.contains(kind), "{kind} is no longer reached");
        assert!(!tabled.contains(kind), "{kind} has a row after all");
    }
    for kind in &tabled {
        assert!(reached.contains(kind), "{kind} is outside the input space");
    }
}
