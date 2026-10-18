//! The tray's meeting rows: the processing queue, the five recent meetings
//! and the meetings whose speakers wait for a review, which the shell's
//! tray draws as menu items and a badge ([`Host::on_tray`](crate::host::Host::on_tray)).
//! Swift: `MenuBarViewModel` and `AppController.pendingReviews`.

use std::collections::BTreeSet;

use steno_core::MeetingStateKind;
use uuid::Uuid;

use crate::main_window::{MeetingListViewModel, ProcessingProgressModel};

/// How many finished meetings the tray lists.
pub const RECENT: usize = 5;

/// One meeting row: the meeting it opens and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayRow {
    pub meeting_id: Uuid,
    pub text: String,
}

/// What the tray shows below its Record rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayMeetings {
    /// Queued and processing meetings, oldest first: "Weekly sync —
    /// Transcribing… 62%".
    pub queue: Vec<TrayRow>,
    /// Ready and failed meetings, newest first, at most [`RECENT`].
    pub recent: Vec<TrayRow>,
    /// Meetings the pipeline flagged with unconfirmed speakers this
    /// launch, newest first; their count is the badge.
    pub reviews: Vec<TrayRow>,
}

/// The rows from the list, the progress and the pending reviews.
#[must_use]
pub fn tray_meetings(
    list: &MeetingListViewModel,
    progress: &ProcessingProgressModel,
    reviews: &BTreeSet<Uuid>,
) -> TrayMeetings {
    let mut queue: Vec<_> = list
        .all
        .iter()
        .filter(|meeting| {
            matches!(
                meeting.state.kind(),
                MeetingStateKind::Queued | MeetingStateKind::Processing
            )
        })
        .collect();
    queue.sort_by_key(|meeting| meeting.started_at);
    let queue = queue
        .into_iter()
        .map(|meeting| {
            let entry = progress.entry(meeting.id);
            let stage = entry.map_or_else(
                || crate::main_window::ProgressEntry::WAITING_TITLE.to_owned(),
                |entry| entry.title(progress.speech_fallback),
            );
            let percent = entry
                .filter(|entry| entry.progress.is_some())
                .map(|entry| format!(" {:.0}%", entry.fraction() * 100.0))
                .unwrap_or_default();
            TrayRow {
                meeting_id: meeting.id,
                text: format!("{} — {stage}{percent}", meeting.title),
            }
        })
        .collect();
    let recent = list
        .all
        .iter()
        .filter(|meeting| {
            meeting.state.kind() == MeetingStateKind::Ready || meeting.state.is_failed()
        })
        .take(RECENT)
        .map(|meeting| TrayRow {
            meeting_id: meeting.id,
            text: if meeting.state.is_failed() {
                format!("{} — Failed", meeting.title)
            } else {
                meeting.title.clone()
            },
        })
        .collect();
    let reviews = list
        .all
        .iter()
        .filter(|meeting| reviews.contains(&meeting.id))
        .map(|meeting| TrayRow {
            meeting_id: meeting.id,
            text: format!("Review speakers in {}", meeting.title),
        })
        .collect();
    TrayMeetings {
        queue,
        recent,
        reviews,
    }
}

/// Keeps the reviews still pending after a reload: a meeting the list no
/// longer has, or one whose speakers are all confirmed by now, leaves.
pub fn prune_reviews(reviews: &mut BTreeSet<Uuid>, list: &MeetingListViewModel) {
    reviews.retain(|id| {
        list.speakers_by_meeting
            .get(id)
            .is_some_and(|speakers| speakers.iter().any(|s| !s.assignment.is_confirmed()))
    });
}
