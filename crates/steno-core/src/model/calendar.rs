//! The calendar events a recording can be named after, and the rule that
//! picks one. Swift: `CalendarEvent` and `CalendarAttendee` in
//! `apps/macos/Steno/Services/AppProtocols.swift`, the rule `CalendarMatch`
//! in `Sources/StenoCore/Model/Calendar.swift`.

use chrono::{DateTime, TimeDelta, Utc};

/// Someone invited to a [`CalendarEvent`]. Swift: `CalendarAttendee`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CalendarAttendee {
    /// The calendar's name for the person, or the email when it has none.
    pub name: String,
    pub email: Option<String>,
    /// The calendar's owner: the recording's "me", never a participant row.
    pub is_current_user: bool,
}

/// One timed event of a calendar day; all-day events are never listed.
/// Swift: `CalendarEvent`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CalendarEvent {
    /// The calendar's identifier for the event. The occurrences of a
    /// repeating event share it.
    pub id: String,
    pub title: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub attendees: Vec<CalendarAttendee>,
}

/// How far ahead [`pick_event`] looks for an event about to start: fifteen
/// minutes. Swift: `CalendarMatch.pick`'s `lookahead` default.
pub const CALENDAR_LOOKAHEAD: TimeDelta = TimeDelta::seconds(900);

/// The event a recording starting at `now` most likely belongs to. An
/// event under way wins (`start <= now < end`), the one that started most
/// recently when several are; otherwise the next one that starts within
/// `lookahead`, the soonest first. Ties go to the smaller `id`, so the
/// choice never depends on the order of `events`. Swift:
/// `CalendarMatch.pick`, which returns the id; the app then took the first
/// event with that id, which is the same event unless a repeating event
/// has two occurrences in one day.
#[must_use]
pub fn pick_event(
    events: &[CalendarEvent],
    now: DateTime<Utc>,
    lookahead: TimeDelta,
) -> Option<&CalendarEvent> {
    let under_way = events
        .iter()
        .filter(|event| event.start <= now && now < event.end)
        .max_by(|a, b| a.start.cmp(&b.start).then_with(|| b.id.cmp(&a.id)));
    under_way.or_else(|| {
        events
            .iter()
            .filter(|event| event.start > now && event.start - now <= lookahead)
            .min_by(|a, b| a.start.cmp(&b.start).then_with(|| a.id.cmp(&b.id)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Swift: `CalendarMatchTests.now`.
    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_413_200, 0).unwrap()
    }

    /// An event from `start` to `end` seconds after [`now`].
    fn event(id: &str, start: i64, end: i64) -> CalendarEvent {
        CalendarEvent {
            id: id.to_owned(),
            title: format!("Title of {id}"),
            start: now() + TimeDelta::seconds(start),
            end: now() + TimeDelta::seconds(end),
            attendees: Vec::new(),
        }
    }

    fn picked(events: &[CalendarEvent], lookahead: TimeDelta) -> Option<&str> {
        pick_event(events, now(), lookahead).map(|event| event.id.as_str())
    }

    /// Swift's `CalendarMatchTests`, one row per expectation, each also
    /// with the events in the other order: the choice must not depend on
    /// it.
    #[test]
    fn the_choice_follows_the_swift_rule() {
        let default = CALENDAR_LOOKAHEAD;
        let rows: Vec<(&str, Vec<CalendarEvent>, TimeDelta, Option<&str>)> = vec![
            (
                "an event under way wins over a later one",
                vec![event("later", 600, 1200), event("current", -600, 600)],
                default,
                Some("current"),
            ),
            (
                "of two under way, the one that started last",
                vec![event("long", -3600, 3600), event("short", -300, 300)],
                default,
                Some("short"),
            ),
            (
                "else the next within fifteen minutes",
                vec![
                    event("far", 2000, 2600),
                    event("soon", 500, 1100),
                    event("past", -1200, -600),
                ],
                default,
                Some("soon"),
            ),
            (
                "nothing starts within a shorter look ahead",
                vec![event("far", 2000, 2600), event("soon", 500, 1100)],
                TimeDelta::seconds(400),
                None,
            ),
            (
                "a longer look ahead still takes the soonest",
                vec![event("far", 2000, 2600), event("soon", 500, 1100)],
                TimeDelta::seconds(2000),
                Some("soon"),
            ),
            (
                "an event that ended at now does not match",
                vec![event("past", -1200, 0)],
                default,
                None,
            ),
            ("no events, no match", vec![], default, None),
            (
                "an event that starts at now is under way",
                vec![event("starts", 0, 600), event("soon", 60, 600)],
                default,
                Some("starts"),
            ),
            (
                "an event exactly fifteen minutes out still counts",
                vec![event("edge", 900, 1200)],
                default,
                Some("edge"),
            ),
            (
                "one a second later does not",
                vec![event("late", 901, 1200)],
                default,
                None,
            ),
            (
                "a tie under way goes to the smaller id",
                vec![event("b", 0, 600), event("a", 0, 600)],
                default,
                Some("a"),
            ),
            (
                "a tie ahead goes to the smaller id",
                vec![event("d", 300, 600), event("c", 300, 600)],
                default,
                Some("c"),
            ),
        ];
        for (what, events, lookahead, expected) in rows {
            assert_eq!(picked(&events, lookahead), expected, "{what}");
            let reversed: Vec<CalendarEvent> = events.into_iter().rev().collect();
            assert_eq!(picked(&reversed, lookahead), expected, "{what}, reversed");
        }
    }

    /// The event itself comes back, with its title and attendees.
    #[test]
    fn the_picked_event_carries_its_title_and_attendees() {
        let mut current = event("current", -60, 600);
        current.attendees.push(CalendarAttendee {
            name: "Ada".to_owned(),
            email: Some("ada@example.com".to_owned()),
            is_current_user: false,
        });
        let events = vec![event("later", 600, 1200), current.clone()];
        assert_eq!(
            pick_event(&events, now(), CALENDAR_LOOKAHEAD),
            Some(&current)
        );
    }
}
