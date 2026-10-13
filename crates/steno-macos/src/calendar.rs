//! `EventKit`: the calendar permission and the timed events of a stretch of
//! time, read as plain values. Swift: `CalendarService` in
//! `apps/macos/Steno/Services/CalendarService.swift` (the events) and
//! `PermissionsService` in `apps/macos/Steno/Services/PermissionsService.swift`
//! (the status and the prompt). Which event names a recording is not
//! decided here: `steno-services`' `calendar` module maps these values and
//! `steno_core::pick_event` picks one.
//!
//! Only [`request_full_access`] can show the system prompt.
//! [`events_between`] reads nothing, and creates no event store, unless the
//! user has already granted full access, so a lookup never prompts.
//!
//! Every call goes through `objc2-event-kit`'s bindings, which mark all of
//! `EventKit` `unsafe`; each block below says what it relies on. The event
//! store is made for the call and released on the thread that made it, so
//! no `EventKit` object crosses a thread.
#![allow(unsafe_code)]

use std::sync::Mutex;
use std::sync::mpsc;

use block2::RcBlock;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::Bool;
use objc2::{AnyThread as _, msg_send};
use objc2_event_kit::{
    EKAuthorizationStatus, EKEntityType, EKEvent, EKEventStore, EKParticipant, EKParticipantType,
};
use objc2_foundation::{NSDate, NSError, NSString, NSURL};

/// What the user allowed Steno to do with calendar events
/// (`EKAuthorizationStatus` for `EKEntityTypeEvent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalendarAccess {
    /// Never asked: [`request_full_access`] shows the prompt.
    NotDetermined,
    /// A policy (parental controls, a profile) forbids access.
    Restricted,
    /// The user said no, here or in System Settings.
    Denied,
    /// Events can be read: the only state [`events_between`] reads in.
    FullAccess,
    /// Events can be added but not read.
    WriteOnly,
}

/// Calendar access as macOS has it recorded for this app, without
/// prompting.
#[must_use]
pub fn access() -> CalendarAccess {
    // SAFETY: a class method with no preconditions: it reads the TCC record
    // for this process and never prompts. `EKEntityType::Event` is one of
    // the two values the method takes.
    let status = unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) };
    match status {
        EKAuthorizationStatus::FullAccess => CalendarAccess::FullAccess,
        EKAuthorizationStatus::WriteOnly => CalendarAccess::WriteOnly,
        EKAuthorizationStatus::Denied => CalendarAccess::Denied,
        EKAuthorizationStatus::Restricted => CalendarAccess::Restricted,
        // `NotDetermined`, and any status a later macOS adds: neither reads
        // events, and the prompt is still the way to find out.
        _ => CalendarAccess::NotDetermined,
    }
}

/// Why [`request_full_access`] or [`events_between`] has no answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CalendarError {
    /// [`events_between`] without full access: nothing was read.
    #[error("calendar access is {0:?}, not full access")]
    NoAccess(CalendarAccess),
    /// The request's completion never ran: `EventKit` dropped it.
    #[error("the calendar access request ended without an answer")]
    NoAnswer,
    /// `EventKit` answered the request with an error.
    #[error("the calendar access request failed: {0}")]
    Request(String),
}

/// Asks for full access to calendar events
/// (`requestFullAccessToEventsWithCompletion:`): the system prompt the
/// first time, the recorded answer after. Blocks until the user answers,
/// so call it from a thread that may wait (never the main thread). `true`
/// when access is granted. The prompt needs
/// `NSCalendarsFullAccessUsageDescription` in the app's `Info.plist`;
/// without it macOS ends the process.
pub fn request_full_access() -> Result<bool, CalendarError> {
    let (sender, receiver) = mpsc::channel::<Result<bool, CalendarError>>();
    // The block is `Fn`, so the one-shot sender sits in a slot it takes
    // from.
    let sender = Mutex::new(Some(sender));
    let completion: RcBlock<dyn Fn(Bool, *mut NSError)> =
        RcBlock::new(move |granted: Bool, error: *mut NSError| {
            // SAFETY: `EventKit` passes either null or a valid `NSError` that
            // lives for the duration of the completion; it is only read here.
            let error = unsafe { error.as_ref() };
            let answer = match error {
                Some(error) if !granted.as_bool() => Err(CalendarError::Request(
                    error.localizedDescription().to_string(),
                )),
                _ => Ok(granted.as_bool()),
            };
            if let Some(sender) = sender
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                let _ = sender.send(answer);
            }
        });
    let store = new_store();
    // SAFETY: `completion` is a valid block for the call: `RcBlock::as_ptr`
    // points at a block this function owns until it returns, and `EventKit`
    // copies the block before the call returns, since it runs it later on
    // a queue of its own. The block's signature is the one the selector
    // declares, `(BOOL granted, NSError *error)`. `store` is alive for the
    // whole wait below, so the request is never cut short by its release.
    unsafe {
        store.requestFullAccessToEventsWithCompletion(RcBlock::as_ptr(&completion));
    }
    let answer = receiver.recv().unwrap_or(Err(CalendarError::NoAnswer));
    drop(store);
    answer
}

/// An attendee as `EventKit` lists it, before Steno's rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Participant {
    /// `EKParticipant.name`, as `EventKit` has it (untrimmed).
    pub name: Option<String>,
    /// `EKParticipant.URL`, usually `mailto:` and the address.
    pub url: Option<String>,
    /// `participantType` is `EKParticipantTypePerson`, not a room, a
    /// resource or a group.
    pub is_person: bool,
    /// `isCurrentUser`: the calendar's owner.
    pub is_current_user: bool,
}

/// An event as `EventKit` lists it, before Steno's rules. Times are seconds
/// since 1970 in UTC.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// `eventIdentifier`; `None` when `EventKit` gave none.
    pub id: Option<String>,
    /// `title`; `None` when `EventKit` gave none.
    pub title: Option<String>,
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub is_all_day: bool,
    pub participants: Vec<Participant>,
}

/// Every event of every calendar that overlaps `start..end` (seconds
/// since 1970, UTC), all-day ones included, in no particular order
/// (`predicateForEventsWithStartDate:endDate:calendars:` with all
/// calendars, then `eventsMatchingPredicate:`). Without full access it
/// reads nothing and answers [`CalendarError::NoAccess`]; it never
/// prompts. Synchronous: it blocks the calling thread for as long as
/// `EventKit` takes (milliseconds for a day), so call it off the thread that
/// starts a recording.
pub fn events_between(start: f64, end: f64) -> Result<Vec<Event>, CalendarError> {
    let access = access();
    if access != CalendarAccess::FullAccess {
        return Err(CalendarError::NoAccess(access));
    }
    Ok(autoreleasepool(|_| {
        let store = new_store();
        let start = NSDate::dateWithTimeIntervalSince1970(start);
        let end = NSDate::dateWithTimeIntervalSince1970(end);
        // SAFETY: `store`, `start` and `end` are valid objects for the
        // call; `None` asks for every calendar, which the method documents.
        // The predicate comes from this store's own factory, the only kind
        // `eventsMatchingPredicate:` accepts without raising.
        let events = unsafe {
            let predicate =
                store.predicateForEventsWithStartDate_endDate_calendars(&start, &end, None);
            store.eventsMatchingPredicate(&predicate)
        };
        events.iter().map(|event| read_event(&event)).collect()
    }))
}

/// An event store for one call, released by the caller's thread.
fn new_store() -> Retained<EKEventStore> {
    // SAFETY: `init` on a freshly allocated `EKEventStore` is the
    // designated initialiser; since macOS 14 it asks for nothing (the
    // prompt is `requestFullAccessToEventsWithCompletion:`), and Steno
    // needs macOS 15.
    unsafe { EKEventStore::init(EKEventStore::alloc()) }
}

fn read_event(event: &EKEvent) -> Event {
    // SAFETY: getters on a valid `EKEvent` that `eventsMatchingPredicate:`
    // returned on this thread. Each object return is read as optional:
    // the headers leave their nullability unspecified, and a `nil` read
    // as non-optional would panic. `isAllDay` returns a plain `BOOL`.
    unsafe {
        let id: Option<Retained<NSString>> = msg_send![event, eventIdentifier];
        let title: Option<Retained<NSString>> = msg_send![event, title];
        let start: Option<Retained<NSDate>> = msg_send![event, startDate];
        let end: Option<Retained<NSDate>> = msg_send![event, endDate];
        let attendees: Option<Retained<objc2_foundation::NSArray<EKParticipant>>> =
            msg_send![event, attendees];
        Event {
            id: id.map(|id| id.to_string()),
            title: title.map(|title| title.to_string()),
            start: start.map(|date| date.timeIntervalSince1970()),
            end: end.map(|date| date.timeIntervalSince1970()),
            is_all_day: event.isAllDay(),
            participants: attendees
                .map(|attendees| attendees.iter().map(|one| read_participant(&one)).collect())
                .unwrap_or_default(),
        }
    }
}

fn read_participant(participant: &EKParticipant) -> Participant {
    // SAFETY: getters on a valid `EKParticipant` from the event's
    // `attendees` array, read on the thread that fetched it; the object
    // returns are read as optional, as in `read_event`.
    unsafe {
        let name: Option<Retained<NSString>> = msg_send![participant, name];
        let url: Option<Retained<NSURL>> = msg_send![participant, URL];
        Participant {
            name: name.map(|name| name.to_string()),
            url: url
                .and_then(|url| url.absoluteString())
                .map(|url| url.to_string()),
            is_person: participant.participantType() == EKParticipantType::Person,
            is_current_user: participant.isCurrentUser(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CalendarAccess, CalendarError, access, events_between};

    /// The status reads without a prompt (a headless test process is
    /// never asked), and the read follows it: without full access it
    /// refuses before it makes an event store, so it can never prompt, and
    /// names the access it found. A test process normally has no grant
    /// (CI, Forge over SSH); one started from a terminal the user let read
    /// the calendar reads events instead.
    #[test]
    fn the_read_follows_the_access_and_never_prompts() {
        let now = 1_790_413_200.0;
        let found = access();
        let read = events_between(now, now + 86_400.0);
        if found == CalendarAccess::FullAccess {
            assert!(read.is_ok(), "{read:?}");
        } else {
            assert_eq!(read, Err(CalendarError::NoAccess(found)));
        }
        assert_eq!(access(), found, "reading changed nothing");
    }
}
