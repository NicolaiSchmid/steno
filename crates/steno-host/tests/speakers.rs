//! The speaker picker through the host, ported from `SpeakersViewModelTests`.
//! `SpeakerPickerStateTests` have no port: the keyboard model of the open
//! picker lives in the web page now.

// The ported suites keep one Swift test per function, long as some are.
#![allow(clippy::too_many_lines)]

mod common;

use steno_host::services::FileSystem as _;

use common::*;
use serde_json::json;
use steno_bridge::{
    BridgeErrorCode, BridgeHost, BridgeTopic, SelectSpeakerParams, SpeakerIdParams, SpeakerOption,
    SpeakerOptionKind, SpeakerOptionsParams,
};
use steno_core::{Participant, ParticipantRole, SpeakerAssignment, SpeakerNameSuggestion};

fn sample() -> Harness {
    Harness::builder()
        .seed(|store, fakes| {
            let folder =
                steno_core::paths::path_from_file_url(&store.settings().unwrap().audio_folder)
                    .unwrap();
            populate_sample(store, fakes, &folder);
        })
        .build()
}

fn options(harness: &Harness, speaker: u32, query: &str) -> Vec<SpeakerOption> {
    harness
        .host
        .speakers_options(SpeakerOptionsParams {
            speaker_id: uuid(speaker),
            query: query.to_owned(),
        })
        .unwrap()
        .options
}

fn person_option(n: u32, label: &str) -> SpeakerOption {
    SpeakerOption {
        kind: SpeakerOptionKind::Person,
        label: label.to_owned(),
        detail: None,
        person_id: Some(uuid(n)),
    }
}

fn create_option(label: &str) -> SpeakerOption {
    SpeakerOption {
        kind: SpeakerOptionKind::Create,
        label: label.to_owned(),
        detail: None,
        person_id: None,
    }
}

fn select(
    harness: &Harness,
    speaker: u32,
    option: SpeakerOption,
) -> Result<(), steno_bridge::BridgeError> {
    harness.host.speakers_select(SelectSpeakerParams {
        speaker_id: uuid(speaker),
        option,
    })
}

/// Swift: `testRowsListEverySpeakerInClusterOrder`.
#[test]
fn rows_list_every_speaker_in_cluster_order() {
    let harness = sample();
    let detail = harness.snapshot(BridgeTopic::MeetingDetail);
    let labels: Vec<&str> = detail["speakers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["clusterLabel"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["Speaker 1", "Speaker 2", "Speaker 3", "Speaker 4"]);
    assert_eq!(detail["speakers"][0]["displayName"], "Nicolai");
    assert_eq!(detail["speakers"][0]["assignment"], "confirmed");
    assert_eq!(
        detail["speakers"][2]["displayName"], "Anna",
        "a suggested speaker reads as the person"
    );
    assert_eq!(detail["speakers"][2]["suggestionName"], "Anna");
    assert_eq!(detail["speakers"][3]["displayName"], "Speaker 4");
    assert_eq!(detail["speakers"][3]["hasClip"], false);
    assert_eq!(detail["speakers"][0]["hasClip"], true);
}

/// Swift: `testOptionsStartWithTheVoiceMatchAndNeverListTheOwnPerson`, `testPrefillIsTheSuggestedName`,
/// `testBuildingOptionsWritesNothing`.
#[test]
fn options_start_with_the_voice_match_and_never_list_the_own_person() {
    let harness = sample();
    let reply = harness
        .host
        .speakers_options(SpeakerOptionsParams {
            speaker_id: uuid(SPEAKER_ANNA),
            query: String::new(),
        })
        .unwrap();
    assert_eq!(reply.prefill.as_deref(), Some("Anna"));
    assert_eq!(reply.options[0].person_id, Some(uuid(PERSON_ANNA)));
    assert_eq!(reply.options[0].detail.as_deref(), Some("Sounds like"));
    assert!(
        reply
            .options
            .iter()
            .any(|option| option.person_id == Some(uuid(PERSON_NICOLAI))
                && option.detail.as_deref() == Some("In this meeting"))
    );
    assert!(
        reply
            .options
            .iter()
            .all(|option| option.kind != SpeakerOptionKind::Create),
        "no create row without a query"
    );

    let own = options(&harness, SPEAKER_NICOLAI, "");
    assert!(
        !own.iter()
            .any(|option| option.person_id == Some(uuid(PERSON_NICOLAI)))
    );
    let confirmed = harness
        .host
        .speakers_options(SpeakerOptionsParams {
            speaker_id: uuid(SPEAKER_NICOLAI),
            query: String::new(),
        })
        .unwrap();
    assert_eq!(confirmed.prefill, None, "a confirmed speaker has none");
    assert_eq!(
        options(&harness, 0xEE, ""),
        Vec::new(),
        "an unknown speaker has no options"
    );

    let typed = options(&harness, SPEAKER_UNKNOWN, "jerome");
    assert_eq!(
        typed[0].person_id,
        Some(uuid(PERSON_JEROME)),
        "the filter ignores accents"
    );
    assert_eq!(
        typed.last().map(|o| o.kind),
        Some(SpeakerOptionKind::Person),
        "an exact name adds no create row"
    );
    let typed = options(&harness, SPEAKER_UNKNOWN, "Maya");
    assert_eq!(typed, vec![create_option("Maya")]);
    assert_eq!(
        harness.store.persons().unwrap().len(),
        3,
        "building options writes nothing"
    );
}

/// Swift: `testSelectingAPersonConfirmsAndRowsFollowTheExport`, `testSelectingTheOwnPersonIsANoOp`,
/// `testClosingThePickerRedeliversOnceAfterChanges`.
#[test]
fn selecting_a_person_confirms_and_re_exports_once() {
    let harness = sample();
    harness.sink.clear();
    select(&harness, SPEAKER_ANNA, person_option(PERSON_ANNA, "Anna")).unwrap();
    let speakers = harness.store.speakers(uuid(MEETING)).unwrap();
    assert_eq!(
        speakers[2].assignment,
        SpeakerAssignment::Confirmed {
            person_id: uuid(PERSON_ANNA)
        }
    );
    assert_eq!(
        *harness.fakes.pipeline.redeliveries.lock().unwrap(),
        vec![uuid(MEETING)],
        "one re-export per change"
    );
    let detail = harness.sink.last(BridgeTopic::MeetingDetail).unwrap();
    assert_eq!(detail["speakers"][2]["assignment"], "confirmed");
    assert!(detail["speakers"][2].get("suggestionName").is_none());
    let list = harness.sink.last(BridgeTopic::MeetingsList).unwrap();
    assert_eq!(
        list["groups"][0]["meetings"][0]["speakers"][2]["isConfirmed"], true,
        "the chips follow"
    );

    select(
        &harness,
        SPEAKER_NICOLAI,
        person_option(PERSON_NICOLAI, "Nicolai"),
    )
    .unwrap();
    assert_eq!(
        harness.fakes.pipeline.redeliveries.lock().unwrap().len(),
        1,
        "the own person again changes nothing"
    );

    let unknown = select(
        &harness,
        SPEAKER_UNKNOWN,
        SpeakerOption {
            kind: SpeakerOptionKind::Person,
            label: "Ghost".to_owned(),
            detail: None,
            person_id: Some(uuid(0xEE)),
        },
    )
    .unwrap_err();
    assert_eq!(unknown.code, BridgeErrorCode::NotFound);
    let blank = select(&harness, SPEAKER_UNKNOWN, create_option("   ")).unwrap_err();
    assert_eq!(blank.code, BridgeErrorCode::InvalidParams);
    let leave = select(
        &harness,
        SPEAKER_UNKNOWN,
        SpeakerOption {
            kind: SpeakerOptionKind::Unknown,
            label: "Leave unnamed".to_owned(),
            detail: None,
            person_id: None,
        },
    )
    .unwrap_err();
    assert_eq!(leave.code, BridgeErrorCode::Failed);
    assert_eq!(
        harness.store.persons().unwrap().len(),
        3,
        "no person was created"
    );
}

/// Swift: `testSelectingCreateNamesANewPersonAndReusesAnExistingName`, `testACreateOptionForAnAttendeeTakesTheEmail`.
#[test]
fn selecting_create_names_a_new_person_reuses_an_existing_name_and_takes_an_attendee_email() {
    let harness = sample();
    harness
        .store
        .save_participant(&Participant {
            id: uuid(0x31),
            meeting_id: uuid(MEETING),
            person_id: None,
            display_name: "Maya".to_owned(),
            role: ParticipantRole::Them,
            email: Some("maya@example.com".to_owned()),
        })
        .unwrap();
    harness.host.store_changed();
    let offered = options(&harness, SPEAKER_UNKNOWN, "");
    let maya = offered
        .iter()
        .find(|option| option.label == "Maya")
        .unwrap();
    assert_eq!(maya.kind, SpeakerOptionKind::Create);
    assert_eq!(maya.detail.as_deref(), Some("Attendee"));

    select(&harness, SPEAKER_UNKNOWN, create_option(" Maya ")).unwrap();
    let speakers = harness.store.speakers(uuid(MEETING)).unwrap();
    let person_id = speakers[3].person_id().unwrap();
    let person = harness.store.person(person_id).unwrap().unwrap();
    assert_eq!(person.display_name, "Maya");
    assert_eq!(person.email.as_deref(), Some("maya@example.com"));
    assert_eq!(
        person.sample_count, 1,
        "the voice is the speaker's embedding"
    );
    assert_eq!(harness.store.persons().unwrap().len(), 4);

    select(&harness, SPEAKER_UNKNOWN, create_option("jérôme")).unwrap();
    let speakers = harness.store.speakers(uuid(MEETING)).unwrap();
    assert_eq!(
        speakers.len(),
        3,
        "Jérôme already owns Speaker 2: the two merge"
    );
    assert_eq!(harness.store.persons().unwrap().len(), 4, "no fifth person");
    assert_eq!(
        harness.snapshot(BridgeTopic::MeetingDetail)["speakers"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

/// Swift: `testLLMNameSuggestionIsAMentionedOption`.
#[test]
fn an_llm_name_suggestion_is_a_mentioned_option() {
    let harness = sample();
    let meeting = harness.store.meeting(uuid(MEETING)).unwrap().unwrap();
    let tasks = harness.store.tasks(uuid(MEETING)).unwrap();
    harness
        .store
        .replace_summary(
            &meeting,
            &tasks,
            &["d".to_owned()],
            &[
                SpeakerNameSuggestion {
                    speaker_id: uuid(SPEAKER_UNKNOWN),
                    name: Some("Anna Berger".to_owned()),
                    confidence: 0.8,
                    evidence: "danke, Anna".to_owned(),
                },
                SpeakerNameSuggestion {
                    speaker_id: uuid(SPEAKER_NICOLAI),
                    name: None,
                    confidence: 0.0,
                    evidence: String::new(),
                },
            ],
        )
        .unwrap();
    harness.host.store_changed();
    let offered = options(&harness, SPEAKER_UNKNOWN, "");
    assert_eq!(
        offered[0],
        SpeakerOption {
            kind: SpeakerOptionKind::Create,
            label: "Anna Berger".to_owned(),
            detail: Some("Mentioned".to_owned()),
            person_id: None
        }
    );
    assert_eq!(
        harness.store.speakers(uuid(MEETING)).unwrap()[3].assignment,
        SpeakerAssignment::Unknown,
        "listing the guess applies nothing"
    );
    assert!(
        !options(&harness, SPEAKER_NICOLAI, "")
            .iter()
            .any(|option| option.detail.as_deref() == Some("Mentioned"))
    );
}

/// Swift: `testPlayWithoutAClipFileReportsAnError`.
#[test]
fn playback_follows_the_clip_files() {
    let harness = sample();
    harness
        .host
        .speakers_play(SpeakerIdParams {
            speaker_id: uuid(SPEAKER_UNKNOWN),
        })
        .unwrap();
    let detail = harness.snapshot(BridgeTopic::MeetingDetail);
    assert_eq!(detail["error"], "This speaker has no sample clip.");
    assert!(
        detail["speakers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["isPlaying"] == false)
    );

    harness
        .host
        .speakers_play(SpeakerIdParams {
            speaker_id: uuid(SPEAKER_NICOLAI),
        })
        .unwrap();
    let detail = harness.snapshot(BridgeTopic::MeetingDetail);
    assert_eq!(detail["speakers"][0]["isPlaying"], true);
    assert_eq!(harness.fakes.clip_player.played.lock().unwrap().len(), 1);
    harness.host.speakers_stop().unwrap();
    assert_eq!(
        harness.snapshot(BridgeTopic::MeetingDetail)["speakers"][0]["isPlaying"],
        false
    );

    harness
        .fakes
        .file_system
        .remove(&clip_path(&harness.audio_folder(), SPEAKER_JEROME))
        .unwrap();
    harness.host.store_changed();
    assert_eq!(
        harness.snapshot(BridgeTopic::MeetingDetail)["speakers"][1]["hasClip"],
        false
    );
    harness
        .host
        .speakers_play(SpeakerIdParams {
            speaker_id: uuid(SPEAKER_JEROME),
        })
        .unwrap();
    assert!(
        harness.snapshot(BridgeTopic::MeetingDetail)["error"]
            .as_str()
            .unwrap()
            .starts_with("The sample clip could not be played (")
    );
    let _ = json!(null);
}
