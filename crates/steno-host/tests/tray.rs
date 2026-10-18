//! The tray's meeting rows through the host: the queue, the recent
//! meetings and the pending speaker reviews.

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use steno_core::{MeetingSource, MeetingState, PipelineStage, ProcessingProgress, TitleOrigin};
use steno_host::main_window::MeetingEvent;
use steno_host::tray::TrayMeetings;

fn texts(rows: &[steno_host::tray::TrayRow]) -> Vec<&str> {
    rows.iter().map(|row| row.text.as_str()).collect()
}

#[test]
fn the_tray_lists_the_queue_and_the_recent_meetings() {
    let harness = Harness::builder()
        .seed(|store, fakes| {
            populate_sample(store, fakes);
            for (n, title, started_at, state) in [
                (
                    0x30,
                    "Weekly sync",
                    "2026-09-29T14:00:00.000Z",
                    MeetingState::Processing,
                ),
                (
                    0x31,
                    "Standup",
                    "2026-09-29T15:00:00.000Z",
                    MeetingState::Queued,
                ),
                (
                    MEETING_FAILED,
                    "Investor update prep",
                    "2026-09-28T13:46:40.000Z",
                    MeetingState::Failed {
                        reason: "Timed out".to_owned(),
                    },
                ),
            ] {
                store
                    .save_meeting(&meeting(
                        n,
                        title,
                        TitleOrigin::Calendar,
                        started_at,
                        600.0,
                        MeetingSource::MacCall,
                        &[],
                        state,
                        None,
                    ))
                    .unwrap();
            }
        })
        .build();
    let told: Arc<Mutex<Vec<TrayMeetings>>> = Arc::default();
    let sink = told.clone();
    harness.host.on_tray(Box::new(move |meetings| {
        sink.lock().unwrap().push(meetings);
    }));
    harness.host.apply_meeting_event(&MeetingEvent::Progress {
        meeting_id: uuid(0x30),
        progress: ProcessingProgress {
            stage: PipelineStage::Decode,
            fraction: 0.62,
            next_fraction: 0.8,
            estimated_remaining_seconds: 95.0,
            is_estimate_seeded: false,
            lane: 0,
            lane_count: 1,
        },
    });
    let told = told.lock().unwrap();
    assert_eq!(told.len(), 2, "once when attached, once for the event");
    let last = told.last().unwrap();
    assert_eq!(
        texts(&last.queue),
        [
            "Weekly sync — Decoding… 62%",
            "Standup — Waiting to process"
        ],
        "oldest first, with the stage once the run has one"
    );
    assert_eq!(
        texts(&last.recent),
        [
            "Produktstrategie 90/10",
            "Tuesday 10:08",
            "Investor update prep — Failed"
        ],
        "newest first"
    );
    assert_eq!(texts(&last.reviews), Vec::<&str>::new());
}

#[test]
fn a_review_stays_pending_until_every_speaker_is_confirmed() {
    let harness = Harness::builder().seed(populate_sample).build();
    harness
        .host
        .apply_meeting_event(&MeetingEvent::SpeakersNeedReview {
            meeting_id: uuid(MEETING),
            speaker_ids: vec![uuid(SPEAKER_ANNA), uuid(SPEAKER_UNKNOWN)],
        });
    harness.host.store_changed();
    assert_eq!(
        texts(&harness.host.tray_meetings().reviews),
        ["Review speakers in Produktstrategie 90/10"]
    );

    let anna = harness.store.person(uuid(PERSON_ANNA)).unwrap().unwrap();
    harness
        .store
        .confirm_speaker(uuid(SPEAKER_ANNA), &anna)
        .unwrap();
    harness.host.store_changed();
    assert_eq!(
        harness.host.tray_meetings().reviews.len(),
        1,
        "Speaker 4 is still unnamed"
    );

    harness
        .store
        .confirm_speaker(uuid(SPEAKER_UNKNOWN), &anna)
        .unwrap();
    harness.host.store_changed();
    assert_eq!(
        texts(&harness.host.tray_meetings().reviews),
        Vec::<&str>::new()
    );
}

#[test]
fn a_deleted_meeting_leaves_the_reviews() {
    let harness = Harness::builder().seed(populate_sample).build();
    harness
        .host
        .apply_meeting_event(&MeetingEvent::SpeakersNeedReview {
            meeting_id: uuid(MEETING),
            speaker_ids: vec![uuid(SPEAKER_UNKNOWN)],
        });
    harness.store.delete_meeting(uuid(MEETING)).unwrap();
    harness.host.store_changed();
    assert_eq!(
        texts(&harness.host.tray_meetings().reviews),
        Vec::<&str>::new()
    );
}
