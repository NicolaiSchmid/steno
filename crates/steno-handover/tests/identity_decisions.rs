//! The handover identity's decision against its truth table,
//! `crates/steno-handover/tests/fixtures/identity-decisions.json` (its
//! `about` says how a row reads), a check of the runner's own refusals, a
//! check that the table has a row for every kind of decision the input
//! space reaches, and properties over that space:
//! - nothing mints silently;
//! - no pairing the phones know is lost;
//! - no decision destroys a stored identity;
//! - a stop after any write decides the same at the next read;
//! - the order the identities were found in never decides.

#![allow(clippy::too_many_lines)]

use std::collections::{BTreeMap, BTreeSet, HashSet};

use chrono::{DateTime, TimeDelta, Utc};
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use steno_handover::identity::decision::{
    Caller, Decision, Found, Identities, Observations, Outcome, Phones, Reason, Record, Write,
    decide,
};

/// The decision's now in every row and case.
fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_790_000_000, 0).unwrap()
}

/// An offset from [`now`]: a sign, a whole number and a unit (`ms`, `s`,
/// `m`, `h` or `d`).
fn offset(text: &str) -> DateTime<Utc> {
    let (sign, rest) = match text.as_bytes().first() {
        Some(b'-') => (-1, &text[1..]),
        Some(b'+') => (1, &text[1..]),
        _ => panic!("{text:?} has no sign"),
    };
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let (number, unit) = rest.split_at(digits);
    let number: i64 = number
        .parse()
        .unwrap_or_else(|_| panic!("{text:?} is no offset"));
    let delta = match unit {
        "ms" => TimeDelta::milliseconds(number),
        "s" => TimeDelta::seconds(number),
        "m" => TimeDelta::minutes(number),
        "h" => TimeDelta::hours(number),
        "d" => TimeDelta::days(number),
        _ => panic!("{text:?} has no unit this table knows"),
    };
    now() + delta * sign
}

/// A JSON value as `serde_json` reads it, except that an object naming a
/// key twice is refused: `serde_json` keeps the last, so a row could
/// silently state a field twice.
struct Strict(Value);

impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor).map(Strict)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Value, E> {
        Ok(value.into())
    }

    fn visit_u64<E>(self, value: u64) -> Result<Value, E> {
        Ok(value.into())
    }

    fn visit_f64<E>(self, value: f64) -> Result<Value, E> {
        Ok(value.into())
    }

    fn visit_str<E>(self, value: &str) -> Result<Value, E> {
        Ok(value.into())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(Strict(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut object = serde_json::Map::new();
        while let Some((key, Strict(value))) = map.next_entry::<String, Strict>()? {
            if object.contains_key(&key) {
                return Err(serde::de::Error::custom(format!("the key {key:?} twice")));
            }
            object.insert(key, value);
        }
        Ok(Value::Object(object))
    }
}

fn text<'a>(value: &'a Value, what: &str) -> &'a str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{what} is no string: {value}"))
}

/// One row of the table: what it observes and what it expects.
struct Row {
    name: String,
    observed: Observations,
    expected: Decision,
}

fn row(value: &Value) -> Row {
    let fields = value.as_object().expect("a row is an object");
    let name = text(&fields["name"], "name").to_owned();
    for key in fields.keys() {
        assert!(
            [
                "name", "caller", "found", "starts", "record", "phones", "expect"
            ]
            .contains(&key.as_str()),
            "{name}: no field {key:?} in this table"
        );
    }
    let caller = match text(&fields["caller"], "caller") {
        "load" => Caller::Load,
        "import" => Caller::Import,
        "move" => Caller::Move,
        other => panic!("{name}: no caller {other:?}"),
    };
    let starts = fields.get("starts").map_or_else(BTreeMap::new, |starts| {
        starts
            .as_object()
            .unwrap_or_else(|| panic!("{name}: starts is no object"))
            .iter()
            .map(|(found, start)| {
                let start = (!start.is_null()).then(|| offset(text(start, "a start")));
                (found.clone(), start)
            })
            .collect()
    });
    let identities = match &fields["found"] {
        Value::String(found) if found == "unreadable" => Identities::Unreadable,
        Value::String(found) if found == "malformed" => Identities::Malformed,
        Value::Array(found) => Identities::Found(
            found
                .iter()
                .map(|name_of| {
                    let fingerprint = text(name_of, "a name found").to_owned();
                    let start = *starts
                        .get(&fingerprint)
                        .unwrap_or_else(|| panic!("{name}: no start for {fingerprint}"));
                    Found { fingerprint, start }
                })
                .collect(),
        ),
        other => panic!("{name}: found is {other}"),
    };
    for start_of in starts.keys() {
        let named = matches!(&identities, Identities::Found(found)
            if found.iter().any(|identity| identity.fingerprint == *start_of));
        assert!(named, "{name}: a start for {start_of}, which is not found");
    }
    let record = match fields.get("record") {
        None => Record::EMPTY,
        Some(Value::String(record)) if record == "notOpened" => Record::NotOpened,
        Some(Value::String(record)) if record == "doesNotParse" => Record::DoesNotParse,
        Some(Value::Object(record)) => {
            for key in record.keys() {
                assert!(
                    key == "current" || key == "pending",
                    "{name}: no record key {key:?}"
                );
            }
            let named = |key| record.get(key).map(|value| text(value, key).to_owned());
            Record::Read {
                current: named("current"),
                pending: named("pending"),
            }
        }
        Some(other) => panic!("{name}: record is {other}"),
    };
    let phones = match fields.get("phones") {
        None => Phones::Paired(Vec::new()),
        Some(Value::String(phones)) if phones == "unreadable" => Phones::Unreadable,
        Some(Value::Array(phones)) => Phones::Paired(
            phones
                .iter()
                .map(|phone| offset(text(phone, "a pairing time")))
                .collect(),
        ),
        Some(other) => panic!("{name}: phones is {other}"),
    };
    let expect = fields["expect"]
        .as_object()
        .unwrap_or_else(|| panic!("{name}: expect is no object"));
    let outcome = match (expect.get("serve"), expect.get("mint"), expect.get("wait")) {
        (Some(served), None, None) => Outcome::Serve(text(served, "serve").to_owned()),
        (None, Some(Value::Bool(true)), None) => Outcome::Mint,
        (None, None, Some(reason)) => Outcome::Wait(reason_named(text(reason, "wait"), &name)),
        _ => panic!("{name}: expect names one of serve, mint and wait"),
    };
    for key in expect.keys() {
        assert!(
            ["serve", "mint", "wait", "writes"].contains(&key.as_str()),
            "{name}: no expect key {key:?}"
        );
    }
    let writes = expect.get("writes").map_or_else(Vec::new, |writes| {
        writes
            .as_array()
            .unwrap_or_else(|| panic!("{name}: writes is no list"))
            .iter()
            .map(|write| match write {
                Value::String(write) if write == "dropNote" => Write::DropNote,
                Value::String(write) if write == "removePhones" => Write::RemovePhones,
                Value::Object(write) if write.len() == 1 && write.contains_key("record") => {
                    Write::Record(text(&write["record"], "record").to_owned())
                }
                other => panic!("{name}: no write {other}"),
            })
            .collect()
    });
    Row {
        observed: Observations {
            caller,
            identities,
            record,
            phones,
            now: now(),
        },
        expected: Decision { outcome, writes },
        name,
    }
}

fn reason_named(reason: &str, name: &str) -> Reason {
    match reason {
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
    }
}

fn rows() -> Vec<Row> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/identity-decisions.json"
    );
    table(&std::fs::read(path).unwrap())
}

/// The rows of the table in `json`.
fn table(json: &[u8]) -> Vec<Row> {
    let Strict(table) = serde_json::from_slice(json).unwrap();
    let keys: Vec<_> = table
        .as_object()
        .expect("the table is an object")
        .keys()
        .collect();
    assert_eq!(keys, ["about", "rows"], "the table's keys");
    table["rows"]
        .as_array()
        .expect("the table has rows")
        .iter()
        .map(row)
        .collect()
}

/// `found` in every rotation and reversed in each.
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

/// Every row decides as the table says, whatever the order the
/// identities were found in.
#[test]
fn every_row_of_the_table_decides_as_it_says() {
    let rows = rows();
    assert_eq!(rows.len(), 95, "a row added or removed changes this count");
    let mut names = HashSet::new();
    for row in &rows {
        assert!(names.insert(row.name.clone()), "{}: twice", row.name);
        let variants = match &row.observed.identities {
            Identities::Found(found) => orders(found)
                .into_iter()
                .map(|order| Observations {
                    identities: Identities::Found(order),
                    ..row.observed.clone()
                })
                .collect(),
            _ => vec![row.observed.clone()],
        };
        for observed in variants {
            assert_eq!(
                decide(&observed),
                row.expected,
                "{}: {observed:?}",
                row.name
            );
        }
    }
}

/// The runner refuses what would let a row say less than it seems to: a
/// key twice, an offset without a sign, a start for a name not found, and
/// a key beside the table's two.
#[test]
fn the_runner_refuses_a_key_twice_an_unsigned_offset_a_stray_start_and_a_stray_key() {
    use std::panic::catch_unwind;
    let twice = serde_json::from_str::<Strict>(r#"{"phones": "unreadable", "phones": []}"#);
    assert!(twice.is_err(), "a key twice was read");
    let nested = serde_json::from_str::<Strict>(r#"{"rows": [{"a": 1, "a": 1}]}"#);
    assert!(nested.is_err(), "a key twice inside a row was read");

    assert!(
        catch_unwind(|| offset("10d")).is_err(),
        "an offset without a sign was read"
    );
    assert_eq!(offset("+10d"), now() + TimeDelta::days(10));
    assert_eq!(offset("-10d"), now() - TimeDelta::days(10));

    let one = |starts: Value| {
        serde_json::json!({
            "name": "x", "caller": "load", "found": ["A"], "starts": starts,
            "expect": {"serve": "A"},
        })
    };
    let stray = one(serde_json::json!({"A": "-1d", "Z": "-1d"}));
    assert!(
        catch_unwind(|| row(&stray)).is_err(),
        "a start for a name not found was read"
    );
    let named = row(&one(serde_json::json!({"A": "-1d"})));
    assert_eq!(named.expected.outcome, Outcome::Serve("A".to_owned()));

    assert!(
        catch_unwind(|| table(br#"{"about": [], "rows": [], "more": []}"#)).is_err(),
        "a key beside about and rows was read"
    );
    assert!(table(br#"{"about": [], "rows": []}"#).is_empty());
}

/// The kind of a decision for `caller`: its outcome (the reason of a
/// wait) and its writes, with the names served and recorded left out.
fn signature(caller: Caller, decided: &Decision) -> String {
    let outcome = match &decided.outcome {
        Outcome::Serve(_) => "serve".to_owned(),
        Outcome::Mint => "mint".to_owned(),
        Outcome::Wait(reason) => format!("wait {reason:?}"),
    };
    let writes: Vec<_> = decided
        .writes
        .iter()
        .map(|write| match write {
            Write::Record(_) => "record".to_owned(),
            other => format!("{other:?}"),
        })
        .collect();
    format!("{caller:?} {outcome} [{}]", writes.join(", "))
}

/// Every kind of decision the input space reaches, for each caller, has a
/// row, except two that no caller observes: the import and the move never
/// pass identities that could not be read (the import reads certificates
/// it found, the move the file it moves).
#[test]
fn the_table_has_a_row_for_every_kind_of_decision_reached() {
    let unobserved = [
        "Import wait SecretUnreadable []",
        "Move wait SecretUnreadable []",
    ];
    let tabled: BTreeSet<String> = rows()
        .iter()
        .map(|row| signature(row.observed.caller, &row.expected))
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

/// The input space the properties run over: every caller; identities that
/// cannot be read, do not parse, none, one or two of three fingerprints
/// (a copy included), each starting long before, before, at a second
/// after now, after now, or unreadable; every record; phones that cannot
/// be read, none, or paired at times around the starts, at a start, and
/// within the clock's slack after now.
fn every_observation() -> Vec<Observations> {
    let starts = [
        Some(offset("-30d")),
        Some(offset("-10d")),
        Some(offset("+1s")),
        Some(offset("+1d")),
        None,
    ];
    let names = ["A", "B", "C"];
    let mut identities = vec![
        Identities::Unreadable,
        Identities::Malformed,
        Identities::Found(Vec::new()),
    ];
    let found = |name: &str, start| Found {
        fingerprint: name.to_owned(),
        start,
    };
    for first in names {
        for first_start in starts {
            identities.push(Identities::Found(vec![found(first, first_start)]));
            identities.push(Identities::Found(vec![
                found(first, first_start),
                found(first, first_start),
            ]));
            for second_start in starts {
                identities.push(Identities::Found(vec![
                    found(first, first_start),
                    found(if first == "A" { "B" } else { "A" }, second_start),
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

/// The identities found in `observed`, none where they could not be read.
fn found(observed: &Observations) -> &[Found] {
    match &observed.identities {
        Identities::Found(found) => found,
        _ => &[],
    }
}

fn pending(record: &Record) -> Option<&str> {
    match record {
        Record::Read { pending, .. } => pending.as_deref(),
        _ => None,
    }
}

/// Nothing mints silently: only the load mints, and only with no identity,
/// nothing recorded, no phone paired and a record it could read; a note
/// beside it (a Pair again that wrote nothing) is dropped first.
#[test]
fn nothing_mints_silently() {
    for observed in every_observation() {
        let decided = decide(&observed);
        if decided.outcome != Outcome::Mint {
            continue;
        }
        assert_eq!(observed.caller, Caller::Load, "{observed:?}");
        assert_eq!(
            observed.identities,
            Identities::Found(Vec::new()),
            "{observed:?}"
        );
        assert_eq!(observed.phones, Phones::Paired(Vec::new()), "{observed:?}");
        let Record::Read {
            current: None,
            pending,
        } = &observed.record
        else {
            panic!("a mint over a record: {observed:?}");
        };
        let writes = if pending.is_some() {
            vec![Write::DropNote]
        } else {
            Vec::new()
        };
        assert_eq!(decided.writes, writes, "{observed:?}");
    }
}

/// No pairing the phones know is lost: the phones go only where the load
/// finishes a Pair again, the stored identity being the noted pending one,
/// and a fingerprint is recorded only for the identity served, never over
/// a record that cannot be read.
#[test]
fn no_pairing_the_phones_know_is_lost() {
    for observed in every_observation() {
        let decided = decide(&observed);
        for write in &decided.writes {
            assert!(
                matches!(observed.record, Record::Read { .. }),
                "a write over a record that cannot be read: {observed:?}"
            );
            match write {
                Write::RemovePhones => {
                    let Identities::Found(found) = &observed.identities else {
                        panic!("{observed:?}");
                    };
                    assert_eq!(observed.caller, Caller::Load, "{observed:?}");
                    assert_eq!(found.len(), 1, "{observed:?}");
                    assert_eq!(
                        pending(&observed.record),
                        Some(found[0].fingerprint.as_str()),
                        "{observed:?}"
                    );
                    assert_eq!(
                        decided.outcome,
                        Outcome::Serve(found[0].fingerprint.clone()),
                        "{observed:?}"
                    );
                }
                Write::Record(fingerprint) => assert_eq!(
                    decided.outcome,
                    Outcome::Serve(fingerprint.clone()),
                    "{observed:?}"
                ),
                Write::DropNote => assert!(pending(&observed.record).is_some(), "{observed:?}"),
            }
        }
        // A served identity is one that was found, or for the move the one
        // already recorded.
        if let Outcome::Serve(served) = &decided.outcome {
            let recorded = matches!(
                &observed.record,
                Record::Read { current: Some(current), .. } if current == served
            );
            assert!(
                found(&observed)
                    .iter()
                    .any(|identity| identity.fingerprint == *served)
                    || (observed.caller == Caller::Move && recorded),
                "served from nothing: {observed:?}"
            );
        }
    }
}

/// No decision destroys a stored identity. A [`Write`] has no write of an
/// identity, by design; a mint is decided only where no identity was found
/// ([`nothing_mints_silently`]) and the phones go only in the load's
/// finished Pair again ([`no_pairing_the_phones_know_is_lost`]). On top of
/// those, the import writes nothing, and the move records at most the one
/// identity it found (the file's) and drops no note and no phone. Two
/// writes outside the decision replace a stored identity, and are the
/// named exceptions: Pair again's own mint (the user's confirmed choice,
/// which removes every phone) and the Mac import's store over a desktop-id
/// build's identity (S6 of `.plans/2026-10-07-stable-promotion.md`).
/// `steno-services`' `no_identity_is_ever_destroyed_by_the_move` runs the
/// move itself over a fake Secret Service.
#[test]
fn no_decision_destroys_a_stored_identity() {
    for observed in every_observation() {
        let decided = decide(&observed);
        match (observed.caller, decided.writes.as_slice()) {
            (Caller::Load, _) | (_, []) => {}
            (Caller::Move, [Write::Record(fingerprint)]) => assert!(
                matches!(found(&observed), [file] if file.fingerprint == *fingerprint),
                "the move recorded another than the file's: {observed:?}"
            ),
            (caller, other) => panic!("{caller:?} writes {other:?}: {observed:?}"),
        }
    }
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

/// A stop after any prefix of the writes decides the same at the next
/// read, and its writes end where the whole list ends. The callers' own
/// writes after the decision (the mint's secret, then its record; the
/// import's store, marker and flag; Pair again's steps) are outside it:
/// `tests/identity.rs` and `steno-services`' tests stop each of them.
#[test]
fn a_stop_after_any_write_decides_the_same_next_time() {
    for observed in every_observation() {
        let decided = decide(&observed);
        let end = after(&observed, &decided.writes);
        for landed in 0..decided.writes.len() {
            let stopped = after(&observed, &decided.writes[..landed]);
            let again = decide(&stopped);
            // The move's outcome names what is recorded once it pinned.
            assert_eq!(again.outcome, decided.outcome, "{landed} of {observed:?}");
            assert_eq!(
                after(&stopped, &again.writes),
                end,
                "{landed} of {observed:?}"
            );
        }
        // And the end decides the same, with nothing more to write except
        // the finished Pair again's own (idempotent) writes.
        let settled = decide(&end);
        assert_eq!(settled.outcome, decided.outcome, "{observed:?}");
        assert_eq!(after(&end, &settled.writes), end, "{observed:?}");
    }
}

/// The order the identities were found in never decides.
#[test]
fn the_order_found_never_decides() {
    for observed in every_observation() {
        let Identities::Found(found) = &observed.identities else {
            continue;
        };
        let decided = decide(&observed);
        for order in orders(found) {
            let reordered = Observations {
                identities: Identities::Found(order),
                ..observed.clone()
            };
            assert_eq!(decide(&reordered), decided, "{observed:?}");
        }
    }
}
