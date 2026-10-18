//! The calendar event that names a recording and its attendees, looked up
//! on a thread of its own when the recording starts, so a slow or hung
//! calendar never holds a start up: a late answer names the row afterwards
//! (`Store::name_recording_after_event`). Nothing here prompts for access.
//! Titles and attendee names are the user's data: no log line carries them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Days, TimeZone, Utc};
use steno_core::{CALENDAR_LOOKAHEAD, CalendarAttendee, CalendarEvent, pick_event};

/// The calendar a recording's start reads.
pub trait Calendar: Send + Sync {
    /// Every timed event of the local calendar day that contains `day`;
    /// empty without access. Never prompts. May block; the recorder calls
    /// it on a thread of its own.
    fn events_on(&self, day: DateTime<Utc>) -> Result<Vec<CalendarEvent>, String>;
}

/// The calendar off the Mac, and for the CLI and the tests: no events,
/// no prompt.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoCalendar;

impl Calendar for NoCalendar {
    fn events_on(&self, _day: DateTime<Utc>) -> Result<Vec<CalendarEvent>, String> {
        Ok(Vec::new())
    }
}

/// The calendar this platform has: `EventKitCalendar` on the Mac,
/// [`NoCalendar`] elsewhere.
#[must_use]
pub fn system_calendar() -> Arc<dyn Calendar> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(EventKitCalendar)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(NoCalendar)
    }
}

/// `EventKit` through `steno_macos::calendar`: the events of the local day,
/// read only with full access, so it never prompts.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, Default)]
pub struct EventKitCalendar;

#[cfg(target_os = "macos")]
impl Calendar for EventKitCalendar {
    fn events_on(&self, day: DateTime<Utc>) -> Result<Vec<CalendarEvent>, String> {
        use steno_macos::calendar::{CalendarError, events_between};

        let Some((start, end)) = day_bounds(day, &chrono::Local) else {
            return Ok(Vec::new());
        };
        #[allow(
            clippy::cast_precision_loss,
            reason = "milliseconds since 1970 sit far inside an f64's 52 bits"
        )]
        let to_seconds = |time: DateTime<Utc>| time.timestamp_millis() as f64 / 1000.0;
        let events = match events_between(to_seconds(start), to_seconds(end)) {
            Ok(events) => events,
            Err(CalendarError::NoAccess(_)) => return Ok(Vec::new()),
            Err(error) => return Err(error.to_string()),
        };
        let mut events: Vec<CalendarEvent> = events
            .into_iter()
            .filter_map(|event| {
                let attendees = event
                    .participants
                    .into_iter()
                    .filter_map(|one| {
                        attendee(
                            one.name.as_deref(),
                            one.url.as_deref(),
                            one.is_person,
                            one.is_current_user,
                        )
                    })
                    .collect();
                timed_event(
                    event.id,
                    event.title,
                    event.start.and_then(from_seconds),
                    event.end.and_then(from_seconds),
                    event.is_all_day,
                    attendees,
                )
            })
            .collect();
        events.sort_by_key(|event| event.start);
        Ok(events)
    }
}

/// Seconds since 1970 as `EventKit`'s `NSDate` gives them, to the
/// millisecond.
#[cfg(target_os = "macos")]
#[allow(
    clippy::cast_possible_truncation,
    reason = "calendar dates sit far inside `i64` milliseconds"
)]
fn from_seconds(seconds: f64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis((seconds * 1000.0).round() as i64)
}

/// The local calendar day in `zone` that contains `instant`, midnight to
/// midnight, so a day that changes the clock lasts 23 or 25 hours.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn day_bounds<Tz: TimeZone>(
    instant: DateTime<Utc>,
    zone: &Tz,
) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let date = instant.with_timezone(zone).date_naive();
    let midnight = |date: chrono::NaiveDate| {
        zone.from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
            .earliest()
            .map(|time| time.with_timezone(&Utc))
    };
    Some((
        midnight(date)?,
        midnight(date.checked_add_days(Days::new(1))?)?,
    ))
}

/// The attendee Steno keeps of an `EventKit` participant: people only; the
/// email from a `mailto:` URL; the name trimmed of spaces and tabs (not
/// line breaks), or the email when the name is blank; none without either.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn attendee(
    name: Option<&str>,
    url: Option<&str>,
    is_person: bool,
    is_current_user: bool,
) -> Option<CalendarAttendee> {
    const MAILTO: &str = "mailto:";
    if !is_person {
        return None;
    }
    let email = url
        .filter(|url| {
            url.get(..MAILTO.len())
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case(MAILTO))
        })
        .map(|url| url[MAILTO.len()..].to_owned());
    let name = name
        .unwrap_or_default()
        .trim_matches(|c: char| c == '\t' || (c.is_whitespace() && !is_line_break(c)));
    if name.is_empty() && email.is_none() {
        return None;
    }
    Some(CalendarAttendee {
        name: if name.is_empty() {
            email.clone().unwrap_or_default()
        } else {
            name.to_owned()
        },
        email,
        is_current_user,
    })
}

/// The characters `CharacterSet.newlines` holds, which the trim leaves.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\u{0B}' | '\u{0C}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// The event Steno keeps of an `EventKit` event: a timed event with an
/// identifier, a start and an end; a missing title is empty.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn timed_event(
    id: Option<String>,
    title: Option<String>,
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    is_all_day: bool,
    attendees: Vec<CalendarAttendee>,
) -> Option<CalendarEvent> {
    if is_all_day {
        return None;
    }
    Some(CalendarEvent {
        id: id?,
        title: title.unwrap_or_default(),
        start: start?,
        end: end?,
        attendees,
    })
}

/// What a running lookup has to hand over.
enum Answer {
    /// Still reading the calendar.
    Pending,
    /// Returned: the event, or none.
    Done(Option<CalendarEvent>),
    /// The recorder wrote the row without it: what runs with the answer.
    Then(Box<dyn FnOnce(Option<CalendarEvent>) + Send>),
}

/// One recording's lookup; see the module doc.
pub(crate) struct Lookup {
    answer: Arc<Mutex<Answer>>,
}

impl Lookup {
    /// The answer once the lookup has returned; the lookup itself while it
    /// still reads, for [`Self::then`].
    pub(crate) fn take(self) -> Result<Option<CalendarEvent>, Lookup> {
        let mut answer = self.answer.lock().unwrap_or_else(PoisonError::into_inner);
        match std::mem::replace(&mut *answer, Answer::Done(None)) {
            Answer::Done(event) => Ok(event),
            other => {
                *answer = other;
                drop(answer);
                Err(self)
            }
        }
    }

    /// Runs `then` with the answer once the lookup returns, here or on the
    /// lookup's thread. A lookup whose thread panicked never runs it.
    pub(crate) fn then(self, then: impl FnOnce(Option<CalendarEvent>) + Send + 'static) {
        let mut answer = self.answer.lock().unwrap_or_else(PoisonError::into_inner);
        match std::mem::replace(&mut *answer, Answer::Done(None)) {
            Answer::Pending => *answer = Answer::Then(Box::new(then)),
            Answer::Done(event) => {
                drop(answer);
                then(event);
            }
            Answer::Then(_) => unreachable!("`then` takes the lookup"),
        }
    }
}

/// The recorder's calendar and the one lookup it may have running.
pub(crate) struct CalendarLookups {
    calendar: Mutex<Arc<dyn Calendar>>,
    running: Arc<AtomicBool>,
    /// Lookups whose answer was handed over: a test's signal that a late
    /// answer is done.
    #[cfg(test)]
    pub(crate) answered: Arc<std::sync::atomic::AtomicUsize>,
}

/// Clears the running flag when the lookup's thread ends, also by a
/// panic.
struct Running(Arc<AtomicBool>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl CalendarLookups {
    pub(crate) fn new(calendar: Arc<dyn Calendar>) -> Self {
        Self {
            calendar: Mutex::new(calendar),
            running: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            answered: Arc::default(),
        }
    }

    /// Reads `calendar` from the next lookup on.
    pub(crate) fn read_from(&self, calendar: Arc<dyn Calendar>) {
        *self.calendar.lock().unwrap_or_else(PoisonError::into_inner) = calendar;
    }

    /// Starts the lookup for a recording that starts at `now` on a thread of
    /// its own. `None` while an earlier lookup has not returned (so a hung
    /// calendar holds one thread), or when the thread does not start.
    pub(crate) fn start(&self, now: DateTime<Utc>) -> Option<Lookup> {
        if self.running.swap(true, Ordering::SeqCst) {
            tracing::debug!("the calendar has not answered the previous recording's lookup");
            return None;
        }
        let running = Running(self.running.clone());
        let calendar = self
            .calendar
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let answer = Arc::new(Mutex::new(Answer::Pending));
        let handed = answer.clone();
        #[cfg(test)]
        let answered = self.answered.clone();
        let spawned = std::thread::Builder::new()
            .name("steno-calendar".into())
            .spawn(move || {
                let event = match calendar.events_on(now) {
                    Ok(events) => pick_event(&events, now, CALENDAR_LOOKAHEAD).cloned(),
                    Err(error) => {
                        tracing::debug!(%error, "the calendar could not be read");
                        None
                    }
                };
                drop(running);
                let mut answer = handed.lock().unwrap_or_else(PoisonError::into_inner);
                match std::mem::replace(&mut *answer, Answer::Done(None)) {
                    Answer::Then(then) => {
                        drop(answer);
                        then(event);
                    }
                    Answer::Pending | Answer::Done(_) => *answer = Answer::Done(event),
                }
                #[cfg(test)]
                answered.fetch_add(1, Ordering::SeqCst);
            });
        match spawned {
            Ok(_) => Some(Lookup { answer }),
            Err(error) => {
                tracing::warn!(%error, "the calendar lookup could not start");
                None
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! A calendar that answers what a test set, and that a test can hold
    //! at a gate.

    use std::sync::{Arc, Condvar, Mutex, PoisonError};

    use chrono::{DateTime, Utc};
    use steno_core::CalendarEvent;

    use super::Calendar;

    #[derive(Default)]
    struct State {
        events: Vec<CalendarEvent>,
        /// While true every read waits.
        held: bool,
        /// Reads that have begun.
        reads: usize,
        /// Reads that wait at the gate now.
        waiting: usize,
        /// While true every read fails.
        failing: bool,
    }

    /// The fake: `events` for any day, or an error while `failing`.
    #[derive(Default)]
    pub(crate) struct FakeCalendar {
        state: Mutex<State>,
        changed: Condvar,
    }

    impl FakeCalendar {
        pub(crate) fn with(events: Vec<CalendarEvent>) -> Arc<Self> {
            let calendar = Arc::new(Self::default());
            calendar.lock().events = events;
            calendar
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, State> {
            self.state.lock().unwrap_or_else(PoisonError::into_inner)
        }

        /// Every read from now on waits until [`Self::release`].
        pub(crate) fn hold(&self) {
            self.lock().held = true;
        }

        pub(crate) fn release(&self) {
            self.lock().held = false;
            self.changed.notify_all();
        }

        pub(crate) fn fail(&self) {
            self.lock().failing = true;
        }

        pub(crate) fn reads(&self) -> usize {
            self.lock().reads
        }

        /// Blocks until a read waits at the gate, for up to `patience`;
        /// whether one does.
        pub(crate) fn wait_for_a_held_read(&self, patience: std::time::Duration) -> bool {
            let (state, _) = self
                .changed
                .wait_timeout_while(self.lock(), patience, |state| state.waiting == 0)
                .unwrap_or_else(PoisonError::into_inner);
            state.waiting > 0
        }
    }

    impl Calendar for FakeCalendar {
        fn events_on(&self, _day: DateTime<Utc>) -> Result<Vec<CalendarEvent>, String> {
            let mut state = self.lock();
            state.reads += 1;
            state.waiting += 1;
            self.changed.notify_all();
            let mut state = self
                .changed
                .wait_while(state, |state| state.held)
                .unwrap_or_else(PoisonError::into_inner);
            state.waiting -= 1;
            if state.failing {
                return Err("the calendar failed".to_owned());
            }
            Ok(state.events.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc;

    use chrono::{FixedOffset, TimeDelta};

    use super::testing::FakeCalendar;
    use super::*;
    use crate::testing::PATIENCE;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_413_200, 0).unwrap()
    }

    fn event(id: &str, start: i64, end: i64) -> CalendarEvent {
        CalendarEvent {
            id: id.to_owned(),
            title: format!("Title of {id}"),
            start: now() + TimeDelta::seconds(start),
            end: now() + TimeDelta::seconds(end),
            attendees: Vec::new(),
        }
    }

    /// One row per attendee rule.
    #[test]
    fn attendees_follow_the_swift_rules() {
        let person = |name: Option<&str>, url: Option<&str>| attendee(name, url, true, false);
        let kept = |name: &str, email: Option<&str>| {
            Some(CalendarAttendee {
                name: name.to_owned(),
                email: email.map(str::to_owned),
                is_current_user: false,
            })
        };
        assert_eq!(
            person(Some("Ada"), Some("mailto:ada@example.com")),
            kept("Ada", Some("ada@example.com")),
            "the email from a mailto URL"
        );
        assert_eq!(
            person(Some("Ada"), Some("MAILTO:ada@example.com")),
            kept("Ada", Some("ada@example.com")),
            "the scheme in any case"
        );
        assert_eq!(
            person(Some("Ada"), Some("urn:uuid:42")),
            kept("Ada", None),
            "no email from another scheme"
        );
        assert_eq!(
            person(Some(" \tAda \t"), None),
            kept("Ada", None),
            "spaces and tabs trimmed"
        );
        assert_eq!(
            person(Some("\nAda"), None),
            kept("\nAda", None),
            "line breaks kept, as Swift's .whitespaces does"
        );
        assert_eq!(
            person(Some("   "), Some("mailto:ada@example.com")),
            kept("ada@example.com", Some("ada@example.com")),
            "a blank name becomes the email"
        );
        assert_eq!(person(None, None), None, "neither name nor email");
        assert_eq!(person(Some(" "), Some("https://example.com")), None);
        assert_eq!(
            attendee(
                Some("Room 4"),
                Some("mailto:room@example.com"),
                false,
                false
            ),
            None,
            "rooms, resources and groups are not attendees"
        );
        assert_eq!(
            attendee(Some("Me"), None, true, true).map(|one| one.is_current_user),
            Some(true),
            "the owner is marked, not dropped: the recorder drops it"
        );
    }

    /// Only timed events with an identifier are kept.
    #[test]
    fn only_timed_events_with_an_id_and_both_times_are_kept() {
        let start = Some(now());
        let end = Some(now() + TimeDelta::hours(1));
        let id = || Some("event".to_owned());
        let kept = timed_event(id(), None, start, end, false, Vec::new()).unwrap();
        assert_eq!(kept.title, "", "a missing title is empty");
        assert_eq!(timed_event(id(), None, start, end, true, Vec::new()), None);
        assert_eq!(timed_event(None, None, start, end, false, Vec::new()), None);
        assert_eq!(timed_event(id(), None, None, end, false, Vec::new()), None);
        assert_eq!(
            timed_event(id(), None, start, None, false, Vec::new()),
            None
        );
    }

    /// The local day runs from midnight to midnight in the zone, so an
    /// instant just before midnight UTC can fall on the next local day.
    #[test]
    fn the_day_is_the_local_one() {
        let berlin_summer = FixedOffset::east_opt(2 * 3600).unwrap();
        let instant = DateTime::parse_from_rfc3339("2026-10-12T23:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let (start, end) = day_bounds(instant, &berlin_summer).unwrap();
        assert_eq!(start.to_rfc3339(), "2026-10-12T22:00:00+00:00");
        assert_eq!(end.to_rfc3339(), "2026-10-13T22:00:00+00:00");
        let (start, _) = day_bounds(instant, &Utc).unwrap();
        assert_eq!(start.to_rfc3339(), "2026-10-12T00:00:00+00:00");
    }

    /// Off the Mac the lookup lists nothing and asks for nothing.
    #[test]
    fn no_calendar_lists_nothing() {
        assert_eq!(NoCalendar.events_on(now()), Ok(Vec::new()));
        if !cfg!(target_os = "macos") {
            assert_eq!(system_calendar().events_on(now()), Ok(Vec::new()));
        }
    }

    /// On the Mac the system calendar reads without asking: without full
    /// access it lists nothing, and the access stays what it was.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_calendar_reads_without_asking() {
        use steno_macos::calendar::{CalendarAccess, access};
        let before = access();
        let read = system_calendar().events_on(now());
        if before == CalendarAccess::FullAccess {
            assert!(read.is_ok(), "{read:?}");
        } else {
            assert_eq!(read, Ok(Vec::new()));
        }
        assert_eq!(access(), before, "the read asked for nothing");
    }

    /// The lookup answers the event the rule picks among the day's.
    #[test]
    fn a_finished_lookup_answers_the_picked_event() {
        let calendar = FakeCalendar::with(vec![event("later", 600, 1200), event("now", -60, 600)]);
        let lookups = CalendarLookups::new(calendar.clone());
        let lookup = lookups.start(now()).unwrap();
        // `then` on the start's thread.
        let (sender, receiver) = mpsc::channel();
        lookup.then(move |event| sender.send(event.map(|event| event.id)).unwrap());
        assert_eq!(
            receiver.recv_timeout(PATIENCE).unwrap().as_deref(),
            Some("now")
        );
    }

    /// While the calendar has not answered, no second lookup starts; once it
    /// answers, the event goes to `then` and the next start reads again.
    #[test]
    fn a_held_lookup_hands_its_event_over_later() {
        let calendar = FakeCalendar::with(vec![event("now", -60, 600)]);
        calendar.hold();
        let lookups = CalendarLookups::new(calendar.clone());
        let lookup = lookups.start(now()).unwrap();
        assert!(calendar.wait_for_a_held_read(PATIENCE));
        let Err(lookup) = lookup.take() else {
            panic!("an answer while it reads")
        };
        assert!(lookups.start(now()).is_none(), "one lookup at a time");
        let (sender, receiver) = mpsc::channel();
        lookup.then(move |event| sender.send(event.map(|event| event.id)).unwrap());
        calendar.release();
        assert_eq!(
            receiver.recv_timeout(PATIENCE).unwrap().as_deref(),
            Some("now")
        );
        // The thread clears its flag before it hands the event over.
        let next = lookups.start(now()).unwrap();
        let (sender, receiver) = mpsc::channel();
        next.then(move |event| sender.send(event.map(|event| event.id)).unwrap());
        assert_eq!(
            receiver.recv_timeout(PATIENCE).unwrap().as_deref(),
            Some("now")
        );
        assert_eq!(calendar.reads(), 2);
    }

    /// No event and a calendar that fails both answer no event.
    #[test]
    fn no_event_and_a_failed_read_answer_none() {
        let empty = FakeCalendar::with(vec![event("ended", -1200, -600)]);
        let failing = FakeCalendar::with(vec![event("now", -60, 600)]);
        failing.fail();
        for calendar in [empty, failing] {
            let lookups = CalendarLookups::new(calendar);
            let (sender, receiver) = mpsc::channel();
            lookups
                .start(now())
                .unwrap()
                .then(move |event| sender.send(event).unwrap());
            assert_eq!(receiver.recv_timeout(PATIENCE).unwrap(), None);
        }
    }

    /// A returned answer is taken whole; a lookup still reading gives
    /// itself back, still pending.
    #[test]
    fn a_returned_answer_is_taken_and_a_pending_one_kept() {
        let answered = Lookup {
            answer: Arc::new(Mutex::new(Answer::Done(Some(event("now", -60, 600))))),
        };
        let Ok(Some(taken)) = answered.take() else {
            panic!("the answer is in")
        };
        assert_eq!(taken.id, "now");

        let reading = Lookup {
            answer: Arc::new(Mutex::new(Answer::Pending)),
        };
        let Err(reading) = reading.take() else {
            panic!("no answer yet")
        };
        assert!(matches!(*reading.answer.lock().unwrap(), Answer::Pending));
    }
}
