//! Moving one paragraph, playing it, and Separate again, through the host.

#![allow(clippy::too_many_lines)]

mod common;

use steno_host::services::FileSystem as _;

use common::*;
use serde_json::Value;
use steno_bridge::{
    BridgeErrorCode, BridgeHost, BridgeTopic, PlayTurnParams, SelectSpeakerParams,
    SetTurnSpeakerParams, SpeakerIdParams, SpeakerOption, SpeakerOptionKind, TurnSegment,
};
use uuid::Uuid;

fn sample() -> Harness {
    Harness::builder().seed(populate_sample).build()
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

/// The id as the snapshot spells it.
fn uuid_text(n: u32) -> String {
    steno_core::json::uuid_string(uuid(n))
}

fn detail(harness: &Harness) -> Value {
    harness.snapshot(BridgeTopic::MeetingDetail)
}

/// The turn's segments as the page sends them, from the snapshot.
fn turn_segments(turn: &Value) -> Vec<TurnSegment> {
    let speaker_id = turn["speakerID"]
        .as_str()
        .map(|id| Uuid::parse_str(id).unwrap());
    turn["segmentIDs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| TurnSegment {
            id: Uuid::parse_str(id.as_str().unwrap()).unwrap(),
            speaker_id,
        })
        .collect()
}

fn move_turn(
    harness: &Harness,
    turn: &Value,
    generation: i64,
    option: SpeakerOption,
) -> Result<(), steno_bridge::BridgeError> {
    harness
        .host
        .transcript_set_turn_speaker(SetTurnSpeakerParams {
            meeting_id: uuid(MEETING),
            generation,
            segments: turn_segments(turn),
            option,
        })
}

fn redeliveries(harness: &Harness) -> usize {
    harness.fakes.pipeline.redeliveries.lock().unwrap().len()
}

#[test]
fn the_snapshot_carries_segments_the_generation_and_combined_voices() {
    let harness = sample();
    let detail = detail(&harness);
    assert_eq!(detail["transcriptGeneration"], 1);
    let first = &detail["transcript"][0];
    assert_eq!(
        first["segmentIDs"],
        serde_json::json!([uuid_text(0x64), uuid_text(0x6E)]),
        "Speaker 5's paragraph reads as Nicolai's and folds into his turn"
    );
    assert_eq!(first["isPlaying"], false);
    let nicolai = &detail["speakers"][0];
    assert_eq!(nicolai["clusterLabel"], "Speaker 1");
    assert_eq!(
        nicolai["combined"],
        serde_json::json!([{
            "id": uuid_text(SPEAKER_COMBINED),
            "clusterLabel": "Speaker 5",
            "quote": "Und die Nebenprojekte pausieren wir bis zur Q1-Planung.",
            "hasClip": true,
            "isPlaying": false,
        }])
    );
    assert_eq!(
        detail["speakers"].as_array().unwrap().len(),
        4,
        "a combined voice is not a row of its own"
    );

    // Its own clip plays.
    harness
        .host
        .speakers_play(SpeakerIdParams {
            speaker_id: uuid(SPEAKER_COMBINED),
        })
        .unwrap();
    assert_eq!(
        self::detail(&harness)["speakers"][0]["combined"][0]["isPlaying"],
        true
    );
}

#[test]
fn a_paragraph_moves_to_a_new_voice_and_its_emptied_voice_goes() {
    let harness = sample();
    let before = detail(&harness);
    let turn = before["transcript"][2].clone();
    assert_eq!(turn["speakerName"], "Speaker 4");

    move_turn(&harness, &turn, 1, person_option(PERSON_ANNA, "Anna")).unwrap();
    let after = detail(&harness);
    let moved = &after["transcript"][2];
    assert_eq!(moved["speakerName"], "Anna");
    let anna_voice = after["speakers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|speaker| speaker["id"] == moved["speakerID"])
        .unwrap();
    assert_eq!(anna_voice["clusterLabel"], "Speaker 6");
    assert_eq!(anna_voice["assignment"], "confirmed");
    assert_eq!(anna_voice["hasClip"], false);
    assert!(
        after["speakers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|speaker| speaker["clusterLabel"] != "Speaker 4"),
        "Speaker 4 had no other paragraph"
    );
    assert_eq!(redeliveries(&harness), 1, "re-exported once");
}

#[test]
fn a_paragraph_moves_to_the_voice_its_person_has() {
    let harness = sample();
    let turn = detail(&harness)["transcript"][2].clone();
    move_turn(&harness, &turn, 1, person_option(PERSON_JEROME, "Jérôme")).unwrap();
    let after = detail(&harness);
    // Jérôme's turn and the moved one are next to each other: one turn.
    assert_eq!(after["transcript"].as_array().unwrap().len(), 2);
    assert_eq!(after["transcript"][1]["speakerName"], "Jérôme");
    assert_eq!(
        after["transcript"][1]["segmentIDs"],
        serde_json::json!([uuid_text(0x65), uuid_text(0x66)])
    );
}

#[test]
fn a_stale_move_is_refused_and_changes_nothing() {
    let harness = sample();
    let before = detail(&harness);
    let turn = before["transcript"][2].clone();

    let old_generation = move_turn(&harness, &turn, 0, person_option(PERSON_ANNA, "Anna"))
        .expect_err("an older transcript");
    assert_eq!(old_generation.code, BridgeErrorCode::Stale);
    assert_eq!(
        old_generation.message,
        "The transcript changed. Refresh to see it as it is now."
    );

    let mut wrong_voice = turn.clone();
    wrong_voice["speakerID"] = Value::String(uuid_text(SPEAKER_JEROME));
    let refused = move_turn(
        &harness,
        &wrong_voice,
        1,
        person_option(PERSON_ANNA, "Anna"),
    )
    .expect_err("another voice than the page showed");
    assert_eq!(refused.code, BridgeErrorCode::Stale);

    let other_meeting = harness
        .host
        .transcript_set_turn_speaker(SetTurnSpeakerParams {
            meeting_id: uuid(MEETING_FAILED),
            generation: 1,
            segments: turn_segments(&turn),
            option: person_option(PERSON_ANNA, "Anna"),
        })
        .expect_err("not the selected meeting");
    assert_eq!(other_meeting.code, BridgeErrorCode::Stale);

    let after = detail(&harness);
    assert_eq!(after["transcript"], before["transcript"]);
    assert_eq!(after["speakers"], before["speakers"]);
    assert_eq!(redeliveries(&harness), 0);
}

#[test]
fn a_turn_plays_its_own_span_capped_at_a_minute() {
    let harness = sample();
    let turn = detail(&harness)["transcript"][0].clone();
    let segment_ids: Vec<Uuid> = turn_segments(&turn).iter().map(|s| s.id).collect();
    harness
        .host
        .transcript_play_turn(PlayTurnParams {
            meeting_id: uuid(MEETING),
            segment_ids: segment_ids.clone(),
        })
        .unwrap();
    let master = master_path(&harness.audio_folder());
    assert_eq!(
        *harness.fakes.clip_player.spans.lock().unwrap(),
        [(master.clone(), 12.0, 41.0)]
    );
    let playing = detail(&harness);
    assert_eq!(playing["transcript"][0]["isPlaying"], true);
    assert_eq!(playing["speakers"][0]["isPlaying"], false);

    // A clip stops it; stop stops both.
    harness
        .host
        .speakers_play(SpeakerIdParams {
            speaker_id: uuid(SPEAKER_JEROME),
        })
        .unwrap();
    let switched = detail(&harness);
    assert_eq!(switched["transcript"][0]["isPlaying"], false);
    assert_eq!(switched["speakers"][1]["isPlaying"], true);
    harness.host.speakers_stop().unwrap();
    assert_eq!(detail(&harness)["speakers"][1]["isPlaying"], false);

    // The whole transcript as one span: 12 s to 90 s, cut to 60 s.
    let every = detail(&harness)["transcript"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|turn| turn_segments(turn).into_iter().map(|s| s.id))
        .collect();
    harness
        .host
        .transcript_play_turn(PlayTurnParams {
            meeting_id: uuid(MEETING),
            segment_ids: every,
        })
        .unwrap();
    assert_eq!(
        harness.fakes.clip_player.spans.lock().unwrap().last(),
        Some(&(master, 12.0, 72.0))
    );
}

#[test]
fn no_turn_plays_once_the_recording_is_gone() {
    let harness = sample();
    harness
        .fakes
        .file_system
        .remove(&master_path(&harness.audio_folder()))
        .unwrap();
    harness.host.store_changed();
    let detail = detail(&harness);
    assert_eq!(detail["retention"]["filesExist"], false);
    let turn = detail["transcript"][0].clone();
    let refused = harness
        .host
        .transcript_play_turn(PlayTurnParams {
            meeting_id: uuid(MEETING),
            segment_ids: turn_segments(&turn).iter().map(|s| s.id).collect(),
        })
        .expect_err("no recording");
    assert_eq!(refused.code, BridgeErrorCode::NotFound);
    assert_eq!(refused.message, "The recording is gone.");
    assert!(harness.fakes.clip_player.spans.lock().unwrap().is_empty());
}

#[test]
fn separate_again_gives_the_combined_voice_back() {
    let harness = sample();
    harness
        .host
        .speakers_separate(SpeakerIdParams {
            speaker_id: uuid(SPEAKER_COMBINED),
        })
        .unwrap();
    let after = detail(&harness);
    let labels: Vec<&str> = after["speakers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|speaker| speaker["clusterLabel"].as_str().unwrap())
        .collect();
    assert_eq!(
        labels,
        [
            "Speaker 1",
            "Speaker 2",
            "Speaker 3",
            "Speaker 4",
            "Speaker 5"
        ]
    );
    assert_eq!(after["speakers"][0]["combined"], serde_json::json!([]));
    assert_eq!(after["speakers"][4]["assignment"], "unknown");
    assert_eq!(after["transcript"][1]["speakerName"], "Speaker 5");
    assert_eq!(redeliveries(&harness), 1);

    // Twice is stale: nothing is combined any more.
    let again = harness
        .host
        .speakers_separate(SpeakerIdParams {
            speaker_id: uuid(SPEAKER_COMBINED),
        })
        .expect_err("not combined");
    assert_eq!(again.code, BridgeErrorCode::Stale);

    // Combining once more, and undoing it, round-trips.
    let before = detail(&harness);
    harness
        .host
        .speakers_select(SelectSpeakerParams {
            speaker_id: uuid(SPEAKER_COMBINED),
            option: person_option(PERSON_NICOLAI, "Nicolai"),
            combine: true,
        })
        .unwrap();
    assert_eq!(
        detail(&harness)["speakers"][0]["combined"][0]["id"],
        uuid_text(SPEAKER_COMBINED)
    );
    harness
        .host
        .speakers_separate(SpeakerIdParams {
            speaker_id: uuid(SPEAKER_COMBINED),
        })
        .unwrap();
    let undone = detail(&harness);
    assert_eq!(undone["speakers"], before["speakers"]);
    assert_eq!(undone["transcript"], before["transcript"]);
}
