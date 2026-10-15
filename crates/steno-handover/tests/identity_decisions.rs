//! The handover identity's decision against its truth table,
//! `tests/fixtures/identity-decisions.json` (its `about` says how a row
//! reads), and three properties over an enumerated input space: nothing
//! mints silently, no pairing the phones know is lost, and a stop after
//! any write decides the same at the next read.

#![allow(clippy::too_many_lines)]

use std::collections::BTreeMap;

use chrono::{DateTime, TimeDelta, Utc};
use serde_json::Value;
use steno_handover::identity::decision::{
    Caller, Decision, Found, Identities, Observations, Outcome, Phones, Reason, Record, Write,
    decide,
};

/// The decision's now in every row and case.
fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_790_000_000, 0).unwrap()
}

/// An offset from [`now`]: an optional sign, a whole number and a unit
/// (`ms`, `s`, `m`, `h` or `d`).
fn offset(text: &str) -> DateTime<Utc> {
    let (sign, rest) = match text.as_bytes().first() {
        Some(b'-') => (-1, &text[1..]),
        Some(b'+') => (1, &text[1..]),
        _ => (1, text),
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
                "name", "caller", "found", "certs", "record", "phones", "expect"
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
    let certs = fields.get("certs").map_or_else(BTreeMap::new, |certs| {
        certs
            .as_object()
            .unwrap_or_else(|| panic!("{name}: certs is no object"))
            .iter()
            .map(|(cert, start)| {
                let start = (!start.is_null()).then(|| offset(text(start, "a start")));
                (cert.clone(), start)
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
                    let start = *certs
                        .get(&fingerprint)
                        .unwrap_or_else(|| panic!("{name}: no start for {fingerprint}"));
                    Found { fingerprint, start }
                })
                .collect(),
        ),
        other => panic!("{name}: found is {other}"),
    };
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
                .map(|phone| offset(text(&phone["pairedAt"], "pairedAt")))
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
    let table: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
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
    assert!(rows.len() >= 80, "{} rows", rows.len());
    let mut names = std::collections::HashSet::new();
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

/// Every caller, outcome and reason has a row.
#[test]
fn the_table_covers_every_caller_outcome_and_reason() {
    let rows = rows();
    let mut seen = std::collections::HashSet::new();
    for row in &rows {
        seen.insert(format!("{:?}", row.observed.caller));
        seen.insert(match &row.expected.outcome {
            Outcome::Serve(_) => "serve".to_owned(),
            Outcome::Mint => "mint".to_owned(),
            Outcome::Wait(reason) => format!("{reason:?}"),
        });
        for write in &row.expected.writes {
            seen.insert(match write {
                Write::Record(_) => "record".to_owned(),
                other => format!("{other:?}"),
            });
        }
    }
    for wanted in [
        "Load",
        "Import",
        "Move",
        "serve",
        "mint",
        "RecordNotOpened",
        "RecordDoesNotParse",
        "SecretUnreadable",
        "Malformed",
        "PhonesUnreadable",
        "Missing",
        "Replaced",
        "PartlyReplaced",
        "ClockBehind",
        "DropNote",
        "RemovePhones",
        "record",
    ] {
        assert!(seen.contains(wanted), "no row for {wanted}");
    }
}

/// The input space the properties run over: every caller; identities that
/// cannot be read, do not parse, none, one or two of three fingerprints
/// (a copy included), each starting long before, before, at a second
/// after now, after now, or unreadable; every record; phones that cannot
/// be read, none, or paired at times around the starts.
fn every_observation() -> Vec<Observations> {
    let starts = [
        Some(offset("-30d")),
        Some(offset("-10d")),
        Some(offset("+1s")),
        Some(offset("+1d")),
        None,
    ];
    let names = ["A", "B", "C"];
    let mut identities = vec![Identities::Unreadable, Identities::Malformed];
    identities.push(Identities::Found(Vec::new()));
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
            let found = match &observed.identities {
                Identities::Found(found) => found.as_slice(),
                _ => &[],
            };
            assert!(
                found.iter().any(|identity| identity.fingerprint == *served)
                    || (observed.caller == Caller::Move && recorded),
                "served from nothing: {observed:?}"
            );
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
/// read, and its writes end where the whole list ends.
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
