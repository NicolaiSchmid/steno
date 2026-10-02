//! The main window's behaviour through the host, ported from
//! `MainWindowBridgeTests`, `MeetingListViewModelTests`,
//! `MeetingDetailViewModelTests` and `DisplayTitleTests`.

// The ported suites keep one Swift test per function, long as some are.
#![allow(clippy::too_many_lines)]

mod common;

use steno_host::services::Recorder as _;

use common::*;
use serde_json::{Value, json};
use steno_bridge::{
    BridgeErrorCode, BridgeHost, BridgeTopic, BridgeWindow, DetailTab, ListFilter, MeetingIdParams,
    OpenUrlParams, RecordingState, SaveNotesParams, SetBoolParams, SetFilterParams, SetQueryParams,
    SetTabParams, SetTagFilterParams, SetTagsParams, SetTemplateParams, StartRecordingParams,
    WindowParams,
};
use steno_core::{
    AudioRetention, Delivery, DeliveryStatus, MeetingSource, MeetingState, TitleOrigin,
};
use steno_host::host::TOPICS;
use steno_host::labels::{display_title, utc};
use steno_host::main_window::snapshots::{delivery_line, turns};

fn sample() -> Harness {
    Harness::builder()
        .seed(|store, fakes| {
            let folder =
                steno_core::paths::path_from_file_url(&store.settings().unwrap().audio_folder)
                    .unwrap();
            populate_sample(store, fakes, &folder);
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
            let folder =
                steno_core::paths::path_from_file_url(&store.settings().unwrap().audio_folder)
                    .unwrap();
            populate_sample(store, fakes, &folder);
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
        .seed(|store, fakes| {
            let folder =
                steno_core::paths::path_from_file_url(&store.settings().unwrap().audio_folder)
                    .unwrap();
            populate_sample(store, fakes, &folder);
        })
        .build();
    let _ = confirming.snapshot(BridgeTopic::MeetingsList);
    confirming.sink.clear();
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

    *harness.fakes.pipeline.failure.lock().unwrap() =
        Some("the endpoint did not answer".to_owned());
    harness.host.meeting_rerun_summary().unwrap();
    let detail = harness.sink.last(BridgeTopic::MeetingDetail).unwrap();
    assert_eq!(
        detail["error"],
        "Summary re-run failed: the endpoint did not answer"
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
            let folder =
                steno_core::paths::path_from_file_url(&store.settings().unwrap().audio_folder)
                    .unwrap();
            populate_sample(store, fakes, &folder);
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

/// Swift: `openURLAcceptsOnlyWebAndMailLinks`, `theAppPublishCarryingADeepLinkConsumesIt`.
#[test]
fn system_and_window_commands() {
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
    let refused = harness
        .host
        .system_open_url(OpenUrlParams {
            url: "file:///etc/hosts".to_owned(),
        })
        .unwrap_err();
    assert_eq!(refused.code, BridgeErrorCode::InvalidParams);
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
    // the 20 Hz interval, which the shell's timer flushes.
    assert_eq!(harness.sink.count(BridgeTopic::Recording), 0);
    assert!(harness.host.next_flush_due().is_some());
    harness.settle();
    let recording = harness.sink.last(BridgeTopic::Recording).unwrap();
    assert_eq!(recording["state"], "recording");
    assert_eq!(recording["mode"], "inPerson");
    assert_eq!(recording["startedAt"], NOW);
    let live = harness.fakes.recorder.status().meeting_id.unwrap();
    assert_eq!(
        harness.sink.all(BridgeTopic::App)[0]["requestedMeetingID"],
        steno_core::json::uuid_string(live)
    );

    harness.host.recording_stop().unwrap();
    harness.settle();
    assert_eq!(
        harness.sink.last(BridgeTopic::Recording).unwrap()["state"],
        "idle"
    );
    assert_eq!(*harness.fakes.recorder.stops.lock().unwrap(), 1);
    harness.host.recording_toggle().unwrap();
    assert_eq!(
        harness.fakes.recorder.status().state,
        RecordingState::Recording
    );

    // Two changes inside one interval fold into one publish.
    std::thread::sleep(std::time::Duration::from_millis(60));
    harness.sink.clear();
    harness.host.recorder_changed();
    harness.host.recorder_changed();
    assert_eq!(harness.sink.count(BridgeTopic::Recording), 1);
    harness.settle();
    assert_eq!(harness.sink.count(BridgeTopic::Recording), 2);
    assert_eq!(harness.host.next_flush_due(), None);
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
