//! The handover identity's one decision: from what the reads found, serve
//! an identity, mint one, or wait, with the writes that go with it. It reads
//! nothing and asks no clock. The truth table in
//! `tests/fixtures/identity-decisions.json` states the rule; a new case is a
//! new row there. No decision destroys a stored identity.

use chrono::{DateTime, TimeDelta, Utc};

/// How far after now a start or a pairing may be before an identity found
/// with nothing recorded waits for the clock. Pairing times are rounded to
/// the millisecond, so a second is a round margin.
pub const CLOCK_SLACK: TimeDelta = TimeDelta::seconds(1);

/// Who asks: where the callers differ, the decision differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    /// The handover's load of its identity.
    Load,
    /// The Swift import, choosing among the certificates it found; it does not
    /// read the record.
    Import,
    /// The Secret Service move, pinning the identity of the file it moves.
    Move,
}

/// An identity found: its fingerprint (lowercase hex SHA-256 of the leaf
/// DER) and its certificate's start, `None` when unreadable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub fingerprint: String,
    pub start: Option<DateTime<Utc>>,
}

/// What the place an identity is kept in holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identities {
    /// It could not be read (a locked keyring).
    Unreadable,
    /// What it holds does not parse as an identity.
    Malformed,
    /// The identities in it: none, one, or for the import several.
    Found(Vec<Found>),
}

/// The record of the identity's fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// The file cannot be opened (its permissions, a disk error).
    NotOpened,
    /// The file does not parse.
    DoesNotParse,
    /// The recorded fingerprint and the note of an unfinished Pair again.
    Read {
        current: Option<String>,
        pending: Option<String>,
    },
}

impl Record {
    /// A record with nothing in it.
    pub const EMPTY: Record = Record::Read {
        current: None,
        pending: None,
    };
}

/// The paired phones, by the time each paired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phones {
    /// The database could not list them.
    Unreadable,
    Paired(Vec<DateTime<Utc>>),
}

/// What the decision decides from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observations {
    pub caller: Caller,
    pub identities: Identities,
    pub record: Record,
    pub phones: Phones,
    /// The clock at the read.
    pub now: DateTime<Utc>,
}

/// What to do, and what to write first, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub outcome: Outcome,
    /// Each write is idempotent, so a stop after any of them decides the same
    /// at the next read.
    pub writes: Vec<Write>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The identity with this fingerprint: served, brought over, or left
    /// recorded.
    Serve(String),
    /// Mint and store a new identity. Only the load mints, and only with no
    /// identity, nothing recorded, no phone paired and a readable record.
    Mint,
    /// Neither. For the move: nothing is pinned, and the next load decides.
    Wait(Reason),
}

/// Why the decision waits: an [`Unavailability`](super::Unavailability), or
/// the read that failed, whose error the caller holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The record cannot be opened.
    RecordNotOpened,
    /// The record does not parse.
    RecordDoesNotParse,
    /// The secret store could not be read.
    SecretUnreadable,
    /// An identity, or a certificate's start, does not parse.
    Malformed,
    /// The paired phones could not be read.
    PhonesUnreadable,
    /// [`Unavailability::Missing`](super::Unavailability::Missing).
    Missing,
    /// [`Unavailability::Replaced`](super::Unavailability::Replaced).
    Replaced,
    /// [`Unavailability::PartlyReplaced`](super::Unavailability::PartlyReplaced).
    PartlyReplaced,
    /// [`Unavailability::ClockBehind`](super::Unavailability::ClockBehind).
    ClockBehind,
}

/// One write, applied in the order given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Write {
    /// Drop the note of a Pair again that wrote nothing.
    DropNote,
    /// Remove every paired phone, to finish a Pair again.
    RemovePhones,
    /// Record this fingerprint, which drops the note in the same write.
    Record(String),
}

/// The decision over `observed`.
///
/// ```
/// use chrono::{TimeDelta, Utc};
/// use steno_handover::identity::decision::{
///     Caller, Found, Identities, Observations, Outcome, Phones, Reason, Record, decide,
/// };
/// let now = Utc::now();
/// // Found with nothing recorded; the one phone paired before it started.
/// let decided = decide(&Observations {
///     caller: Caller::Load,
///     identities: Identities::Found(vec![Found {
///         fingerprint: "a".into(),
///         start: Some(now - TimeDelta::days(10)),
///     }]),
///     record: Record::EMPTY,
///     phones: Phones::Paired(vec![now - TimeDelta::days(20)]),
///     now,
/// });
/// assert_eq!(decided.outcome, Outcome::Wait(Reason::Replaced));
/// assert!(decided.writes.is_empty());
/// ```
#[must_use]
pub fn decide(observed: &Observations) -> Decision {
    match observed.caller {
        Caller::Load => load(observed),
        Caller::Import => import(observed),
        // The move pins the file's identity.
        Caller::Move => pin(observed),
    }
}

fn wait(reason: Reason) -> Decision {
    Decision {
        outcome: Outcome::Wait(reason),
        writes: Vec::new(),
    }
}

/// The identities found, or the wait for a place that could not be read or
/// held none that parses.
fn identities_found(identities: &Identities) -> Result<&[Found], Decision> {
    match identities {
        Identities::Unreadable => Err(wait(Reason::SecretUnreadable)),
        Identities::Malformed => Err(wait(Reason::Malformed)),
        Identities::Found(found) => Ok(found),
    }
}

/// The note's drop, where a note is left.
fn drop_note(pending: Option<&str>) -> Vec<Write> {
    pending.map(|_| Write::DropNote).into_iter().collect()
}

fn serve(fingerprint: &str, writes: Vec<Write>) -> Decision {
    Decision {
        outcome: Outcome::Serve(fingerprint.to_owned()),
        writes,
    }
}

fn load(observed: &Observations) -> Decision {
    let (current, pending) = match &observed.record {
        Record::NotOpened => return wait(Reason::RecordNotOpened),
        Record::DoesNotParse => return wait(Reason::RecordDoesNotParse),
        Record::Read { current, pending } => (current.as_deref(), pending.as_deref()),
    };
    let found = match identities_found(&observed.identities) {
        Ok(found) => found,
        Err(waiting) => return waiting,
    };
    let identity = match found {
        [] => return no_identity(current, pending, &observed.phones),
        [identity] => identity,
        // The secret store holds one identity at most.
        _ => return wait(Reason::Replaced),
    };
    let fingerprint = identity.fingerprint.as_str();
    if pending == Some(fingerprint) {
        // A Pair again whose new identity was written: finish it.
        if observed.phones == Phones::Unreadable {
            return wait(Reason::PhonesUnreadable);
        }
        return serve(
            fingerprint,
            vec![Write::RemovePhones, Write::Record(fingerprint.to_owned())],
        );
    }
    if current == Some(fingerprint) {
        // The recorded identity; a note beside it wrote nothing.
        return serve(fingerprint, drop_note(pending));
    }
    if current.is_some() || pending.is_some() {
        return wait(Reason::Replaced);
    }
    match judge(found, &observed.phones, observed.now) {
        Ok(_) => serve(fingerprint, vec![Write::Record(fingerprint.to_owned())]),
        Err(reason) => wait(reason),
    }
}

/// The load with no identity found: a leftover note goes, then a mint only
/// with nothing recorded and no phone paired.
fn no_identity(current: Option<&str>, pending: Option<&str>, phones: &Phones) -> Decision {
    let outcome = match (current, phones) {
        (Some(_), _) => Outcome::Wait(Reason::Missing),
        (None, Phones::Unreadable) => Outcome::Wait(Reason::PhonesUnreadable),
        (None, Phones::Paired(paired)) if !paired.is_empty() => Outcome::Wait(Reason::Missing),
        (None, Phones::Paired(_)) => Outcome::Mint,
    };
    Decision {
        outcome,
        writes: drop_note(pending),
    }
}

fn import(observed: &Observations) -> Decision {
    let found = match identities_found(&observed.identities) {
        Ok(found) => found,
        Err(waiting) => return waiting,
    };
    // Copies of one certificate (one identity in two keychains) count once.
    let mut seen = std::collections::HashSet::new();
    let mut distinct = found.to_vec();
    distinct.retain(|identity| seen.insert(identity.fingerprint.clone()));
    match judge(&distinct, &observed.phones, observed.now) {
        Ok(index) => serve(&distinct[index].fingerprint, Vec::new()),
        Err(reason) => wait(reason),
    }
}

/// The move pins the file's identity where nothing is recorded, unless no
/// paired phone can know it. Where the rule cannot tell, it pins, and the
/// load then waits rather than adopting the service's identity.
fn pin(observed: &Observations) -> Decision {
    match &observed.record {
        Record::NotOpened => return wait(Reason::RecordNotOpened),
        Record::DoesNotParse => return wait(Reason::RecordDoesNotParse),
        Record::Read {
            current: Some(current),
            ..
        } => return serve(current, Vec::new()),
        Record::Read { current: None, .. } => {}
    }
    let found = match identities_found(&observed.identities) {
        Ok(found) => found,
        Err(waiting) => return waiting,
    };
    let [identity] = found else {
        return wait(Reason::Replaced);
    };
    match judge(found, &observed.phones, observed.now) {
        Err(Reason::Replaced) => wait(Reason::Replaced),
        _ => serve(
            &identity.fingerprint,
            vec![Write::Record(identity.fingerprint.clone())],
        ),
    }
}

/// The pairing-time rule: a phone knows the newest identity whose start is
/// at or before its pairing. Returns the index in `found` every paired phone
/// knows at `now`, or why there is none; equal starts have no newest.
fn judge(found: &[Found], phones: &Phones, now: DateTime<Utc>) -> Result<usize, Reason> {
    let Phones::Paired(paired) = phones else {
        return Err(Reason::PhonesUnreadable);
    };
    let starts: Option<Vec<_>> = found.iter().map(|identity| identity.start).collect();
    let starts = starts.ok_or(Reason::Malformed)?;
    let latest = now + CLOCK_SLACK;
    if starts.iter().chain(paired).any(|instant| *instant > latest) {
        return Err(Reason::ClockBehind);
    }
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
        _ if known.iter().any(Option::is_some) => Err(Reason::PartlyReplaced),
        _ => Err(Reason::Replaced),
    }
}
