//! The main window's behaviour through the host, ported from
//! `MainWindowBridgeTests`, `MeetingListViewModelTests`,
//! `MeetingDetailViewModelTests` and `DisplayTitleTests`.

// The ported suites keep one Swift test per function, long as some are.
#![allow(clippy::too_many_lines)]

mod common;

use steno_host::services::Recorder as _;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use common::*;
use serde_json::{Value, json};
use steno_bridge::{
    BridgeErrorCode, BridgeEvent, BridgeHost, BridgeTopic, BridgeWindow, DetailTab, EventSink,
    ListFilter, MeetingIdParams, OpenUrlParams, RecordingState, SaveNotesParams, SetBoolParams,
    SetFilterParams, SetQueryParams, SetTabParams, SetTagFilterParams, SetTagsParams,
    SetTemplateParams, StartRecordingParams, WindowParams,
};
use steno_core::{
    AudioRetention, Delivery, DeliveryStatus, MeetingEvent, MeetingOperation, MeetingSource,
    MeetingState, PipelineStage, TitleOrigin,
};
use steno_host::fakes::FakeServices;
use steno_host::host::TOPICS;
use steno_host::labels::{display_title, utc};
use steno_host::main_window::snapshots::{delivery_line, turns};
use steno_host::services::{AutoStopStatus, FileSystem as _, ProcessAgainRefusal as Refusal};

fn sample() -> Harness {
    Harness::builder()
        .seed(|store, fakes| {
            populate_sample(store, fakes);
            set_retention(store, AudioRetention::KeepDays(30));
        })
        .build()
}

fn ids(list: &Value) -> Vec<String> {
    list["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group["meetings"].as_array().unwrap().iter())
        .map(|row| row["id"].as_str().unwrap().to_owned())
        .collect()
}

fn id(n: u32) -> String {
    steno_core::json::uuid_string(uuid(n))
}

/// Swift: `anEmptyStorePublishesEveryTopicAndTheFirstMeetingFlipsTheApp`.
#[test]
fn page_ready_publishes_every_topic_once_and_the_first_meeting_flips_the_app() {
    let harness = Harness::builder().without_page_ready().build();
    harness
        .host
        .meetings_set_filter(SetFilterParams {
            filter: ListFilter::Ready,
        })
        .unwrap();
    assert!(
        harness.sink.events.lock().unwrap().is_empty(),
        "nothing before page.ready"
    );
    harness.host.page_ready().unwrap();
    assert_eq!(harness.sink.topics(), TOPICS.to_vec());
    assert_eq!(
        harness.sink.last(BridgeTopic::MeetingDetail),
        Some(Value::Null)
    );
    let app = harness.sink.last(BridgeTopic::App).unwrap();
    assert!(
        app.get("setupBanner").is_none(),
        "no banner without meetings"
    );
    assert_eq!(app["version"], "0.10.0");

    harness.store.save_meeting(&sample_meeting()).unwrap();
    harness.sink.clear();
    harness.host.store_changed();
    let list = harness.sink.last(BridgeTopic::MeetingsList).unwrap();
    assert_eq!(
        list["selection"],
        id(MEETING),
        "the first fill selects the newest meeting"
    );
    let app = harness.sink.last(BridgeTopic::App).unwrap();
    assert_eq!(app["setupBanner"]["title"], "Summaries and export are off.");
    let detail = harness.sink.last(BridgeTopic::MeetingDetail).unwrap();
    assert_eq!(detail["id"], id(MEETING));

    harness.host.setup_dismiss_banner().unwrap();
    assert!(
        harness
            .sink
            .last(BridgeTopic::App)
            .unwrap()
            .get("setupBanner")
            .is_none()
    );
}

/// Swift: `setFilterChangesTheListAndRepublishes`, `testFiltersByStateAndTagAndKeepsSelection`,
/// `testCountsIgnoreTheTagFilterAndTheSettersClearIt`, `theThreeFilterSettersClearEveryFilter`.
#[test]
fn filters_narrow_the_list_keep_the_selection_and_the_counts() {
    let harness = sample();
    let list = harness.snapshot(BridgeTopic::MeetingsList);
    assert_eq!(
        ids(&list),
        [id(MEETING), id(MEETING_IN_PERSON), id(MEETING_FAILED)],
        "newest first"
    );
    assert_eq!(list["groups"][0]["day"], "2026-09-29");
    assert_eq!(list["groups"][1]["day"], "2026-09-28");
    assert_eq!(list["selection"], id(MEETING));

    harness.sink.clear();
    harness
        .host
        .meetings_set_filter(SetFilterParams {
            filter: ListFilter::Failed,
        })
        .unwrap();
    let list = harness.sink.last(BridgeTopic::MeetingsList).unwrap();
    assert_eq!(ids(&list), [id(MEETING_FAILED)]);
    assert_eq!(
        list["selection"],
        id(MEETING),
        "a filter never drops the selection"
    );
    assert_eq!(
        list["counts"],
        json!({"all": 3, "failed": 1, "processing": 0, "ready": 2})
    );
    assert_eq!(list["filter"], "failed");

    harness
        .host
        .meetings_set_filter(SetFilterParams {
            filter: ListFilter::All,
        })
        .unwrap();
    harness
        .host
        .meetings_set_tag_filter(SetTagFilterParams {
            tag: Some("q4".to_owned()),
        })
        .unwrap();
    let list = harness.sink.last(BridgeTopic::MeetingsList).unwrap();
    assert_eq!(ids(&list), [id(MEETING)]);
    assert_eq!(list["counts"]["all"], 3, "counts ignore the tag filter");
    assert_eq!(list["tagFilter"], "q4");

    harness
        .host
        .meetings_set_tag_filter(SetTagFilterParams { tag: None })
        .unwrap();
    harness
        .host
        .meetings_set_query(SetQueryParams {
            query: "Prioritäten".to_owned(),
        })
        .unwrap();
    let list = harness.sink.last(BridgeTopic::MeetingsList).unwrap();
    assert_eq!(ids(&list), [id(MEETING)], "FTS over the transcript");
    harness
        .host
        .meetings_set_query(SetQueryParams {
            query: "prep".to_owned(),
        })
        .unwrap();
    assert_eq!(
        ids(&harness.sink.last(BridgeTopic::MeetingsList).unwrap()),
        [id(MEETING_FAILED)],
        "FTS over the title"
    );
    harness
        .host
        .meetings_set_query(SetQueryParams {
            query: "nothing matches".to_owned(),
        })
        .unwrap();
    assert_eq!(
        ids(&harness.sink.last(BridgeTopic::MeetingsList).unwrap()),
        Vec::<String>::new()
    );

    // The three setters back to their defaults clear every filter.
    harness
        .host
        .meetings_set_query(SetQueryParams {
            query: String::new(),
        })
        .unwrap();
    let list = harness.sink.last(BridgeTopic::MeetingsList).unwrap();
    assert_eq!(ids(&list).len(), 3);
    assert_eq!(list["query"], "");
}

/// Swift: `selectingAnUnlistedMeetingWaitsForItsRow`, `testASelectionMadeAheadOfItsRowSurvivesTheNextUpdate`.
#[test]
fn selecting_an_unlisted_meeting_waits_for_its_row() {
    let harness = sample();
    let new_id = uuid(0x99);
    harness.sink.clear();
    harness
        .host
        .meetings_select(MeetingIdParams { meeting_id: new_id })
        .unwrap();
    let list = harness.sink.last(BridgeTopic::MeetingsList).unwrap();
    assert_eq!(list["selection"], id(MEETING), "the selection waits");

    let mut late = sample_meeting();
    late.id = new_id;
    late.title = "Late".to_owned();
    late.tags = Vec::new();
    harness.store.save_meeting(&late).unwrap();
    harness.host.store_changed();
    let list = harness.sink.last(BridgeTopic::MeetingsList).unwrap();
    assert_eq!(list["selection"], id(0x99));
    assert_eq!(
        harness.sink.last(BridgeTopic::MeetingDetail).unwrap()["title"],
        "Late"
    );

    // A listed meeting selects at once.
    harness
        .host
        .meetings_select(MeetingIdParams {
            meeting_id: uuid(MEETING_FAILED),
        })
        .unwrap();
    let detail = harness.sink.last(BridgeTopic::MeetingDetail).unwrap();
    assert_eq!(detail["state"], "failed");
    assert_eq!(
        detail["failureReason"],
        "Transcription failed: model not installed"
    );
    assert_eq!(detail["summaryStatus"]["kind"], "pending");
}

/// Swift: `deleteAsksFirst`, `testDeleteRemovesTheMeetingAndRefusesABusyOne`.
#[test]
fn delete_asks_first_and_refuses_a_busy_meeting() {
    let harness = Harness::builder()
        .confirm(false)
        .seed(|store, fakes| {
            populate_sample(store, fakes);
            let mut processing = sample_meeting();
            processing.id = uuid(0x77);
            processing.state = MeetingState::Processing;
            processing.title = "Still processing".to_owned();
            store.save_meeting(&processing).unwrap();
        })
        .build();
    let reply = harness
        .host
        .meetings_delete(MeetingIdParams {
            meeting_id: uuid(MEETING),
        })
        .unwrap();
    assert!(!reply.confirmed);
    assert!(
        harness.store.meeting(uuid(MEETING)).unwrap().is_some(),
        "a decline changes nothing"
    );

    let busy = harness
        .host
        .meetings_delete(MeetingIdParams {
            meeting_id: uuid(0x77),
        })
        .unwrap_err();
    assert_eq!(busy.code, BridgeErrorCode::Failed);
    assert_eq!(busy.message, "This meeting is still being processed.");
    let missing = harness
        .host
        .meetings_delete(MeetingIdParams {
            meeting_id: uuid(0xEE),
        })
        .unwrap_err();
    assert_eq!(missing.code, BridgeErrorCode::NotFound);

    let confirming = Harness::builder()
        .confirm(true)
        .seed(populate_sample)
        .build();
    let _ = confirming.snapshot(BridgeTopic::MeetingsList);
    confirming.sink.clear();
    // Whether each commit saw the entry already forgotten: the delete's
    // commit must, since the launch's adoption reads the entries after the
    // rows.
    let forgotten_at_commit: Arc<Mutex<Vec<bool>>> = Arc::default();
    let (seen, recorder) = (
        forgotten_at_commit.clone(),
        confirming.fakes.recorder.clone(),
    );
    confirming.store.probe_commits(move |_| {
        let forgotten = recorder.forgotten.lock().unwrap().contains(&uuid(MEETING));
        seen.lock().unwrap().push(forgotten);
    });
    let audio = confirming.audio_folder();
    confirming
        .fakes
        .recorder
        .recorded
        .lock()
        .unwrap()
        .insert(uuid(MEETING), audio);
    let reply = confirming
        .host
        .meetings_delete(MeetingIdParams {
            meeting_id: uuid(MEETING),
        })
        .unwrap();
    assert!(reply.confirmed);
    assert!(confirming.store.meeting(uuid(MEETING)).unwrap().is_none());
    assert!(confirming.store.asset(uuid(MEETING)).unwrap().is_none());
    assert_eq!(confirming.store.persons().unwrap().len(), 3, "people stay");
    // The meeting folder goes whole, through the file system seam. An
    // asset names it, so the recorder is not asked where else it may be;
    // it forgets what it kept for the meeting's recovery.
    let folder = confirming
        .audio_folder()
        .join(steno_core::json::uuid_string(uuid(MEETING)));
    assert_eq!(
        *confirming.fakes.file_system.removed.lock().unwrap(),
        vec![folder.clone()]
    );
    assert_eq!(
        *confirming.fakes.recorder.asked.lock().unwrap(),
        Vec::<uuid::Uuid>::new()
    );
    assert_eq!(
        *confirming.fakes.recorder.forgotten.lock().unwrap(),
        [uuid(MEETING)]
    );
    assert_eq!(*forgotten_at_commit.lock().unwrap(), [true]);
    assert!(
        confirming
            .fakes
            .recorder
            .recorded
            .lock()
            .unwrap()
            .is_empty(),
        "a delete that went through does not record the entry again"
    );
    assert!(
        !confirming
            .fakes
            .file_system
            .exists(&master_path(&confirming.audio_folder()))
    );
    assert!(
        !confirming
            .fakes
            .file_system
            .exists(&clip_path(&confirming.audio_folder(), SPEAKER_NICOLAI))
    );
    let list = confirming.sink.last(BridgeTopic::MeetingsList).unwrap();
    assert_eq!(ids(&list).len(), 2);
    assert!(
        list.get("selection").is_none(),
        "the deleted selection clears"
    );
    assert_eq!(
        confirming.sink.last(BridgeTopic::MeetingDetail),
        Some(Value::Null)
    );

    // A queued meeting has a progress entry; deleting it evicts the entry,
    // as the store's `deleted` event did (`ProcessingProgressModel.apply`).
    let queued = Harness::builder()
        .confirm(true)
        .seed(|store, _| {
            let mut meeting = sample_meeting();
            meeting.id = uuid(0x88);
            meeting.state = MeetingState::Queued;
            meeting.summary = None;
            store.save_meeting(&meeting).unwrap();
        })
        .build();
    assert_eq!(
        queued.snapshot(BridgeTopic::Progress)["entries"][0]["meetingID"],
        id(0x88)
    );
    queued.sink.clear();
    queued
        .host
        .meetings_delete(MeetingIdParams {
            meeting_id: uuid(0x88),
        })
        .unwrap();
    assert_eq!(
        queued.snapshot(BridgeTopic::Progress)["entries"],
        json!([]),
        "the progress entry goes with the meeting"
    );
    // No asset names its files: the recorder says where its folder may be.
    assert_eq!(*queued.fakes.recorder.asked.lock().unwrap(), [uuid(0x88)]);
    assert_eq!(queued.sink.count(BridgeTopic::Progress), 1);
}

/// A file that will not go is reported once the rows are gone (no sweep
/// finds that audio again), as `MeetingStore.delete` threw its first
/// failure; a delete the store refuses keeps the meeting's progress entry.
#[test]
fn a_delete_reports_files_that_stay_and_a_refusal_keeps_the_progress_entry() {
    let harness = Harness::builder()
        .seed(|store, fakes| {
            populate_sample(store, fakes);
            fakes
                .file_system
                .fail_removals(Some("Operation not permitted"));
        })
        .build();
    let reply = harness
        .host
        .meetings_delete(MeetingIdParams {
            meeting_id: uuid(MEETING),
        })
        .unwrap();
    assert!(reply.confirmed);
    assert!(harness.store.meeting(uuid(MEETING)).unwrap().is_none());
    let list = harness.sink.last(BridgeTopic::MeetingsList).unwrap();
    assert_eq!(
        list["error"],
        "Meeting could not be deleted: Operation not permitted"
    );
    assert!(
        harness
            .fakes
            .file_system
            .exists(&master_path(&harness.audio_folder())),
        "the master is still on disk, and the page says so"
    );

    // The meeting starts processing while the prompt is up: the store
    // refuses, the reason shows, and the queued entry is not evicted.
    let refused = Harness::builder()
        .confirm_with(|host, _| {
            let mut meeting = host.store().meeting(uuid(0x88)).unwrap().unwrap();
            meeting.state = MeetingState::Processing;
            host.store().save_meeting(&meeting).unwrap();
            true
        })
        .seed(|store, _| {
            let mut meeting = sample_meeting();
            meeting.id = uuid(0x88);
            meeting.state = MeetingState::Queued;
            meeting.summary = None;
            store.save_meeting(&meeting).unwrap();
        })
        .build();
    refused
        .host
        .meetings_delete(MeetingIdParams {
            meeting_id: uuid(0x88),
        })
        .unwrap();
    assert!(refused.store.meeting(uuid(0x88)).unwrap().is_some());
    assert_eq!(
        refused.sink.last(BridgeTopic::MeetingsList).unwrap()["error"]
            .as_str()
            .map(|error| error.starts_with("Meeting could not be deleted: ")),
        Some(true)
    );
    assert_eq!(
        refused.snapshot(BridgeTopic::Progress)["entries"][0]["meetingID"],
        id(0x88),
        "the refused meeting keeps its progress entry"
    );
}

/// A confirmed delete the store refuses (the pipeline took the meeting up
/// while the prompt was open) records the entry the recorder forgot for
/// it again: the row stays, and until it is durable that entry is what
/// lets a launch adopt its master. One whose entry cannot be forgotten is
/// refused before any row goes.
#[test]
fn a_delete_that_does_not_go_through_keeps_the_recorders_entry() {
    let refused = Harness::builder()
        .confirm_with(|host, _| {
            let mut meeting = host.store().meeting(uuid(MEETING)).unwrap().unwrap();
            meeting.state = MeetingState::Processing;
            host.store().save_meeting(&meeting).unwrap();
            true
        })
        .seed(populate_sample)
        .build();
    let audio = refused.audio_folder();
    refused
        .fakes
        .recorder
        .recorded
        .lock()
        .unwrap()
        .insert(uuid(MEETING), audio.clone());
    let reply = refused
        .host
        .meetings_delete(MeetingIdParams {
            meeting_id: uuid(MEETING),
        })
        .unwrap();
    assert!(reply.confirmed);
    assert!(refused.store.meeting(uuid(MEETING)).unwrap().is_some());
    assert_eq!(
        *refused.fakes.recorder.forgotten.lock().unwrap(),
        [uuid(MEETING)]
    );
    assert_eq!(
        refused
            .fakes
            .recorder
            .recorded
            .lock()
            .unwrap()
            .get(&uuid(MEETING)),
        Some(&audio),
        "the entry is recorded again"
    );

    let unforgotten = Harness::builder()
        .confirm(true)
        .seed(|store, fakes| {
            populate_sample(store, fakes);
            *fakes.recorder.forget_fails.lock().unwrap() = true;
        })
        .build();
    let error = unforgotten
        .host
        .meetings_delete(MeetingIdParams {
            meeting_id: uuid(MEETING),
        })
        .unwrap_err();
    assert_eq!(error.code, BridgeErrorCode::Failed);
    assert_eq!(error.message, "Steno could not delete this meeting.");
    assert!(unforgotten.store.meeting(uuid(MEETING)).unwrap().is_some());
    assert!(unforgotten.store.asset(uuid(MEETING)).unwrap().is_some());
    assert!(
        unforgotten
            .fakes
            .file_system
            .removed
            .lock()
            .unwrap()
            .is_empty()
    );
}

/// A delete that a panic unwinds before its commit (here inside the
/// write, as the commit runs) keeps the row, and the recorder's entry is
/// recorded again as the drop guard unwinds.
#[test]
fn a_delete_unwound_before_its_commit_keeps_the_recorders_entry() {
    let armed = Arc::new(AtomicBool::new(false));
    let arm = armed.clone();
    let harness = Harness::builder()
        .confirm_with(move |_, _| {
            arm.store(true, Ordering::SeqCst);
            true
        })
        .seed(populate_sample)
        .build();
    harness.store.probe_commits(move |_| {
        assert!(
            !armed.swap(false, Ordering::SeqCst),
            "a panic before the delete's commit"
        );
    });
    let audio = harness.audio_folder();
    harness
        .fakes
        .recorder
        .recorded
        .lock()
        .unwrap()
        .insert(uuid(MEETING), audio.clone());
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        harness.host.meetings_delete(MeetingIdParams {
            meeting_id: uuid(MEETING),
        })
    }));
    assert!(unwound.is_err());
    assert!(harness.store.meeting(uuid(MEETING)).unwrap().is_some());
    assert_eq!(
        harness
            .fakes
            .recorder
            .recorded
            .lock()
            .unwrap()
            .get(&uuid(MEETING)),
        Some(&audio),
        "the entry is recorded again"
    );
}

/// Two delete prompts for one meeting: the second, answered after the
/// first deleted the meeting, finds no row and records no entry for a
/// meeting that is gone.
#[test]
fn a_second_delete_prompt_records_no_entry_for_a_deleted_meeting() {
    let harness = Harness::builder()
        .confirm_with(|host, _| {
            // The first prompt's delete, done while this one is open.
            host.store().delete_meeting(uuid(MEETING)).unwrap();
            true
        })
        .seed(populate_sample)
        .build();
    let audio = harness.audio_folder();
    harness
        .fakes
        .recorder
        .recorded
        .lock()
        .unwrap()
        .insert(uuid(MEETING), audio);
    let reply = harness
        .host
        .meetings_delete(MeetingIdParams {
            meeting_id: uuid(MEETING),
        })
        .unwrap();
    assert!(reply.confirmed);
    assert!(harness.store.meeting(uuid(MEETING)).unwrap().is_none());
    assert_eq!(
        *harness.fakes.recorder.forgotten.lock().unwrap(),
        [uuid(MEETING)]
    );
    assert!(harness.fakes.recorder.recorded.lock().unwrap().is_empty());
}

/// A refused delete reads the row and restores its entry under the host
/// lock: a second prompt's delete, begun between the two, waits for the
/// restore and then forgets that entry with the rows, so the deleted
/// meeting keeps no entry.
#[test]
fn a_second_delete_waits_for_a_refused_deletes_restore() {
    let first = Arc::new(AtomicBool::new(true));
    let harness = Harness::builder()
        .confirm_with(move |host, _| {
            if first.swap(false, Ordering::SeqCst) {
                // The pipeline takes the meeting up while the first prompt
                // is open.
                let mut meeting = host.store().meeting(uuid(MEETING)).unwrap().unwrap();
                meeting.state = MeetingState::Processing;
                host.store().save_meeting(&meeting).unwrap();
            }
            true
        })
        .seed(populate_sample)
        .build();
    let audio = harness.audio_folder();
    harness
        .fakes
        .recorder
        .recorded
        .lock()
        .unwrap()
        .insert(uuid(MEETING), audio);
    let host = harness.host.clone();
    let second = Arc::new(Mutex::new(None));
    let spawned = second.clone();
    *harness.fakes.recorder.before_restore.lock().unwrap() = Some(Box::new(move || {
        // The pipeline is done with it, and a second prompt deletes it.
        let mut meeting = host.store().meeting(uuid(MEETING)).unwrap().unwrap();
        meeting.state = MeetingState::Ready;
        host.store().save_meeting(&meeting).unwrap();
        let (done, finished) = std::sync::mpsc::channel();
        *spawned.lock().unwrap() = Some(std::thread::spawn(move || {
            host.store_changed();
            let reply = host.meetings_delete(MeetingIdParams {
                meeting_id: uuid(MEETING),
            });
            let _ = done.send(());
            reply
        }));
        // Long enough for that delete to finish if nothing held it back.
        let _ = finished.recv_timeout(std::time::Duration::from_millis(500));
    }));
    let reply = harness
        .host
        .meetings_delete(MeetingIdParams {
            meeting_id: uuid(MEETING),
        })
        .unwrap();
    assert!(reply.confirmed);
    let second = second.lock().unwrap().take().expect("a second prompt");
    assert!(second.join().unwrap().unwrap().confirmed);
    assert!(harness.store.meeting(uuid(MEETING)).unwrap().is_none());
    assert!(harness.fakes.recorder.recorded.lock().unwrap().is_empty());
}

/// The detail heading derives the title as the list does (Swift:
/// `meeting.displayTitle()`), so an untitled meeting reads "Monday 10:06".
#[test]
fn the_detail_heading_derives_an_untitled_meetings_title() {
    let harness = Harness::builder()
        .seed(|store, _| {
            store
                .save_meeting(&meeting(
                    0x66,
                    "Recording",
                    TitleOrigin::Default,
                    "2026-09-28T08:06:00.000Z",
                    60.0,
                    MeetingSource::MacInPerson,
                    &[],
                    MeetingState::Ready,
                    None,
                ))
                .unwrap();
        })
        .build();
    let _ = harness.snapshot(BridgeTopic::MeetingsList);
    let detail = harness.snapshot(BridgeTopic::MeetingDetail);
    assert_eq!(detail["title"], "Monday 08:06");
    assert_eq!(
        harness.snapshot(BridgeTopic::MeetingsList)["groups"][0]["meetings"][0]["title"],
        "Monday 08:06",
        "the same title as the list row"
    );
}

/// A sink that reads the host back from inside `emit`: the Tauri shell
/// serialises on the webview thread, and the host must not hold its lock
/// while it does.
struct ReentrantSink {
    host: Mutex<Option<steno_host::Host>>,
    seen: Mutex<Vec<(BridgeTopic, Option<Value>)>>,
}

impl EventSink for ReentrantSink {
    fn emit(&self, event: BridgeEvent) {
        let read = self
            .host
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|host| host.snapshot(event.topic));
        self.seen.lock().unwrap().push((event.topic, read));
    }
}

#[test]
fn snapshots_are_emitted_outside_the_hosts_lock() {
    let harness = sample();
    let sink = Arc::new(ReentrantSink {
        host: Mutex::new(Some(harness.host.clone())),
        seen: Mutex::new(Vec::new()),
    });
    harness.host.attach(sink.clone());
    // A new selection publishes the list, then the detail it swapped in.
    // An emit under the lock deadlocks the sink's read: fail, do not hang.
    let host = harness.host.clone();
    within_five_seconds("the selection", move || {
        host.meetings_select(MeetingIdParams {
            meeting_id: uuid(MEETING_FAILED),
        })
    })
    .unwrap();
    let seen = sink.seen.lock().unwrap();
    assert!(!seen.is_empty(), "the command published");
    for (topic, read) in seen.iter() {
        assert!(read.is_some(), "{topic:?} could be read back from emit");
    }
    assert_eq!(
        seen.iter().map(|(topic, _)| *topic).collect::<Vec<_>>(),
        vec![BridgeTopic::MeetingsList, BridgeTopic::MeetingDetail],
        "in publish order"
    );
}

/// A sink as slow as a window channel can be: it sleeps 0 to 7 ms per emit,
/// spread by a hash of the emit count, and keeps the payloads per topic in
/// arrival order.
#[derive(Default)]
struct SlowSink {
    emits: std::sync::atomic::AtomicU64,
    payloads: Mutex<std::collections::BTreeMap<BridgeTopic, Vec<Value>>>,
}

impl EventSink for SlowSink {
    fn emit(&self, event: BridgeEvent) {
        let n = self
            .emits
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let spread = n.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 61;
        std::thread::sleep(std::time::Duration::from_millis(spread));
        self.payloads
            .lock()
            .unwrap()
            .entry(event.topic)
            .or_default()
            .push(event.payload);
    }
}

/// Snapshots are built under the lock and emitted after it; only the
/// publishing mutex keeps two publishers from emitting in the opposite
/// order to the one they built in, which would leave the page on an older
/// state. Two threads raise a counter (the meeting count, the recorder's
/// warning) and notify; three more publish the same topics as fast as they
/// can; per topic, what reaches the sink never goes backwards.
#[test]
fn concurrent_publishes_reach_the_sink_in_build_order() {
    const WRITES: u32 = 40;
    let harness = sample();
    let sink = Arc::new(SlowSink::default());
    harness.host.attach(sink.clone());
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for n in 1..=WRITES {
                harness
                    .store
                    .save_meeting(&meeting(
                        0x1000 + n,
                        &format!("Write {n}"),
                        TitleOrigin::User,
                        "2026-09-28T10:00:00.000Z",
                        60.0,
                        MeetingSource::MacCall,
                        &[],
                        MeetingState::Ready,
                        None,
                    ))
                    .unwrap();
                harness.host.store_changed();
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            done.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        scope.spawn(|| {
            let mut n = 0;
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                n += 1;
                harness
                    .fakes
                    .recorder
                    .set_messages(Some(&format!("w{n:06}")), None);
                harness.host.recorder_changed();
                std::thread::sleep(std::time::Duration::from_micros(300));
            }
        });
        for _ in 0..3 {
            scope.spawn(|| {
                while !done.load(std::sync::atomic::Ordering::SeqCst) {
                    harness
                        .host
                        .meetings_set_filter(SetFilterParams {
                            filter: ListFilter::All,
                        })
                        .unwrap();
                    harness.host.recorder_changed();
                }
            });
        }
    });
    harness.wait_for("the throttled recording to flush", |harness| {
        harness.host.next_flush_due().is_none()
    });

    let payloads = sink.payloads.lock().unwrap();
    let counts: Vec<i64> = payloads[&BridgeTopic::MeetingsList]
        .iter()
        .map(|list| list["counts"]["all"].as_i64().unwrap())
        .collect();
    assert!(
        counts.windows(2).all(|pair| pair[0] <= pair[1]),
        "the meeting count went backwards: {counts:?}"
    );
    assert_eq!(counts.last(), Some(&(3 + i64::from(WRITES))));
    let warnings: Vec<&str> = payloads[&BridgeTopic::Recording]
        .iter()
        .filter_map(|recording| recording["warning"].as_str())
        .collect();
    assert!(
        warnings.windows(2).all(|pair| pair[0] <= pair[1]),
        "the recorder's warning went backwards: {warnings:?}"
    );
}

/// `meeting.revealRecording`, `meeting.revealExport`, `page.layout`,
/// `recording.keepGoing` and `recording.clearMessages` through the host.
#[test]
fn reveal_layout_and_the_recorder_messages_reach_their_services() {
    let harness = sample();
    harness.host.meeting_reveal_recording().unwrap();
    assert_eq!(
        *harness.fakes.opener.revealed.lock().unwrap(),
        vec![master_path(&harness.audio_folder())]
    );
    harness
        .fakes
        .file_system
        .remove(&master_path(&harness.audio_folder()))
        .unwrap();
    harness.host.store_changed();
    let gone = harness.host.meeting_reveal_recording().unwrap_err();
    assert_eq!(gone.code, BridgeErrorCode::NotFound);
    assert_eq!(gone.message, "The recording is no longer on this Mac.");
    assert_eq!(harness.fakes.opener.revealed.lock().unwrap().len(), 1);

    harness
        .store
        .save_delivery(&Delivery {
            id: Delivery::id_for(uuid(MEETING), "obsidian-folder"),
            meeting_id: uuid(MEETING),
            destination_id: "obsidian-folder".to_owned(),
            status: DeliveryStatus::Delivered,
            last_attempt_at: Some(now()),
            receipt: Some(steno_core::DeliveryReceipt {
                root: "/Users/nicolai/Notes".to_owned(),
                folder: "Meetings/2026-09-29 Produktstrategie".to_owned(),
                files: Vec::new(),
                renderer_version: 1,
                warnings: Vec::new(),
            }),
        })
        .unwrap();
    harness.host.store_changed();
    harness.host.meeting_reveal_export().unwrap();
    assert_eq!(
        harness.fakes.opener.revealed.lock().unwrap()[1],
        std::path::PathBuf::from("/Users/nicolai/Notes/Meetings/2026-09-29 Produktstrategie")
    );

    harness.sink.clear();
    harness
        .host
        .page_layout(steno_bridge::PageLayoutParams {
            window: BridgeWindow::Main,
            width: 1_024.0,
            height: 768.0,
        })
        .unwrap();
    assert!(
        harness.sink.events.lock().unwrap().is_empty(),
        "validated and dropped: nothing publishes"
    );

    harness
        .fakes
        .recorder
        .set_messages(Some("Low input level."), Some("Tap lost."));
    harness.fakes.recorder.set_auto_stop(Some(AutoStopStatus {
        app_name: Some("Zoom".to_owned()),
        remaining_seconds: 42.0,
        total_seconds: 90.0,
    }));
    let recording = harness.snapshot(BridgeTopic::Recording);
    assert_eq!(recording["warning"], "Low input level.");
    assert_eq!(recording["error"], "Tap lost.");
    assert_eq!(recording["autoStop"]["remainingSeconds"], 42.0);
    harness.host.recording_keep_going().unwrap();
    assert_eq!(*harness.fakes.recorder.kept.lock().unwrap(), 1);
    assert!(
        harness
            .snapshot(BridgeTopic::Recording)
            .get("autoStop")
            .is_none(),
        "keep going disarms the auto-stop"
    );
    harness.host.recording_clear_messages().unwrap();
    let recording = harness.snapshot(BridgeTopic::Recording);
    assert!(recording.get("warning").is_none());
    assert!(recording.get("error").is_none());
}

/// Swift: `notesLandOnTheMeetingTheyWereTypedFor`.
#[test]
fn notes_land_on_the_meeting_they_were_typed_for() {
    let harness = sample();
    harness
        .host
        .meeting_save_notes(SaveNotesParams {
            meeting_id: uuid(MEETING_FAILED),
            text: "Follow up with Anna on Monday.".to_owned(),
        })
        .unwrap();
    assert_eq!(
        harness
            .store
            .meeting(uuid(MEETING_FAILED))
            .unwrap()
            .unwrap()
            .scratchpad,
        "Follow up with Anna on Monday."
    );
    assert_eq!(
        harness
            .store
            .meeting(uuid(MEETING))
            .unwrap()
            .unwrap()
            .scratchpad,
        "",
        "not the selection"
    );
    let missing = harness
        .host
        .meeting_save_notes(SaveNotesParams {
            meeting_id: uuid(0xEE),
            text: "x".to_owned(),
        })
        .unwrap_err();
    assert_eq!(missing.code, BridgeErrorCode::NotFound);
    harness
        .host
        .meeting_save_notes(SaveNotesParams {
            meeting_id: uuid(MEETING),
            text: "mine".to_owned(),
        })
        .unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::MeetingDetail)["notes"],
        "mine"
    );
}

/// Swift: `methodsOfOtherWindowsAndOfNoSelectionAreTypedErrors`.
#[test]
fn methods_without_a_selection_are_typed_errors() {
    let harness = Harness::builder().build();
    let error = harness
        .host
        .meeting_set_tab(SetTabParams {
            tab: DetailTab::Transcript,
        })
        .unwrap_err();
    assert_eq!(error.code, BridgeErrorCode::NotFound);
    assert_eq!(error.message, "No meeting is selected.");
    assert_eq!(
        harness.host.meeting_rerun_summary().unwrap_err().code,
        BridgeErrorCode::NotFound
    );
    assert_eq!(
        harness
            .host
            .meeting_delete_recording_now()
            .unwrap_err()
            .code,
        BridgeErrorCode::NotFound
    );
    assert_eq!(
        harness.host.speakers_stop().unwrap_err().code,
        BridgeErrorCode::NotFound
    );
}

/// Swift: `testTagsTypedAreNormalised`, `testTemplateChangeRerunsSummaryAndReexportRedelivers`,
/// `testRunSummaryFailureIsReported`.
#[test]
fn tags_templates_and_reruns_reach_the_store_and_the_pipeline() {
    let harness = sample();
    harness.sink.clear();
    harness
        .host
        .meeting_set_tab(SetTabParams {
            tab: DetailTab::Transcript,
        })
        .unwrap();
    assert_eq!(
        harness.sink.last(BridgeTopic::MeetingDetail).unwrap()["tab"],
        "transcript"
    );

    harness
        .host
        .meeting_set_tags(SetTagsParams {
            tags: vec![
                "Q4".to_owned(),
                " q4 ".to_owned(),
                "Strategie".to_owned(),
                String::new(),
            ],
        })
        .unwrap();
    assert_eq!(
        harness.store.meeting(uuid(MEETING)).unwrap().unwrap().tags,
        ["q4", "strategie"]
    );
    let list = harness.sink.last(BridgeTopic::MeetingsList).unwrap();
    assert_eq!(
        list["groups"][0]["meetings"][0]["tags"],
        json!(["q4", "strategie"])
    );

    harness
        .host
        .meeting_set_template(SetTemplateParams {
            template_id: "daily-standup".to_owned(),
        })
        .unwrap();
    assert_eq!(
        harness
            .store
            .meeting(uuid(MEETING))
            .unwrap()
            .unwrap()
            .template_id,
        "daily-standup"
    );
    assert_eq!(
        *harness.fakes.pipeline.summary_reruns.lock().unwrap(),
        vec![(uuid(MEETING), "daily-standup".to_owned())]
    );
    harness
        .host
        .meeting_set_template(SetTemplateParams {
            template_id: "nope".to_owned(),
        })
        .unwrap();
    assert_eq!(
        harness
            .store
            .meeting(uuid(MEETING))
            .unwrap()
            .unwrap()
            .template_id,
        "daily-standup",
        "an unknown template changes nothing"
    );

    harness.host.meeting_reexport().unwrap();
    assert_eq!(
        *harness.fakes.pipeline.redeliveries.lock().unwrap(),
        vec![uuid(MEETING)]
    );

    harness
        .fakes
        .pipeline
        .fail_calls(Some("the endpoint did not answer"));
    harness.host.meeting_rerun_summary().unwrap();
    let detail = harness.sink.last(BridgeTopic::MeetingDetail).unwrap();
    assert_eq!(
        detail["error"],
        "Summary re-run failed: the endpoint did not answer"
    );
    assert_eq!(detail["isBusy"], false);
}

/// A re-run or a re-export that fails after the pipeline accepted it
/// arrives as `OperationFailed`; the selected meeting's detail shows it on
/// its error line, another meeting's failure leaves the line alone.
#[test]
fn a_background_failure_of_the_selected_meeting_reaches_the_detail_error_line() {
    let harness = sample();
    harness.host.meeting_reexport().unwrap();
    assert_eq!(
        harness.sink.last(BridgeTopic::MeetingDetail).unwrap()["error"],
        Value::Null
    );
    harness.sink.clear();
    harness
        .host
        .apply_meeting_event(&MeetingEvent::OperationFailed {
            meeting_id: uuid(MEETING_IN_PERSON),
            operation: MeetingOperation::Reexport,
            stage: PipelineStage::Deliver,
            failure: "deliver: the vault is gone".to_owned(),
        });
    assert!(
        harness.sink.last(BridgeTopic::MeetingDetail).is_none(),
        "another meeting's failure is not the detail's"
    );
    harness
        .host
        .apply_meeting_event(&MeetingEvent::OperationFailed {
            meeting_id: uuid(MEETING),
            operation: MeetingOperation::Reexport,
            stage: PipelineStage::Deliver,
            failure: "deliver: the vault is gone".to_owned(),
        });
    assert_eq!(
        harness.sink.last(BridgeTopic::MeetingDetail).unwrap()["error"],
        "Re-export failed: deliver: the vault is gone"
    );
    harness
        .host
        .apply_meeting_event(&MeetingEvent::OperationFailed {
            meeting_id: uuid(MEETING),
            operation: MeetingOperation::SummaryRerun,
            stage: PipelineStage::Summarize,
            failure: "summarize: HTTP 401".to_owned(),
        });
    let detail = harness.sink.last(BridgeTopic::MeetingDetail).unwrap();
    assert_eq!(
        detail["error"],
        "Summary re-run failed: summarize: HTTP 401"
    );
    assert_eq!(detail["isBusy"], false);
}

/// Swift: `testKeepAudioTogglesRetention`, `testKeepToggleShowsOnlyWhenTheDefaultIsNotForever`,
/// `testTurningKeepOffUnderDeleteAfterProcessingAsksFirst`.
#[test]
fn keep_audio_goes_through_the_pipeline_and_asks_before_deleting_now() {
    let harness = sample();
    let detail = harness.snapshot(BridgeTopic::MeetingDetail);
    assert_eq!(detail["retention"]["showsKeepToggle"], true);
    assert_eq!(detail["retention"]["kind"], "deletesOn");
    assert_eq!(detail["retention"]["keepsAudio"], false);

    let reply = harness
        .host
        .meeting_set_keep_audio(SetBoolParams { value: true })
        .unwrap();
    assert!(reply.confirmed);
    assert_eq!(
        *harness.fakes.pipeline.retention.lock().unwrap(),
        vec![(uuid(MEETING), AudioRetention::KeepForever)]
    );
    let reply = harness
        .host
        .meeting_set_keep_audio(SetBoolParams { value: false })
        .unwrap();
    assert!(reply.confirmed, "off under a days rule applies at once");
    assert_eq!(
        harness.fakes.pipeline.retention.lock().unwrap()[1],
        (uuid(MEETING), AudioRetention::KeepDays(30))
    );

    // Under "until processed, then delete" with a ready meeting and nothing
    // left to export, turning keep off asks first; a decline changes nothing.
    set_retention(&harness.store, AudioRetention::DeleteAfterProcessing);
    harness.host.store_changed();
    let declining = Harness::builder()
        .confirm(false)
        .seed(|store, fakes| {
            populate_sample(store, fakes);
            set_retention(store, AudioRetention::DeleteAfterProcessing);
        })
        .build();
    let _ = declining.snapshot(BridgeTopic::MeetingDetail);
    let reply = declining
        .host
        .meeting_set_keep_audio(SetBoolParams { value: false })
        .unwrap();
    assert!(!reply.confirmed);
    assert!(
        declining
            .fakes
            .pipeline
            .retention
            .lock()
            .unwrap()
            .is_empty()
    );
    let reply = declining.host.meeting_delete_recording_now().unwrap();
    assert!(!reply.confirmed);

    let reply = harness.host.meeting_delete_recording_now().unwrap();
    assert!(reply.confirmed);
    assert_eq!(
        harness.fakes.pipeline.retention.lock().unwrap()[2],
        (uuid(MEETING), AudioRetention::DeleteAfterProcessing)
    );

    // Forever as the default hides the toggle.
    set_retention(&harness.store, AudioRetention::KeepForever);
    harness.host.store_changed();
    assert_eq!(
        harness.snapshot(BridgeTopic::MeetingDetail)["retention"]["showsKeepToggle"],
        false
    );
}

/// `ui.confirmDestructive` hands the page's prompt to the shell's dialog
/// as sent and replies with the answer, a decline included.
#[test]
fn the_pages_destructive_prompt_reaches_the_dialog() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let harness = Harness::builder()
        .confirm_with({
            let asked = asked.clone();
            move |_, params| {
                asked.lock().unwrap().push(params.clone());
                false
            }
        })
        .build();
    let prompt = steno_bridge::ConfirmDestructiveParams {
        title: "Remove this phone?".to_owned(),
        message: "It can pair again later.".to_owned(),
        confirm_title: "Remove".to_owned(),
    };
    let reply = harness.host.ui_confirm_destructive(prompt.clone()).unwrap();
    assert!(!reply.confirmed);
    assert_eq!(*asked.lock().unwrap(), vec![prompt]);
}

/// The delete-recording prompt runs with the lock released, so the
/// selection can move while it is up. The answer lands on the meeting it
/// was asked for and the newly selected one is untouched, as in Swift,
/// whose `MainWindowBridge.setKeepAudio(_:on:confirming:)` held the asked
/// meeting's detail model across the prompt. A refusal for a meeting no
/// longer selected comes back as the reply's error.
#[test]
fn keep_audio_lands_on_the_meeting_it_was_asked_for_when_the_selection_moved() {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let harness = Harness::builder()
        .confirm_with({
            let prompts = prompts.clone();
            move |host, params| {
                prompts.lock().unwrap().push(params.title.clone());
                host.meetings_select(MeetingIdParams {
                    meeting_id: uuid(MEETING_IN_PERSON),
                })
                .unwrap();
                true
            }
        })
        .seed(|store, fakes| {
            populate_sample(store, fakes);
            set_retention(store, AudioRetention::DeleteAfterProcessing);
        })
        .build();
    assert_eq!(
        harness.snapshot(BridgeTopic::MeetingDetail)["id"],
        id(MEETING)
    );

    // A prompt asked under the lock deadlocks the selection: fail, do not hang.
    let host = harness.host.clone();
    let reply = within_five_seconds("the delete-recording prompt", move || {
        host.meeting_delete_recording_now()
    })
    .unwrap();
    assert!(reply.confirmed);
    assert_eq!(
        harness.snapshot(BridgeTopic::MeetingDetail)["id"],
        id(MEETING_IN_PERSON),
        "the selection moved while the prompt was up"
    );
    harness
        .host
        .meetings_select(MeetingIdParams {
            meeting_id: uuid(MEETING),
        })
        .unwrap();
    let reply = harness
        .host
        .meeting_set_keep_audio(SetBoolParams { value: false })
        .unwrap();
    assert!(reply.confirmed);
    assert_eq!(
        *prompts.lock().unwrap(),
        vec!["Delete this recording now?"; 2],
        "both commands asked"
    );
    assert_eq!(
        *harness.fakes.pipeline.retention.lock().unwrap(),
        vec![(uuid(MEETING), AudioRetention::DeleteAfterProcessing); 2],
        "both answers landed on the meeting they were asked for"
    );

    // The pipeline's refusal for the meeting no longer selected is the
    // reply's error; the selected meeting's error line stays empty.
    harness
        .fakes
        .pipeline
        .fail_calls(Some("meeting has no audio asset"));
    harness
        .host
        .meetings_select(MeetingIdParams {
            meeting_id: uuid(MEETING),
        })
        .unwrap();
    let error = harness.host.meeting_delete_recording_now().unwrap_err();
    assert_eq!(error.code, BridgeErrorCode::Failed);
    assert_eq!(
        error.message,
        "Retention could not be changed: meeting has no audio asset"
    );
    let detail = harness.snapshot(BridgeTopic::MeetingDetail);
    assert_eq!(detail["id"], id(MEETING_IN_PERSON));
    assert_eq!(detail["error"], Value::Null);
    assert!(
        harness
            .fakes
            .pipeline
            .retention
            .lock()
            .unwrap()
            .iter()
            .all(|(meeting, _)| *meeting == uuid(MEETING)),
        "the newly selected meeting's rule never changed"
    );
}

/// A meeting deleted while the prompt is up is `not_found` and nothing
/// changes. Swift's `applyRetention` failed on the missing asset row; the
/// host names the missing meeting instead.
#[test]
fn keep_audio_on_a_meeting_deleted_during_the_prompt_is_not_found() {
    let store_slot = Arc::new(Mutex::new(None::<Arc<steno_core::Store>>));
    let harness = Harness::builder()
        .confirm_with({
            let store_slot = store_slot.clone();
            move |_, _| {
                let store = store_slot.lock().unwrap().clone().unwrap();
                store.delete_meeting(uuid(MEETING)).unwrap();
                true
            }
        })
        .seed(populate_sample)
        .build();
    *store_slot.lock().unwrap() = Some(harness.store.clone());
    assert_eq!(
        harness.snapshot(BridgeTopic::MeetingDetail)["id"],
        id(MEETING)
    );

    let error = harness.host.meeting_delete_recording_now().unwrap_err();
    assert_eq!(error.code, BridgeErrorCode::NotFound);
    assert_eq!(error.message, "No meeting with that id is listed.");
    assert!(
        harness.fakes.pipeline.retention.lock().unwrap().is_empty(),
        "nothing was changed"
    );
}

/// The pipeline is the only judge of a keep change, as in Swift: its
/// refusal goes on the selected meeting's error line (the reply still says
/// the user went ahead), and a meeting still processing takes the rule,
/// which `applyRetention` documents as safe; only a meeting delete refuses
/// a busy meeting. Swift: `MeetingDetailViewModel.setKeepAudio(_:)`.
#[test]
fn a_refused_keep_change_shows_on_the_detail_and_a_busy_meeting_takes_the_rule() {
    let harness = Harness::builder()
        .seed(|store, fakes| {
            populate_sample(store, fakes);
            set_retention(store, AudioRetention::KeepDays(30));
            let mut processing = sample_meeting();
            processing.id = uuid(0x77);
            processing.state = MeetingState::Processing;
            processing.title = "Still processing".to_owned();
            store.save_meeting(&processing).unwrap();
        })
        .build();
    let _ = harness.snapshot(BridgeTopic::MeetingDetail);
    harness
        .fakes
        .pipeline
        .fail_calls(Some("meeting has no audio asset"));
    harness.sink.clear();
    let reply = harness
        .host
        .meeting_set_keep_audio(SetBoolParams { value: true })
        .unwrap();
    assert!(reply.confirmed);
    assert_eq!(
        harness.sink.last(BridgeTopic::MeetingDetail).unwrap()["error"],
        "Retention could not be changed: meeting has no audio asset",
        "the refusal reaches the page through a detail publish"
    );
    let detail = harness.snapshot(BridgeTopic::MeetingDetail);
    assert_eq!(detail["id"], id(MEETING));
    assert_eq!(
        detail["error"],
        "Retention could not be changed: meeting has no audio asset"
    );

    harness.fakes.pipeline.fail_calls(None);
    harness
        .host
        .meetings_select(MeetingIdParams {
            meeting_id: uuid(0x77),
        })
        .unwrap();
    assert_eq!(harness.snapshot(BridgeTopic::MeetingDetail)["id"], id(0x77));
    let reply = harness.host.meeting_delete_recording_now().unwrap();
    assert!(reply.confirmed);
    assert_eq!(
        harness.fakes.pipeline.retention.lock().unwrap().last(),
        Some(&(uuid(0x77), AudioRetention::KeepDays(30))),
        "a processing meeting takes the rule; the pipeline stamps it later"
    );
}

/// Swift: `testExportStatusFollowsTheDeliveriesAndTheVault`, `testSummaryStatusFollowsTheSummaryAndTheEndpoint`.
#[test]
fn the_detail_footer_and_summary_rows_follow_the_store() {
    let harness = sample();
    let detail = harness.snapshot(BridgeTopic::MeetingDetail);
    assert_eq!(detail["export"]["status"], "notConfigured");
    assert_eq!(detail["canRerunSummary"], false, "no endpoint");
    assert_eq!(detail["summaryStatus"]["kind"], "present");

    configure_llm(&harness.store, "qwen3-8b");
    harness.host.store_changed();
    assert_eq!(
        harness.snapshot(BridgeTopic::MeetingDetail)["canRerunSummary"],
        true
    );

    harness
        .store
        .save_delivery(&Delivery {
            id: Delivery::id_for(uuid(MEETING), "obsidian-folder"),
            meeting_id: uuid(MEETING),
            destination_id: "obsidian-folder".to_owned(),
            status: DeliveryStatus::Failed("vault missing".to_owned()),
            last_attempt_at: Some(now()),
            receipt: None,
        })
        .unwrap();
    harness.host.store_changed();
    let detail = harness.snapshot(BridgeTopic::MeetingDetail);
    assert_eq!(detail["export"]["status"], "failed");
    assert_eq!(
        detail["export"]["message"],
        "Obsidian · Failed: vault missing"
    );
    assert_eq!(detail["export"]["canReveal"], false);
    // Once the launch stopped retrying it, the line says it keeps failing.
    harness
        .fakes
        .pipeline
        .keeps_failing
        .lock()
        .unwrap()
        .push(uuid(MEETING));
    harness.host.store_changed();
    let detail = harness.snapshot(BridgeTopic::MeetingDetail);
    assert_eq!(detail["export"]["status"], "failed");
    assert_eq!(
        detail["export"]["message"],
        "Export to Obsidian keeps failing: vault missing"
    );
    harness.fakes.pipeline.keeps_failing.lock().unwrap().clear();
    assert_eq!(
        harness.host.meeting_reveal_export().unwrap_err().code,
        BridgeErrorCode::NotFound
    );

    // A meeting ready without a summary: skipped, runnable once an endpoint exists.
    let mut bare = sample_meeting();
    bare.id = uuid(0x55);
    bare.summary = None;
    harness.store.save_meeting(&bare).unwrap();
    harness.host.store_changed();
    harness
        .host
        .meetings_select(MeetingIdParams {
            meeting_id: uuid(0x55),
        })
        .unwrap();
    let detail = harness.snapshot(BridgeTopic::MeetingDetail);
    assert_eq!(detail["summaryStatus"]["kind"], "skippedRunnable");
    assert_eq!(detail["summaryStatus"]["actionTitle"], "Run summary");
    assert_eq!(
        detail["canRerunSummary"], false,
        "no transcript to summarise"
    );
}

/// A meeting processed while the API key was withheld (the Swift import's
/// gate) has no summary though an endpoint is set: while the gate still
/// withholds the key the row says so in plain words and opens Settings,
/// with no re-run on offer; once a key is saved it is the runnable row.
#[test]
fn a_summary_skipped_for_a_withheld_key_says_so_until_the_key_is_saved() {
    let harness = Harness::builder()
        .with_withheld_api_key(true)
        .seed(|store, fakes| {
            populate_sample(store, fakes);
            configure_llm(store, "qwen3-8b");
            drop_sample_summary(store);
        })
        .build();
    let _ = harness.snapshot(BridgeTopic::MeetingsList);
    let detail = harness.snapshot(BridgeTopic::MeetingDetail);
    let status = &detail["summaryStatus"];
    assert_eq!(status["kind"], "skippedUnconfigured", "{status}");
    assert_eq!(status["title"], "Summary skipped");
    assert_eq!(
        status["body"],
        steno_host::setup::copy::SUMMARY_KEY_WITHHELD_BODY
    );
    assert!(
        status["body"]
            .as_str()
            .unwrap()
            .starts_with("Steno can't use your API key yet")
    );
    assert_eq!(status["actionTitle"], "Set up summaries");
    assert_eq!(detail["canRerunSummary"], false, "no key, no re-run");

    *harness
        .fakes
        .withheld_api_key
        .as_ref()
        .unwrap()
        .withheld
        .lock()
        .unwrap() = false;
    harness.host.store_changed();
    let detail = harness.snapshot(BridgeTopic::MeetingDetail);
    assert_eq!(detail["summaryStatus"]["kind"], "skippedRunnable");
    assert_eq!(detail["summaryStatus"]["actionTitle"], "Run summary");
    assert_eq!(detail["canRerunSummary"], true);
}

/// A withheld key released without a settings change, by a key-only
/// Summaries save or by the import's step, publishes the open detail
/// again: its Run summary appears with the command, not with the next
/// store poll.
#[test]
fn the_detail_goes_out_again_once_a_key_write_or_the_import_releases_the_key() {
    for release in ["a key-only save", "the import's step"] {
        let harness = Harness::builder()
            .with_withheld_api_key(true)
            .with_swift_import(1)
            .seed(|store, fakes| {
                populate_sample(store, fakes);
                configure_llm(store, "qwen3-8b");
                drop_sample_summary(store);
            })
            .build();
        let _ = harness.snapshot(BridgeTopic::MeetingsList);
        assert_eq!(
            harness.snapshot(BridgeTopic::MeetingDetail)["canRerunSummary"],
            false
        );
        harness.sink.clear();
        // The real gate opens on the key's write and at the step's end.
        *harness
            .fakes
            .withheld_api_key
            .as_ref()
            .unwrap()
            .withheld
            .lock()
            .unwrap() = false;
        if release == "a key-only save" {
            let settings = harness.host.for_window(BridgeWindow::Settings);
            settings
                .settings_summaries_update(steno_bridge::SummariesUpdateParams {
                    base_url: None,
                    model: None,
                    context_tokens: None,
                    api_key: Some("sk-typed".to_owned()),
                })
                .unwrap();
            settings.settings_summaries_save().unwrap();
        } else {
            harness.host.onboarding_import().unwrap();
        }
        let detail = harness
            .sink
            .last(BridgeTopic::MeetingDetail)
            .unwrap_or_else(|| panic!("{release}: the detail did not go out"));
        assert_eq!(detail["canRerunSummary"], true, "{release}");
        assert_eq!(
            detail["summaryStatus"]["kind"], "skippedRunnable",
            "{release}"
        );
    }
}

/// Swift: `openURLAcceptsOnlyWebAndMailLinks`, `theAppPublishCarryingADeepLinkConsumesIt`.
#[test]
fn open_url_takes_https_and_mail_links_and_only_onboarding_closes_itself() {
    let harness = sample();
    harness
        .host
        .system_open_url(OpenUrlParams {
            url: "https://github.com/NicolaiSchmid/steno".to_owned(),
        })
        .unwrap();
    harness
        .host
        .system_open_url(OpenUrlParams {
            url: "mailto:hi@example.com".to_owned(),
        })
        .unwrap();
    for refused in [
        "file:///etc/hosts",
        "http://example.com",
        "javascript:alert(1)",
    ] {
        let error = harness
            .host
            .system_open_url(OpenUrlParams {
                url: refused.to_owned(),
            })
            .unwrap_err();
        assert_eq!(error.code, BridgeErrorCode::InvalidParams, "{refused}");
        assert_eq!(
            error.message,
            "Only https: and mailto: links open from the page."
        );
    }
    assert_eq!(harness.fakes.opener.opened.lock().unwrap().len(), 2);

    harness.sink.clear();
    harness
        .host
        .window_open(WindowParams {
            window: BridgeWindow::Main,
            section: None,
            meeting_id: Some(uuid(MEETING_FAILED)),
        })
        .unwrap();
    assert_eq!(
        *harness.fakes.opener.windows_opened.lock().unwrap(),
        vec![BridgeWindow::Main]
    );
    let apps = harness.sink.all(BridgeTopic::App);
    assert_eq!(
        apps[0]["requestedMeetingID"],
        id(MEETING_FAILED),
        "the request rides on the publish"
    );
    assert!(
        apps.last().unwrap().get("requestedMeetingID").is_none(),
        "and is consumed by it"
    );
    assert_eq!(
        harness.sink.last(BridgeTopic::MeetingsList).unwrap()["selection"],
        id(MEETING_FAILED)
    );

    harness.sink.clear();
    harness
        .host
        .window_open(WindowParams {
            window: BridgeWindow::Settings,
            section: Some(steno_bridge::SettingsSection::Summaries),
            meeting_id: None,
        })
        .unwrap();
    let apps = harness.sink.all(BridgeTopic::App);
    assert_eq!(apps[0]["requestedSettingsSection"], "summaries");
    assert!(
        apps.last()
            .unwrap()
            .get("requestedSettingsSection")
            .is_none()
    );

    let error = harness
        .host
        .window_close(WindowParams {
            window: BridgeWindow::Main,
            section: None,
            meeting_id: None,
        })
        .unwrap_err();
    assert_eq!(error.code, BridgeErrorCode::InvalidParams);
}

/// Swift: `theRecordingSnapshotFollowsTheRecorder`.
#[test]
fn recording_follows_the_recorder_and_the_start_selects_the_live_row() {
    let harness = Harness::builder().build();
    harness.sink.clear();
    harness
        .host
        .recording_start(StartRecordingParams {
            mode: steno_bridge::CaptureMode::InPerson,
        })
        .unwrap();
    // `page.ready` published `recording` a moment ago: the change waits out
    // the 20 Hz interval, which the host's own flush thread ends; nothing
    // else is called.
    assert_eq!(harness.sink.count(BridgeTopic::Recording), 0);
    assert!(harness.host.next_flush_due().is_some());
    harness.wait_for("the throttled recording publish", |harness| {
        harness.sink.count(BridgeTopic::Recording) == 1
    });
    let recording = harness.sink.last(BridgeTopic::Recording).unwrap();
    assert_eq!(recording["state"], "recording");
    assert_eq!(recording["mode"], "inPerson");
    assert_eq!(recording["startedAt"], NOW);
    let live = harness.fakes.recorder.status().meeting_id.unwrap();
    assert_eq!(
        harness.sink.all(BridgeTopic::App)[0]["requestedMeetingID"],
        steno_core::json::uuid_string(live)
    );

    // A stop within 50 ms of the last publish must not read "recording"
    // until the next command: the flush thread publishes it.
    harness.host.recording_stop().unwrap();
    harness.wait_for("the stop to publish", |harness| {
        harness.sink.last(BridgeTopic::Recording).unwrap()["state"] == "idle"
    });
    assert_eq!(*harness.fakes.recorder.stops.lock().unwrap(), 1);
    harness.host.recording_toggle().unwrap();
    assert_eq!(
        harness.fakes.recorder.status().state,
        RecordingState::Recording
    );
    harness.wait_for("the toggle to publish", |harness| {
        harness.sink.last(BridgeTopic::Recording).unwrap()["state"] == "recording"
    });

    // Two changes inside one interval fold into one publish.
    std::thread::sleep(std::time::Duration::from_millis(60));
    harness.sink.clear();
    harness.host.recorder_changed();
    harness.host.recorder_changed();
    assert_eq!(harness.sink.count(BridgeTopic::Recording), 1);
    harness.wait_for("the folded publish", |harness| {
        harness.sink.count(BridgeTopic::Recording) == 2
    });
    std::thread::sleep(std::time::Duration::from_millis(60));
    assert_eq!(harness.sink.count(BridgeTopic::Recording), 2);
    assert_eq!(harness.host.next_flush_due(), None);
}

/// The web app tells a live `recording` row from one a failed save left by
/// the recorder's state, so the list follows the recorder: when its state
/// or meeting moves (here a stop that queued its meeting), the list is
/// reloaded and published at once, not at the next store change; a level
/// update alone reloads nothing. Rust only.
#[test]
fn the_list_follows_the_recorders_state_and_meeting() {
    let mut row = meeting(
        1,
        "Call",
        TitleOrigin::Default,
        NOW,
        60.0,
        MeetingSource::MacCall,
        &[],
        MeetingState::Recording,
        None,
    );
    let seeded = row.clone();
    let harness = Harness::builder()
        .seed(move |store, _| store.save_meeting(&seeded).unwrap())
        .build();
    harness
        .fakes
        .recorder
        .set_status(steno_host::services::RecorderStatus {
            state: RecordingState::Recording,
            meeting_id: Some(uuid(1)),
            ..steno_host::services::RecorderStatus::idle()
        });
    harness.host.recorder_changed();
    harness.sink.clear();
    harness.host.recorder_changed();
    assert_eq!(
        harness.sink.count(BridgeTopic::MeetingsList),
        0,
        "levels alone"
    );

    // The stop queued the meeting, then the recorder went idle.
    row.state = MeetingState::Queued;
    harness.store.save_meeting(&row).unwrap();
    harness
        .fakes
        .recorder
        .set_status(steno_host::services::RecorderStatus::idle());
    harness.host.recorder_changed();
    let list = harness.sink.last(BridgeTopic::MeetingsList).unwrap();
    assert_eq!(list["groups"][0]["meetings"][0]["state"], "queued");
}

/// A meeting left `recording` (a save that failed, a folder gone for
/// good) can be deleted while the recorder is idle and its master is not
/// being written, with its folder in each audio folder the recorder names
/// (no asset row names them), and the recorder forgets it. While the
/// recorder records, every recording row is refused, the live one among
/// them; so is one whose master another process still writes. Rust only.
#[test]
fn a_recording_row_can_be_deleted_while_the_recorder_is_idle() {
    let mut left = sample_meeting();
    left.id = uuid(0x78);
    left.state = MeetingState::Recording;
    let seeded = left.clone();
    let harness = Harness::builder()
        .confirm(true)
        .seed(move |store, _| store.save_meeting(&seeded).unwrap())
        .build();
    let _ = harness.snapshot(BridgeTopic::MeetingsList);
    let delete = || {
        harness.host.meetings_delete(MeetingIdParams {
            meeting_id: left.id,
        })
    };
    harness
        .fakes
        .recorder
        .set_status(steno_host::services::RecorderStatus {
            state: RecordingState::Recording,
            meeting_id: Some(uuid(0x79)),
            ..steno_host::services::RecorderStatus::idle()
        });
    assert_eq!(
        delete().unwrap_err().message,
        "This meeting is still recording."
    );
    assert!(harness.store.meeting(left.id).unwrap().is_some());

    harness
        .fakes
        .recorder
        .set_status(steno_host::services::RecorderStatus::idle());
    let recorded = std::path::PathBuf::from("/Volumes/Old/Steno");
    let folders = vec![recorded.clone(), harness.audio_folder()];
    harness.fakes.recorder.left.lock().unwrap().insert(
        left.id,
        steno_host::services::LeftRecording {
            folders: folders.clone(),
            still_written: true,
        },
    );
    assert_eq!(
        delete().unwrap_err().message,
        "This meeting is still recording."
    );
    assert!(harness.store.meeting(left.id).unwrap().is_some());

    harness.fakes.recorder.left.lock().unwrap().insert(
        left.id,
        steno_host::services::LeftRecording {
            folders,
            still_written: false,
        },
    );
    assert!(delete().unwrap().confirmed);
    assert!(harness.store.meeting(left.id).unwrap().is_none());
    let id = steno_core::json::uuid_string(left.id);
    assert_eq!(
        *harness.fakes.file_system.removed.lock().unwrap(),
        [recorded.join(&id), harness.audio_folder().join(&id)]
    );
    assert_eq!(*harness.fakes.recorder.forgotten.lock().unwrap(), [left.id]);
}

/// Swift: `DisplayTitleTests`.
#[test]
fn derived_titles_read_weekday_or_month_day_and_time() {
    let mut meeting = sample_meeting();
    meeting.title = "Call 2026-09-29 14:50".to_owned();
    meeting.title_origin = TitleOrigin::Default;
    let today = now();
    assert_eq!(display_title(&meeting, today, utc()), "Tuesday 12:50");
    let six_days = date("2026-10-05T12:50:00.000Z");
    assert_eq!(display_title(&meeting, six_days, utc()), "Tuesday 12:50");
    let eight_days = date("2026-10-07T12:50:00.000Z");
    assert_eq!(display_title(&meeting, eight_days, utc()), "Sep 29 12:50");
    let future = date("2026-09-20T12:50:00.000Z");
    assert_eq!(display_title(&meeting, future, utc()), "Sep 29 12:50");
    let berlin = chrono::FixedOffset::east_opt(2 * 3600).unwrap();
    assert_eq!(display_title(&meeting, today, berlin), "Tuesday 14:50");
    meeting.title_origin = TitleOrigin::Calendar;
    assert_eq!(
        display_title(&meeting, today, utc()),
        "Call 2026-09-29 14:50",
        "a calendar title renders unchanged"
    );
    assert_eq!(meeting.source, MeetingSource::MacCall);
}

/// Swift: `MainWindowSnapshotHelperTests`.
#[test]
fn turns_merge_consecutive_segments_and_delivery_lines_name_the_destination() {
    let meeting_id = uuid(MEETING);
    let segment =
        |n: u32, start: f64, speaker: Option<u32>, text: &str| steno_core::TranscriptSegment {
            id: uuid(n),
            meeting_id,
            start,
            end: start + 1.0,
            speaker_id: speaker.map(uuid),
            lane: steno_core::AudioLane::Mic,
            text: text.to_owned(),
            raw_text: text.to_owned(),
        };
    let turns = turns(
        &[
            segment(3, 2.0, Some(SPEAKER_NICOLAI), "c"),
            segment(1, 0.0, Some(SPEAKER_NICOLAI), "a"),
            segment(2, 1.0, Some(SPEAKER_NICOLAI), "b"),
            segment(4, 3.0, None, "d"),
            segment(5, 4.0, None, "e"),
        ],
        |id| {
            if id.is_some() {
                "Nicolai".to_owned()
            } else {
                "Unknown".to_owned()
            }
        },
    );
    assert_eq!(turns.len(), 3);
    assert_eq!(turns[0].text, "a b c");
    assert_eq!(turns[0].end_seconds, 3.0);
    assert_eq!(turns[1].speaker_name, "Unknown");

    let delivered = Delivery {
        id: uuid(9),
        meeting_id,
        destination_id: "obsidian-folder".to_owned(),
        status: DeliveryStatus::Delivered,
        last_attempt_at: Some(date("2026-09-29T08:02:00.000Z")),
        receipt: None,
    };
    assert_eq!(
        delivery_line(&delivered, utc()),
        "Obsidian · Exported 08:02"
    );
    let pending = Delivery {
        destination_id: "markdown".to_owned(),
        status: DeliveryStatus::Pending,
        ..delivered
    };
    assert_eq!(delivery_line(&pending, utc()), "markdown · Pending");
}

#[test]
fn an_export_line_shows_the_warnings_of_the_receipt_it_delivered() {
    const NO_AUDIO: &str = "The audio was already removed, so the export has no audio file";
    let delivered = Delivery {
        id: uuid(9),
        meeting_id: uuid(MEETING),
        destination_id: "obsidian-folder".to_owned(),
        status: DeliveryStatus::Delivered,
        last_attempt_at: Some(date("2026-09-29T08:02:00.000Z")),
        receipt: Some(steno_core::DeliveryReceipt {
            root: "/vault".to_owned(),
            folder: "Meetings/2026-09-29 Produktstrategie".to_owned(),
            files: Vec::new(),
            renderer_version: 1,
            warnings: vec![NO_AUDIO.to_owned()],
        }),
    };
    assert_eq!(
        delivery_line(&delivered, utc()),
        format!("Obsidian · Exported 08:02 · {NO_AUDIO}")
    );

    // A later attempt that failed keeps the last receipt; its warning is
    // not this attempt's.
    let failed = Delivery {
        status: DeliveryStatus::Failed("disk full".to_owned()),
        ..delivered
    };
    assert_eq!(
        delivery_line(&failed, utc()),
        "Obsidian · Failed: disk full"
    );
}

/// The failed sample meeting with a recording of its own on disk (in the
/// fake file system); its master's path.
fn give_the_failed_meeting_a_recording(store: &steno_core::Store, fakes: &FakeServices) -> PathBuf {
    let audio_folder =
        steno_core::paths::file_url_path(&store.settings().unwrap().audio_folder).unwrap();
    let meeting = store.meeting(uuid(MEETING_FAILED)).unwrap().unwrap();
    let master = audio_folder
        .join(steno_core::json::uuid_string(meeting.id))
        .join("master.caf");
    let asset = steno_core::AudioAsset {
        id: uuid(0x47),
        meeting_id: meeting.id,
        url: steno_core::paths::file_url(&master, false),
        format: steno_core::AudioFormat::Caf48kFloat32,
        lanes: vec![steno_core::AudioLane::Mic, steno_core::AudioLane::System],
        sidecars_16k: std::collections::BTreeMap::new(),
        mixdown_url: None,
        retention: AudioRetention::KeepDays(30),
        expires_at: None,
    };
    store.save_meeting_with_asset(&meeting, &asset).unwrap();
    fakes.file_system.create(master.clone());
    master
}

/// "Process again" on the selected failed meeting whose recording is on
/// disk is offered (`canProcessAgain`) and reaches the pipeline; each
/// refusal is the error line in the user's words, and one while the app
/// quits shows nothing. A meeting that is not failed is not offered and
/// never reaches the pipeline.
#[test]
fn process_again_runs_a_failed_meeting_and_words_each_refusal() {
    let harness = Harness::builder()
        .seed(|store, fakes| {
            populate_sample(store, fakes);
            give_the_failed_meeting_a_recording(store, fakes);
        })
        .build();
    let pipeline = &harness.fakes.pipeline;
    let error_line = || harness.sink.last(BridgeTopic::MeetingDetail).unwrap()["error"].clone();

    // The selection is the ready meeting: only a failed one is processed again.
    assert_eq!(
        harness.snapshot(BridgeTopic::MeetingDetail)["canProcessAgain"],
        false
    );
    harness.host.meeting_process_again().unwrap();
    assert_eq!(
        error_line(),
        "Only a failed meeting can be processed again."
    );
    assert!(pipeline.processed_again.lock().unwrap().is_empty());

    harness
        .host
        .meetings_select(MeetingIdParams {
            meeting_id: uuid(MEETING_FAILED),
        })
        .unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::MeetingDetail)["canProcessAgain"],
        true
    );
    harness.host.meeting_process_again().unwrap();
    assert_eq!(error_line(), Value::Null, "accepted: the line clears");
    assert_eq!(
        *pipeline.processed_again.lock().unwrap(),
        [uuid(MEETING_FAILED)]
    );
    assert_eq!(
        harness.sink.last(BridgeTopic::MeetingDetail).unwrap()["isBusy"],
        false
    );

    for (refusal, line) in [
        (
            Refusal::RecordingGone,
            "The recording is no longer on this Mac, so the meeting cannot be processed again.",
        ),
        (Refusal::Busy, "This meeting is already being processed."),
        (
            Refusal::NotOffered,
            "Only a failed meeting can be processed again.",
        ),
        (Refusal::MeetingGone, "This meeting no longer exists."),
        (
            Refusal::CouldNotStart("decode: the disk is full".to_owned()),
            "Processing could not start: decode: the disk is full",
        ),
    ] {
        *pipeline.process_again_refusal.lock().unwrap() = Some(refusal.clone());
        harness.host.meeting_process_again().unwrap();
        assert_eq!(error_line(), line, "{refusal:?}");
    }

    // Quitting: nothing is shown, not even the last refusal's line.
    *pipeline.process_again_refusal.lock().unwrap() = None;
    harness.host.meeting_process_again().unwrap();
    *pipeline.process_again_refusal.lock().unwrap() = Some(Refusal::Quitting);
    harness.host.meeting_process_again().unwrap();
    assert_eq!(error_line(), Value::Null);
}

/// A stale page clicks "Process again" on the selected failed meeting after
/// its recording has gone (the button is hidden, the page has not caught
/// up). The detail refuses before the pipeline is asked, and the error line
/// says the recording is gone: the meeting is failed, so "Only a failed
/// meeting" would be wrong.
#[test]
fn process_again_words_a_gone_recording_through_the_detail() {
    let master = Arc::new(Mutex::new(PathBuf::new()));
    let harness = {
        let master = master.clone();
        Harness::builder()
            .seed(move |store, fakes| {
                populate_sample(store, fakes);
                *master.lock().unwrap() = give_the_failed_meeting_a_recording(store, fakes);
            })
            .build()
    };
    harness
        .host
        .meetings_select(MeetingIdParams {
            meeting_id: uuid(MEETING_FAILED),
        })
        .unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::MeetingDetail)["canProcessAgain"],
        true
    );
    harness
        .fakes
        .file_system
        .existing
        .lock()
        .unwrap()
        .remove(&*master.lock().unwrap());
    harness.host.store_changed();
    assert_eq!(
        harness.snapshot(BridgeTopic::MeetingDetail)["canProcessAgain"],
        false
    );
    harness.host.meeting_process_again().unwrap();
    assert!(
        harness
            .fakes
            .pipeline
            .processed_again
            .lock()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        harness.sink.last(BridgeTopic::MeetingDetail).unwrap()["error"],
        "The recording is no longer on this Mac, so the meeting cannot be processed again."
    );
}

/// While the API key is withheld for the stored endpoint (the Swift
/// import's gate), a failed meeting with its recording on disk is not
/// offered "Process again": the run would complete with no cleanup and no
/// summary, and the button would then be gone for good. A stale click is
/// refused before the pipeline is asked, in plain words. Once a key is
/// saved it is offered and reaches the pipeline.
#[test]
fn process_again_waits_for_a_withheld_key() {
    let harness = Harness::builder()
        .with_withheld_api_key(true)
        .seed(|store, fakes| {
            populate_sample(store, fakes);
            configure_llm(store, "qwen3-8b");
            give_the_failed_meeting_a_recording(store, fakes);
        })
        .build();
    let pipeline = &harness.fakes.pipeline;
    harness
        .host
        .meetings_select(MeetingIdParams {
            meeting_id: uuid(MEETING_FAILED),
        })
        .unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::MeetingDetail)["canProcessAgain"],
        false
    );
    harness.host.meeting_process_again().unwrap();
    assert_eq!(
        harness.sink.last(BridgeTopic::MeetingDetail).unwrap()["error"],
        "Steno can't use your API key yet. Enter your API key in Settings, then process the meeting again."
    );
    assert!(pipeline.processed_again.lock().unwrap().is_empty());

    *harness
        .fakes
        .withheld_api_key
        .as_ref()
        .unwrap()
        .withheld
        .lock()
        .unwrap() = false;
    harness.host.store_changed();
    assert_eq!(
        harness.snapshot(BridgeTopic::MeetingDetail)["canProcessAgain"],
        true
    );
    harness.host.meeting_process_again().unwrap();
    assert_eq!(
        *pipeline.processed_again.lock().unwrap(),
        [uuid(MEETING_FAILED)]
    );
}

/// Off the Mac the recording is "on this computer".
#[test]
fn process_again_words_a_gone_recording_for_the_platform() {
    let harness = Harness::builder()
        .platform(steno_core::Platform::Linux)
        .seed(|store, fakes| {
            populate_sample(store, fakes);
            give_the_failed_meeting_a_recording(store, fakes);
        })
        .build();
    harness
        .host
        .meetings_select(MeetingIdParams {
            meeting_id: uuid(MEETING_FAILED),
        })
        .unwrap();
    *harness.fakes.pipeline.process_again_refusal.lock().unwrap() = Some(Refusal::RecordingGone);
    harness.host.meeting_process_again().unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::MeetingDetail)["error"],
        "The recording is no longer on this computer, so the meeting cannot be processed again."
    );
}
