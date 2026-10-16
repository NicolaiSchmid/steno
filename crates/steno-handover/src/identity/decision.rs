//! The handover identity's one decision: from what the reads found, serve
//! an identity, mint one, or wait, with the writes that go with it
//! ([`decide`]). It reads nothing and asks no clock: each caller reads,
//! decides, then applies the writes in order.
//!
//! Its truth table is the statement of the rule:
//! `crates/steno-handover/tests/fixtures/identity-decisions.json`, one row
//! per case, which `tests/identity_decisions.rs` runs through [`decide`].
//! A new case is a new row there, not a paragraph here.
//!
//! Three callers ask ([`Caller`]):
//! - the load ([`HandoverIdentity::load_or_create`]): the identity the
//!   secret store holds, the record, the paired phones;
//! - the Swift import's choice among the Swift certificates it found
//!   (`steno_services::swift_import::choose_certificate`), which stores
//!   what it serves over whatever the record holds;
//! - the Secret Service move, which pins the identity of the file it moves
//!   (`pin_file_identity` in `steno-services`).
//!
//! An identity found with nothing recorded is judged by the pairing-time
//! rule: a phone knows the newest identity whose start is at or before its
//! pairing. Two equal starts (a certificate's start is a whole second) are
//! no newest one, so nothing is ever chosen by order. Over the times it can
//! read, a start or a pairing more than [`CLOCK_SLACK`] after now means the
//! clock went back, whatever the other times say; phones or a start that
//! cannot be read wait with their read's reason first, as fixing the clock
//! would not help. The import judges every certificate it found, the load
//! the one identity it found, and the move the file's identity alone.
//!
//! No decision destroys a stored identity: [`Write`] holds no write of a
//! secret, the move records at most the file's fingerprint, and the import
//! writes nothing. The two writes that replace a stored identity are the
//! callers' own, by design: Pair again ([`HandoverIdentity::mint_anew`],
//! the user's confirmed choice, which removes every phone) and the Mac
//! import's store over a desktop-id build's identity
//! (`steno_services::swift_import::store_imported_identity`, S6 of
//! `.plans/2026-10-07-stable-promotion.md`).
//!
//! Swift: no counterpart; the Swift app has no guard and mints after a
//! keychain reset (P35 of `.plans/2026-10-07-stable-promotion.md`).
//!
//! [`HandoverIdentity::load_or_create`]: super::HandoverIdentity::load_or_create
//! [`HandoverIdentity::mint_anew`]: super::HandoverIdentity::mint_anew

use chrono::{DateTime, TimeDelta, Utc};

/// How far after now a start or a pairing may be before an identity found
/// with nothing recorded waits for the clock ([`Reason::ClockBehind`]),
/// whatever the other times it can read say. The database stores a pairing time rounded half up
/// to the millisecond, so a phone paired in the instant of the decision can
/// read up to half a millisecond after now; a second is a round margin over
/// that. Starts are never rounded but share it, so the check stays one
/// comparison.
pub const CLOCK_SLACK: TimeDelta = TimeDelta::seconds(1);

/// Who asks: where the callers differ, the decision differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    /// The handover's load of its identity.
    Load,
    /// The Swift import, choosing among the Swift certificates it found.
    /// It does not read the record: its store records what it serves.
    Import,
    /// The Secret Service move, pinning the identity of the file it moves
    /// while the service holds another one.
    Move,
}

/// An identity found: its fingerprint (SHA-256 of the leaf DER, lowercase
/// hex) and when its certificate starts, `None` when that cannot be read.
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
    /// The identities in it: none, one, or for the import several, copies
    /// of one certificate included.
    Found(Vec<Found>),
}

/// The record of the identity's fingerprint
/// ([`FingerprintRecord`](super::FingerprintRecord)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// The file cannot be opened (its permissions, a disk error).
    NotOpened,
    /// The file does not parse.
    DoesNotParse,
    /// The recorded fingerprint and the note of an unfinished Pair again,
    /// each `None` when absent.
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
    /// Each write is idempotent, so a stop after any of them decides the
    /// same at the next read and writes the rest.
    pub writes: Vec<Write>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The identity with this fingerprint: the load serves it, the import
    /// brings it over, the move leaves it recorded.
    Serve(String),
    /// Mint a new identity and store it with its fingerprint recorded.
    /// Only the load mints, and only with no identity, nothing recorded, no
    /// phone paired and a record that reads.
    Mint,
    /// Neither; the reason is the handover's. For the move: nothing is
    /// pinned, and the next load decides.
    Wait(Reason),
}

/// Why the decision waits. Five are
/// [`Unavailability`](super::Unavailability)'s; the others name the read
/// that failed, whose error the caller holds. Each says what the load
/// returns and the card Settings > iPhone shows for it (`wait_reason` in
/// `steno-services`); the import maps the last three to its own lines
/// (`choose_certificate`) and every other reason to a failed lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The record cannot be opened:
    /// [`IdentityError::Record`](super::IdentityError::Record) over an
    /// [`std::io::Error`], the card `fileNotOpened`.
    RecordNotOpened,
    /// The record does not parse:
    /// [`IdentityError::Record`](super::IdentityError::Record), the card
    /// `unreadable`.
    RecordDoesNotParse,
    /// The place the identity is kept could not be read:
    /// [`Unavailability::Unreadable`](super::Unavailability::Unreadable),
    /// the card `unreadable`.
    SecretUnreadable,
    /// An identity, or a certificate's start, does not parse:
    /// [`IdentityError::Malformed`](super::IdentityError::Malformed), the
    /// card `unreadable`.
    Malformed,
    /// The paired phones could not be read:
    /// [`IdentityError::Store`](super::IdentityError::Store), the card
    /// `unreadable`.
    PhonesUnreadable,
    /// [`Unavailability::Missing`](super::Unavailability::Missing), the
    /// card `missing`.
    Missing,
    /// [`Unavailability::Replaced`](super::Unavailability::Replaced), the
    /// card `replaced`.
    Replaced,
    /// [`Unavailability::PartlyReplaced`](super::Unavailability::PartlyReplaced),
    /// the card `partlyReplaced`.
    PartlyReplaced,
    /// [`Unavailability::ClockBehind`](super::Unavailability::ClockBehind),
    /// the card `clockBehind`.
    ClockBehind,
}

/// One write, applied in the order given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Write {
    /// Drop the note of a Pair again that wrote nothing; one that cannot
    /// be dropped is left, as the next read decides the same.
    DropNote,
    /// Remove every paired phone: only to finish a Pair again whose new
    /// identity the secret store holds.
    RemovePhones,
    /// Record this fingerprint, which drops the note in the same write.
    Record(String),
}

/// The decision over `observed`; the truth table
/// (`crates/steno-handover/tests/fixtures/identity-decisions.json`) is its
/// statement.
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

/// The identities found, or the wait for a place that could not be read
/// or held no identity that parses.
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

/// The load with no identity found. A note left stands for a Pair again
/// that wrote nothing, so it goes; then the guard: a mint only with
/// nothing recorded and no phone paired.
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

/// The move pins the file's identity where nothing is recorded, unless the
/// rule shows that no paired phone can know it. It runs once, so where the
/// rule cannot tell (some phones before the identity, the clock, a time or
/// the phones that cannot be read) it pins, and the load waits as
/// replaced: never adopting the service's identity over phones that may
/// know the file's.
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

/// The pairing-time rule over identities found with nothing recorded: of
/// `found`, the index of the one every paired phone knows at `now`, or why
/// there is none. The phones are read before the starts, and both before
/// the clock.
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
