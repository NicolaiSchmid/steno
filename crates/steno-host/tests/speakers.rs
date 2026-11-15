//! The speaker picker through the host. The open picker's keyboard model
//! lives in the web page.

// Some tests are long.
#![allow(clippy::too_many_lines)]

mod common;

use steno_host::services::FileSystem as _;

use common::*;
use serde_json::json;
use steno_bridge::{
    BridgeErrorCode, BridgeHost, BridgeTopic, RejectSpeakerParams, SelectSpeakerParams,
    SpeakerIdParams, SpeakerOption, SpeakerOptionKind, SpeakerOptionReason, SpeakerOptionsParams,
    VoiceStrength,
};
use steno_core::{Participant, ParticipantRole, SpeakerAssignment, SpeakerNameSuggestion};

fn sample() -> Harness {
    Harness::builder().seed(populate_sample).build()
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
        reason: None,
        strength: None,
        person_id: Some(uuid(n)),
    }
}

fn create_option(label: &str) -> SpeakerOption {
    SpeakerOption {
        kind: SpeakerOptionKind::Create,
        label: label.to_owned(),
        reason: None,
        strength: None,
        person_id: None,
    }
}

fn select(
    harness: &Harness,
    speaker: u32,
    option: SpeakerOption,
) -> Result<(), steno_bridge::BridgeError> {
    select_combining(harness, speaker, option, false)
}

fn select_combining(
    harness: &Harness,
    speaker: u32,
    option: SpeakerOption,
    combine: bool,
) -> Result<(), steno_bridge::BridgeError> {
    harness.host.speakers_select(SelectSpeakerParams {
        speaker_id: uuid(speaker),
        option,
        combine,
    })
}

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

/// The prefill is the suggested name, and building the options writes nothing.
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
    assert_eq!(reply.options[0].reason, Some(SpeakerOptionReason::Voice));
    assert_eq!(
        reply.options[0].strength,
        VoiceStrength::of(0.87),
        "the strength is the match's"
    );
    assert!(
        reply
            .options
            .iter()
            .any(|option| option.person_id == Some(uuid(PERSON_NICOLAI))
                && option.reason == Some(SpeakerOptionReason::InThisMeeting))
    );
    assert!(
        reply.options[1..]
            .iter()
            .all(|option| option.strength.is_none()),
        "only a voice match has a strength"
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

/// Confirming a suggestion as a person who owns another voice here would
/// merge the two; without `combine` the host refuses and names that voice.
#[test]
fn a_pick_that_would_combine_needs_combine() {
    let harness = sample();
    let _ = harness.snapshot(BridgeTopic::MeetingDetail);
    let refused = select(
        &harness,
        SPEAKER_ANNA,
        person_option(PERSON_NICOLAI, "Nicolai"),
    )
    .unwrap_err();
    assert_eq!(refused.code, BridgeErrorCode::WouldCombine);
    assert_eq!(
        refused.message,
        "Nicolai already speaks as Speaker 1 in this meeting."
    );
    assert_eq!(harness.store.speakers(uuid(MEETING)).unwrap().len(), 4);
    assert!(
        harness
            .fakes
            .pipeline
            .redeliveries
            .lock()
            .unwrap()
            .is_empty()
    );

    select_combining(
        &harness,
        SPEAKER_ANNA,
        person_option(PERSON_NICOLAI, "Nicolai"),
        true,
    )
    .unwrap();
    let speakers = harness.store.speakers(uuid(MEETING)).unwrap();
    assert!(
        speakers
            .iter()
            .all(|speaker| speaker.id != uuid(SPEAKER_ANNA))
    );
    assert_eq!(
        *harness.fakes.pipeline.redeliveries.lock().unwrap(),
        vec![uuid(MEETING)]
    );
}

/// A pick the store refuses replies as an error instead of a success with
/// the failure left in the detail.
#[test]
fn a_failed_pick_replies_as_an_error() {
    let harness = sample();
    let _ = harness.snapshot(BridgeTopic::MeetingDetail);
    // Gone behind the host's back: the rows still list the speaker.
    harness.store.delete_meeting(uuid(MEETING)).unwrap();
    let error = select(&harness, SPEAKER_ANNA, person_option(PERSON_ANNA, "Anna")).unwrap_err();
    assert_eq!(error.code, BridgeErrorCode::Failed);
    assert!(
        error.message.starts_with("Speaker could not be named: "),
        "{}",
        error.message
    );
    assert!(
        harness
            .fakes
            .pipeline
            .redeliveries
            .lock()
            .unwrap()
            .is_empty()
    );
}

/// Selecting the own person is a no-op; closing the picker redelivers once
/// after changes.
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
            reason: None,
            strength: None,
            person_id: Some(uuid(0xEE)),
        },
    )
    .unwrap_err();
    assert_eq!(unknown.code, BridgeErrorCode::NotFound);
    let blank = select(&harness, SPEAKER_UNKNOWN, create_option("   ")).unwrap_err();
    assert_eq!(blank.code, BridgeErrorCode::InvalidParams);
    assert_eq!(
        harness.store.persons().unwrap().len(),
        3,
        "no person was created"
    );
}

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
    assert_eq!(
        maya.reason,
        Some(SpeakerOptionReason::Invited),
        "Speaker 3 is a suggestion, so two voices are left"
    );

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

    let refused = select(&harness, SPEAKER_UNKNOWN, create_option("jérôme")).unwrap_err();
    assert_eq!(refused.code, BridgeErrorCode::WouldCombine);
    assert_eq!(
        refused.message,
        "Jérôme already speaks as Speaker 2 in this meeting."
    );
    assert_eq!(
        harness.store.speakers(uuid(MEETING)).unwrap()[3].person_id(),
        Some(person_id),
        "a refused pick changes nothing"
    );

    select_combining(&harness, SPEAKER_UNKNOWN, create_option("jérôme"), true).unwrap();
    let speakers = harness.store.speakers(uuid(MEETING)).unwrap();
    assert_eq!(
        speakers.len(),
        3,
        "Jérôme already owns Speaker 2: with combine the two merge"
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
            reason: Some(SpeakerOptionReason::Mentioned),
            strength: None,
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
            .any(|option| option.reason == Some(SpeakerOptionReason::Mentioned))
    );
}

/// Playing without a clip file reports an error.
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

/// A clip that ends on its own resets its play button at the next pass.
#[test]
fn a_clip_that_ends_republishes_the_detail() {
    use steno_host::services::ClipPlayer as _;
    let harness = sample();
    harness
        .host
        .speakers_play(SpeakerIdParams {
            speaker_id: uuid(SPEAKER_NICOLAI),
        })
        .unwrap();
    harness.host.reload_pass();
    harness.sink.clear();
    harness.fakes.clip_player.stop();
    harness.host.reload_pass();
    let detail = harness.sink.last(BridgeTopic::MeetingDetail).unwrap();
    assert_eq!(detail["speakers"][0]["isPlaying"], false);
}

/// Confirming a speaker once the audio is gone removes only the clip its
/// row names, under the run that wrote it.
#[test]
fn confirming_after_the_audio_is_gone_removes_the_run_clip_the_row_names() {
    use steno_core::paths::file_url;
    use steno_core::{RecordingLayout, Speaker, Store, testing::sample_data};
    use steno_host::speakers::{OptionKind, SpeakersViewModel};

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("steno.sqlite")).unwrap();
    let anna = sample_data::person(0, "Anna");
    store.save_person(&anna).unwrap();
    let meeting = sample_data::meeting();
    store.save_meeting(&meeting).unwrap();
    let layout = RecordingLayout::new(&dir.path().join("audio"), meeting.id);
    layout.create_directories(true).unwrap();
    let run = uuid::Uuid::new_v4();
    let (confirmed, other) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let unnamed = layout.sample_clip(confirmed);
    std::fs::write(&unnamed, b"an earlier run's clip").unwrap();
    for (id, label) in [(confirmed, "Speaker 1"), (other, "Speaker 2")] {
        let clip = layout.run_sample_clip(id, run);
        std::fs::write(&clip, b"clip").unwrap();
        store
            .save_speaker(&Speaker {
                id,
                meeting_id: meeting.id,
                cluster_label: label.to_owned(),
                assignment: SpeakerAssignment::Unknown,
                embedding: None,
                sample_clip_range: None,
                sample_clip_url: Some(file_url(&clip, false)),
                cluster_confidence: 1.0,
                dismissed: false,
            })
            .unwrap();
    }
    let mut speakers = SpeakersViewModel::default();
    speakers.update(
        store.export(meeting.id).unwrap(),
        &store,
        &steno_host::services::RealFileSystem,
    );

    let option = steno_host::speakers::SpeakerOption {
        kind: OptionKind::Person(anna),
        tag: None,
    };
    assert!(
        speakers
            .select(&option, confirmed, false, &store, meeting.started_at)
            .unwrap()
    );

    assert!(!layout.run_sample_clip(confirmed, run).exists());
    assert!(unnamed.exists(), "a file no row names is not confirm's");
    assert!(layout.run_sample_clip(other, run).exists());
    let stored = store.speakers(meeting.id).unwrap();
    let row = stored
        .iter()
        .find(|speaker| speaker.id == confirmed)
        .unwrap();
    assert_eq!(row.sample_clip_url, None);
}

/// Naming a meeting's "me" speaker the first time makes that person the
/// user's own: every other meeting whose "me" is still unknown is
/// confirmed as them, except one where they already own another speaker;
/// a "me" named otherwise stays, and a later naming changes nothing.
#[test]
fn naming_me_the_first_time_makes_that_person_you_in_every_unnamed_meeting() {
    use steno_core::{Speaker, Store, me_speaker_id, testing::sample_data};
    use steno_host::speakers::{OptionKind, SpeakersViewModel};
    use uuid::Uuid;

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("steno.sqlite")).unwrap();
    let anna = sample_data::person(0, "Anna");
    let ben = sample_data::person(1, "Ben");
    for person in [&anna, &ben] {
        store.save_person(person).unwrap();
    }
    let speaker = |meeting_id: Uuid, id: Uuid, label: &str, assignment| Speaker {
        id,
        meeting_id,
        cluster_label: label.to_owned(),
        assignment,
        embedding: None,
        sample_clip_range: None,
        sample_clip_url: None,
        cluster_confidence: 1.0,
        dismissed: false,
    };
    let confirmed = |person: &steno_core::Person| SpeakerAssignment::Confirmed {
        person_id: person.id,
    };
    // The meeting named here, one unnamed, one named Ben, one where Anna
    // owns "Speaker 1".
    let [named_here, unnamed, named_ben, anna_elsewhere] = std::array::from_fn(|_| Uuid::new_v4());
    for (meeting_id, me, other) in [
        (named_here, SpeakerAssignment::Unknown, None),
        (unnamed, SpeakerAssignment::Unknown, None),
        (named_ben, confirmed(&ben), None),
        (
            anna_elsewhere,
            SpeakerAssignment::Unknown,
            Some(confirmed(&anna)),
        ),
    ] {
        let mut meeting = sample_data::meeting();
        meeting.id = meeting_id;
        store.save_meeting(&meeting).unwrap();
        store
            .save_participant(&Participant {
                id: Uuid::new_v4(),
                meeting_id,
                person_id: None,
                display_name: "Me".to_owned(),
                role: ParticipantRole::Me,
                email: None,
            })
            .unwrap();
        store
            .save_speaker(&speaker(meeting_id, me_speaker_id(meeting_id), "Me", me))
            .unwrap();
        if let Some(assignment) = other {
            store
                .save_speaker(&speaker(
                    meeting_id,
                    Uuid::new_v4(),
                    "Speaker 1",
                    assignment,
                ))
                .unwrap();
        }
    }
    let name_me = |meeting_id: Uuid, person: &steno_core::Person| {
        let mut speakers = SpeakersViewModel::default();
        speakers.update(
            store.export(meeting_id).unwrap(),
            &store,
            &steno_host::services::RealFileSystem,
        );
        let option = steno_host::speakers::SpeakerOption {
            kind: OptionKind::Person(person.clone()),
            tag: None,
        };
        assert!(
            speakers
                .select(
                    &option,
                    me_speaker_id(meeting_id),
                    false,
                    &store,
                    sample_data::started_at()
                )
                .unwrap()
        );
        assert_eq!(speakers.error, None);
    };
    let me_of = |meeting_id: Uuid| {
        let export = store.export(meeting_id).unwrap();
        let me = export
            .speakers
            .iter()
            .find(|speaker| speaker.id == me_speaker_id(meeting_id))
            .unwrap()
            .person_id();
        let participant = &export.participants[0];
        (me, participant.person_id, participant.display_name.clone())
    };

    name_me(named_here, &anna);
    assert_eq!(store.settings().unwrap().self_person_id, Some(anna.id));
    let you = (Some(anna.id), Some(anna.id), "Anna".to_owned());
    assert_eq!(me_of(named_here), you);
    assert_eq!(me_of(unnamed), you);
    assert_eq!(me_of(named_ben), (Some(ben.id), None, "Me".to_owned()));
    assert_eq!(me_of(anna_elsewhere), (None, None, "Me".to_owned()));

    name_me(anna_elsewhere, &ben);
    assert_eq!(
        store.settings().unwrap().self_person_id,
        Some(anna.id),
        "a later naming keeps the first"
    );
}

fn reject(harness: &Harness, speaker: u32, person: u32) -> Result<(), steno_bridge::BridgeError> {
    harness.host.speakers_reject(RejectSpeakerParams {
        speaker_id: uuid(speaker),
        person_id: uuid(person),
    })
}

fn speaker_id(n: u32) -> SpeakerIdParams {
    SpeakerIdParams {
        speaker_id: uuid(n),
    }
}

/// No to "Sounds like Anna": the voice is unnamed, Anna is never suggested
/// for it again (only found when typed), and her voice is untouched.
#[test]
fn no_to_a_suggestion_records_it_and_leaves_voice_memory() {
    let harness = sample();
    let anna = harness.store.person(uuid(PERSON_ANNA)).unwrap().unwrap();
    harness.sink.clear();
    reject(&harness, SPEAKER_ANNA, PERSON_ANNA).unwrap();

    let detail = harness.sink.last(BridgeTopic::MeetingDetail).unwrap();
    assert_eq!(detail["speakers"][2]["assignment"], "unknown");
    assert!(detail["speakers"][2].get("suggestionName").is_none());
    assert!(detail["speakers"][2].get("suggestionStrength").is_none());
    assert_eq!(detail["speakers"][2]["displayName"], "Speaker 3");
    assert_eq!(
        harness.store.person(uuid(PERSON_ANNA)).unwrap().unwrap(),
        anna,
        "a No never touches voice memory"
    );
    assert_eq!(
        *harness.fakes.pipeline.redeliveries.lock().unwrap(),
        vec![uuid(MEETING)],
        "one re-export"
    );
    assert!(
        !options(&harness, SPEAKER_ANNA, "")
            .iter()
            .any(|option| option.person_id == Some(uuid(PERSON_ANNA))),
        "never suggested again"
    );
    assert!(
        options(&harness, SPEAKER_ANNA, "ann")
            .iter()
            .any(|option| option.person_id == Some(uuid(PERSON_ANNA))),
        "still found when typed"
    );
    assert!(
        options(&harness, SPEAKER_UNKNOWN, "")
            .iter()
            .all(|option| option.person_id != Some(uuid(PERSON_ANNA))
                || option.reason != Some(SpeakerOptionReason::Voice)),
        "the No is about Speaker 3 only"
    );

    let missing = reject(&harness, SPEAKER_ANNA, 0xEE).unwrap_err();
    assert_eq!(missing.code, BridgeErrorCode::Failed);
}

/// With one voice left unnamed and one invitee nobody is named as, that
/// invitee is the last one left; a second invitee makes them plain invited.
#[test]
fn the_last_invitee_left_is_offered_as_such() {
    let harness = sample();
    let invitee = |n: u32, name: &str| Participant {
        id: uuid(n),
        meeting_id: uuid(MEETING),
        person_id: None,
        display_name: name.to_owned(),
        role: ParticipantRole::Them,
        email: None,
    };
    harness
        .store
        .save_participant(&invitee(0x31, "Maya"))
        .unwrap();
    select(&harness, SPEAKER_ANNA, person_option(PERSON_ANNA, "Anna")).unwrap();
    let offered = options(&harness, SPEAKER_UNKNOWN, "");
    let maya = offered
        .iter()
        .find(|option| option.label == "Maya")
        .unwrap();
    assert_eq!(maya.reason, Some(SpeakerOptionReason::LastInvitee));
    assert_eq!(maya.strength, None);

    harness
        .store
        .save_participant(&invitee(0x32, "Tom"))
        .unwrap();
    harness.host.store_changed();
    let offered = options(&harness, SPEAKER_UNKNOWN, "");
    assert!(
        offered
            .iter()
            .filter(|option| option.label == "Maya" || option.label == "Tom")
            .all(|option| option.reason == Some(SpeakerOptionReason::Invited))
    );
}

/// Not a person: the voice leaves the counts and the chips, its turn keeps
/// its text and reads "Not a person"; Restore brings it back unnamed.
#[test]
fn not_a_person_reads_so_and_restore_brings_it_back() {
    let harness = sample();
    harness.sink.clear();
    harness
        .host
        .speakers_dismiss(speaker_id(SPEAKER_UNKNOWN))
        .unwrap();
    let detail = harness.sink.last(BridgeTopic::MeetingDetail).unwrap();
    assert_eq!(detail["speakers"][3]["dismissed"], true);
    assert_eq!(detail["speakers"][3]["assignment"], "unknown");
    let turn = detail["transcript"]
        .as_array()
        .unwrap()
        .iter()
        .find(|turn| turn["speakerID"] == json!(uuid(SPEAKER_UNKNOWN).to_string().to_uppercase()))
        .unwrap();
    assert_eq!(turn["speakerName"], "Not a person");
    assert_eq!(
        turn["text"],
        "Die Partner sollten das nicht aus zweiter Hand hören."
    );
    let list = harness.sink.last(BridgeTopic::MeetingsList).unwrap();
    assert_eq!(
        list["groups"][0]["meetings"][0]["speakers"]
            .as_array()
            .unwrap()
            .len(),
        3,
        "no chip for it"
    );
    assert!(harness.store.speakers(uuid(MEETING)).unwrap()[3].dismissed);

    harness
        .host
        .speakers_restore(speaker_id(SPEAKER_UNKNOWN))
        .unwrap();
    let detail = harness.sink.last(BridgeTopic::MeetingDetail).unwrap();
    assert_eq!(detail["speakers"][3]["dismissed"], false);
    assert_eq!(detail["transcript"][2]["speakerName"], "Speaker 4");
    assert_eq!(
        harness.fakes.pipeline.redeliveries.lock().unwrap().len(),
        2,
        "one re-export per change"
    );
}

/// Remove name: the voice is unnamed in this meeting, the person keeps
/// their name and their voice forgets it.
#[test]
fn remove_name_unnames_the_voice() {
    let harness = sample();
    harness
        .host
        .speakers_clear(speaker_id(SPEAKER_JEROME))
        .unwrap();
    let detail = harness.sink.last(BridgeTopic::MeetingDetail).unwrap();
    assert_eq!(detail["speakers"][1]["assignment"], "unknown");
    assert_eq!(detail["speakers"][1]["displayName"], "Speaker 2");
    let jerome = harness.store.person(uuid(PERSON_JEROME)).unwrap().unwrap();
    assert_eq!(jerome.display_name, "Jérôme");
    assert_eq!(jerome.sample_count, 0);
    // An unnamed voice again changes nothing.
    harness
        .host
        .speakers_clear(speaker_id(SPEAKER_JEROME))
        .unwrap();
    assert_eq!(harness.fakes.pipeline.redeliveries.lock().unwrap().len(), 1);
}

/// The quote is what the clip says: the voice's words inside the clip
/// range.
#[test]
fn the_quote_is_the_clips_words() {
    let harness = sample();
    let detail = harness.snapshot(BridgeTopic::MeetingDetail);
    assert_eq!(
        detail["speakers"][0]["quote"],
        "Lass uns kurz auf die Prioritäten schauen. Ich würde vorschlagen, dass wir neunzig Prozent auf den Kern setzen."
    );
    assert!(
        detail["speakers"][2].get("quote").is_none(),
        "a voice that says nothing has no quote"
    );
    assert_eq!(
        detail["speakers"][2]["suggestionStrength"],
        json!(VoiceStrength::of(0.87))
    );
}

/// A voice match below the floor is not shown: the speaker reads as
/// unnamed and the options carry no voice reason; between the floor and
/// the strong threshold it is "likely".
#[test]
fn a_weak_voice_match_is_hidden_and_a_middling_one_is_likely() {
    let harness = sample();
    let mut speaker = harness.store.speakers(uuid(MEETING)).unwrap()[2].clone();
    speaker.assignment = SpeakerAssignment::Suggested {
        person_id: uuid(PERSON_ANNA),
        similarity: 0.55,
    };
    harness.store.save_speaker(&speaker).unwrap();
    harness.host.store_changed();
    let detail = harness.snapshot(BridgeTopic::MeetingDetail);
    assert_eq!(detail["speakers"][2]["assignment"], "unknown");
    assert_eq!(detail["speakers"][2]["displayName"], "Speaker 3");
    assert!(
        options(&harness, SPEAKER_ANNA, "")
            .iter()
            .all(|option| option.reason != Some(SpeakerOptionReason::Voice))
    );

    speaker.assignment = SpeakerAssignment::Suggested {
        person_id: uuid(PERSON_ANNA),
        similarity: 0.66,
    };
    harness.store.save_speaker(&speaker).unwrap();
    harness.host.store_changed();
    let detail = harness.snapshot(BridgeTopic::MeetingDetail);
    assert_eq!(detail["speakers"][2]["suggestionStrength"], "likely");
    assert_eq!(
        options(&harness, SPEAKER_ANNA, "")[0].strength,
        Some(VoiceStrength::Likely)
    );
}
