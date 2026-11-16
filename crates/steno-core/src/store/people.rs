//! `person`, `participant`, `speaker` and `speakerNameSuggestion` rows.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use uuid::Uuid;

use super::convert::{DbDate, DbEmbedding, DbEnum, DbUuid, RowExt as _};
use super::{
    Result, Store, StoreError, assets, execute_cached, insert_sql, query_all, settings, upsert_sql,
};
use crate::model::{
    Embedding, Participant, ParticipantRole, Person, Speaker, SpeakerAssignment,
    SpeakerAssignmentKind, SpeakerNameSuggestion, TimeRange, me_speaker_id, room_speaker_id,
};
use crate::paths::file_url_path;

const PERSON_COLUMNS: &str = "id, displayName, email, embedding, sampleCount, createdAt";

fn person_from_row(row: &Row<'_>) -> rusqlite::Result<Person> {
    Ok(Person {
        id: row.col::<DbUuid>("id")?,
        display_name: row.get("displayName")?,
        email: row.get("email")?,
        embedding: row.col::<Option<DbEmbedding<_>>>("embedding")?,
        sample_count: row.get("sampleCount")?,
        created_at: row.col::<DbDate>("createdAt")?,
    })
}

pub(super) fn fetch_person(connection: &Connection, id: Uuid) -> Result<Option<Person>> {
    Ok(connection
        .query_row(
            &format!("SELECT {PERSON_COLUMNS} FROM person WHERE id = ?1"),
            [DbUuid(id)],
            person_from_row,
        )
        .optional()?)
}

pub(super) fn save_person(connection: &Connection, person: &Person) -> Result<()> {
    connection.execute(
        &upsert_sql("person", PERSON_COLUMNS),
        params![
            DbUuid(person.id),
            person.display_name,
            person.email,
            person.embedding.as_ref().map(DbEmbedding),
            person.sample_count,
            DbDate(person.created_at),
        ],
    )?;
    Ok(())
}

const PARTICIPANT_COLUMNS: &str = "id, meetingID, personID, displayName, role, email";

fn participant_from_row(row: &Row<'_>) -> rusqlite::Result<Participant> {
    Ok(Participant {
        id: row.col::<DbUuid>("id")?,
        meeting_id: row.col::<DbUuid>("meetingID")?,
        person_id: row.col::<Option<DbUuid>>("personID")?,
        display_name: row.get("displayName")?,
        role: row.col::<DbEnum<_>>("role")?,
        email: row.get("email")?,
    })
}

pub(super) fn save_participant(connection: &Connection, participant: &Participant) -> Result<()> {
    execute_cached(
        connection,
        &upsert_sql("participant", PARTICIPANT_COLUMNS),
        params![
            DbUuid(participant.id),
            DbUuid(participant.meeting_id),
            participant.person_id.map(DbUuid),
            participant.display_name,
            DbEnum(participant.role),
            participant.email,
        ],
    )
}

const SPEAKER_COLUMNS: &str = "id, meetingID, clusterLabel, assignment, personID, similarity, \
     embedding, sampleClipStart, sampleClipEnd, sampleClipURL, clusterConfidence, dismissed";

fn speaker_from_row(row: &Row<'_>) -> rusqlite::Result<Speaker> {
    let kind = row.col::<DbEnum<_>>("assignment")?;
    let person_id = row.col::<Option<DbUuid>>("personID")?;
    let similarity: Option<f64> = row.get("similarity")?;
    let clip_start: Option<f64> = row.get("sampleClipStart")?;
    let clip_end: Option<f64> = row.get("sampleClipEnd")?;
    let confidence: f64 = row.get("clusterConfidence")?;
    // Float columns hold an `f32` widened to `f64` (as the Swift app writes
    // them); narrowing back is exact.
    #[allow(clippy::cast_possible_truncation)]
    let narrow = |value: f64| value as f32;
    Ok(Speaker {
        id: row.col::<DbUuid>("id")?,
        meeting_id: row.col::<DbUuid>("meetingID")?,
        cluster_label: row.get("clusterLabel")?,
        assignment: SpeakerAssignment::from_columns(kind, person_id, similarity.map(narrow)),
        embedding: row.col::<Option<DbEmbedding<_>>>("embedding")?,
        sample_clip_range: match (clip_start, clip_end) {
            (Some(lower), Some(upper)) if lower <= upper => Some(TimeRange { lower, upper }),
            _ => None,
        },
        sample_clip_url: row.get("sampleClipURL")?,
        cluster_confidence: narrow(confidence),
        dismissed: row.get("dismissed")?,
    })
}

fn write_speaker(connection: &Connection, speaker: &Speaker, sql: &str) -> Result<()> {
    execute_cached(
        connection,
        sql,
        params![
            DbUuid(speaker.id),
            DbUuid(speaker.meeting_id),
            speaker.cluster_label,
            DbEnum(speaker.assignment.kind()),
            speaker.assignment.person_id().map(DbUuid),
            speaker.assignment.similarity().map(f64::from),
            speaker.embedding.as_ref().map(DbEmbedding),
            speaker.sample_clip_range.map(|range| range.lower),
            speaker.sample_clip_range.map(|range| range.upper),
            speaker.sample_clip_url,
            f64::from(speaker.cluster_confidence),
            speaker.dismissed,
        ],
    )
}

pub(super) fn insert_speaker(connection: &Connection, speaker: &Speaker) -> Result<()> {
    write_speaker(connection, speaker, &insert_sql("speaker", SPEAKER_COLUMNS))
}

pub(super) fn save_speaker(connection: &Connection, speaker: &Speaker) -> Result<()> {
    write_speaker(connection, speaker, &upsert_sql("speaker", SPEAKER_COLUMNS))
}

pub(super) fn fetch_speaker(connection: &Connection, id: Uuid) -> Result<Option<Speaker>> {
    Ok(connection
        .query_row(
            &format!("SELECT {SPEAKER_COLUMNS} FROM speaker WHERE id = ?1"),
            [DbUuid(id)],
            speaker_from_row,
        )
        .optional()?)
}

/// The voice a combined row joined: `None` for a root or a missing row.
fn merged_into(connection: &Connection, id: Uuid) -> Result<Option<Uuid>> {
    Ok(connection
        .query_row(
            "SELECT mergedInto FROM speaker WHERE id = ?1",
            [DbUuid(id)],
            |row| row.col::<Option<DbUuid>>("mergedInto"),
        )
        .optional()?
        .flatten())
}

/// The speaker `id` reads as: the voice it is combined into, or `id`
/// itself. Combining flattens ([`merge_speaker_rows`]), so a combined row
/// always points at a root and no chain is longer than one step.
pub(super) fn root_of(connection: &Connection, id: Uuid) -> Result<Uuid> {
    Ok(merged_into(connection, id)?.unwrap_or(id))
}

/// Two clusters inside one meeting, combined so it can be undone:
/// `source` reads as `target`'s root from now on (`mergedInto`), and so
/// does every row that was combined into `source`, re-pointed in the same
/// update, so every combined row points straight at a root. Nothing else
/// changes on any row: `source` keeps its segments, embedding, clip,
/// assignment, dismissal, answers and `confirmedSimilarity` (which stays
/// the score of its own answer; combining records none), so
/// [`Store::separate_speaker`] only clears the column. Returns the persons
/// whose voices the combine touched; voices are not refreshed here.
pub(super) fn merge_speaker_rows(
    connection: &Connection,
    source: Uuid,
    target: Uuid,
    meeting_id: Uuid,
) -> Result<BTreeSet<Uuid>> {
    let mut person_ids = BTreeSet::new();
    if source == target {
        return Ok(person_ids);
    }
    let merged = fetch_speaker(connection, source)?.ok_or(StoreError::SpeakerNotFound(source))?;
    let kept = fetch_speaker(connection, target)?.ok_or(StoreError::SpeakerNotFound(target))?;
    if merged.meeting_id != meeting_id || kept.meeting_id != meeting_id {
        return Err(StoreError::SpeakersInDifferentMeetings(source, target));
    }
    let root = root_of(connection, target)?;
    if root == source {
        return Ok(person_ids);
    }
    person_ids.extend(confirmed_person(&merged));
    if let Some(root_speaker) = fetch_speaker(connection, root)? {
        person_ids.extend(confirmed_person(&root_speaker));
    }
    connection.execute(
        "UPDATE speaker SET mergedInto = ?1 WHERE id = ?2 OR mergedInto = ?2",
        params![DbUuid(root), DbUuid(source)],
    )?;
    Ok(person_ids)
}

/// The person a speaker is confirmed as.
fn confirmed_person(speaker: &Speaker) -> Option<Uuid> {
    match speaker.assignment {
        SpeakerAssignment::Confirmed { person_id } => Some(person_id),
        _ => None,
    }
}

/// Nulls a confirmed speaker's `sampleClipURL` and returns it for removal
/// when the meeting's master no longer exists (or never did).
fn drop_clip_when_audio_is_gone(
    connection: &Connection,
    speaker_id: Uuid,
) -> Result<Option<String>> {
    let Some(mut speaker) = fetch_speaker(connection, speaker_id)? else {
        return Ok(None);
    };
    let Some(clip) = speaker.sample_clip_url.clone() else {
        return Ok(None);
    };
    let master_exists = assets::assets_of_meeting(connection, speaker.meeting_id)?
        .first()
        .and_then(|asset| file_url_path(&asset.url))
        .is_some_and(|path| path.exists());
    if master_exists {
        return Ok(None);
    }
    speaker.sample_clip_url = None;
    save_speaker(connection, &speaker)?;
    Ok(Some(clip))
}

/// How many confirmed speakers a person's voice is computed from.
const VOICE_WINDOW: usize = 50;

/// Recomputes a person's voice: the normalised mean of the newest
/// `VOICE_WINDOW` confirmed embeddings by `meeting.startedAt`, and their
/// count: each voice confirmed as the person and every speaker combined
/// into one; a dismissed voice never counts. A missing person is ignored.
pub(super) fn refresh_voice(connection: &Connection, person_id: Uuid) -> Result<()> {
    let Some(mut person) = fetch_person(connection, person_id)? else {
        return Ok(());
    };
    // Every root confirmed as the person, and every row combined into one
    // of them, whatever its own assignment. A combined row points straight
    // at its root.
    let embeddings: Vec<Embedding> = query_all(
        connection,
        "SELECT speaker.embedding AS embedding FROM speaker \
         JOIN speaker AS root ON root.id = COALESCE(speaker.mergedInto, speaker.id) \
         JOIN meeting ON meeting.id = speaker.meetingID \
         WHERE root.personID = ?1 AND root.assignment = ?2 AND root.dismissed = 0 \
         AND root.mergedInto IS NULL AND speaker.embedding IS NOT NULL \
         ORDER BY meeting.startedAt DESC, speaker.id",
        params![DbUuid(person_id), SpeakerAssignmentKind::Confirmed.as_str()],
        |row| row.col::<DbEmbedding<Embedding>>("embedding"),
    )?
    .into_iter()
    .filter(|embedding| embedding.0.len() == Embedding::DIMENSION)
    .take(VOICE_WINDOW)
    .collect();
    person.embedding = Embedding::mean(&embeddings);
    person.sample_count = i64::try_from(embeddings.len()).unwrap_or(i64::MAX);
    save_person(connection, &person)
}

/// The meeting's "me" participant takes `person`'s id and name.
fn name_me_participant(connection: &Connection, meeting_id: Uuid, person: &Person) -> Result<()> {
    connection.execute(
        "UPDATE participant SET personID = ?1, displayName = ?2 WHERE meetingID = ?3 AND role = ?4",
        params![
            DbUuid(person.id),
            person.display_name,
            DbUuid(meeting_id),
            DbEnum(ParticipantRole::Me)
        ],
    )?;
    Ok(())
}

/// The label the meeting's "me" participant and speaker carry while the
/// user is unnamed (`LaneMerger::ME_SPEAKER_LABEL` in `steno-pipeline`).
const ME_LABEL: &str = "Me";

/// The meeting's "me" participant back to unnamed: no person, named
/// [`ME_LABEL`]. Who the user is (`Settings::self_person_id`) stays.
fn unname_me_participant(connection: &Connection, meeting_id: Uuid) -> Result<()> {
    connection.execute(
        "UPDATE participant SET personID = NULL, displayName = ?1 WHERE meetingID = ?2 AND role = ?3",
        params![ME_LABEL, DbUuid(meeting_id), DbEnum(ParticipantRole::Me)],
    )?;
    Ok(())
}

/// The voice the meeting's "me" speaker reads as: the "me" speaker itself,
/// or the voice it is combined into.
fn me_root(connection: &Connection, meeting_id: Uuid) -> Result<Uuid> {
    root_of(connection, me_speaker_id(meeting_id))
}

/// Called after a write touched `speaker_id`: when the "me" speaker reads
/// as the same voice, the meeting's "me" participant follows that voice,
/// named after the person it is confirmed as, else unnamed. So renaming,
/// removing the name of, dismissing, combining or separating the voice a
/// combined "me" speaker joined renames the participant too. Checked after
/// the write, so a combine that moved the "me" speaker's voice counts. A
/// meeting without a "me" speaker is left alone.
fn follow_me(connection: &Connection, meeting_id: Uuid, speaker_id: Uuid) -> Result<()> {
    let me = me_speaker_id(meeting_id);
    if fetch_speaker(connection, me)?.is_none() {
        return Ok(());
    }
    if root_of(connection, me)? != root_of(connection, speaker_id)? {
        return Ok(());
    }
    let person = match fetch_speaker(connection, me_root(connection, meeting_id)?)?
        .as_ref()
        .and_then(confirmed_person)
    {
        Some(person_id) => fetch_person(connection, person_id)?,
        None => None,
    };
    match person {
        Some(person) => name_me_participant(connection, meeting_id, &person),
        None => unname_me_participant(connection, meeting_id),
    }
}

/// The speaker back to `unknown`, inside a caller's transaction: the voice
/// of the person it was confirmed to is recomputed without it, and when the
/// "me" speaker reads as it, the "me" participant follows. A speaker that is
/// already unknown is left alone. Returns the speaker as written.
fn unassign_speaker(connection: &Connection, speaker_id: Uuid) -> Result<Speaker> {
    let mut speaker =
        fetch_speaker(connection, speaker_id)?.ok_or(StoreError::SpeakerNotFound(speaker_id))?;
    let previous = speaker.assignment.clone();
    if previous == SpeakerAssignment::Unknown {
        return Ok(speaker);
    }
    speaker.assignment = SpeakerAssignment::Unknown;
    save_speaker(connection, &speaker)?;
    if let SpeakerAssignment::Confirmed { person_id } = previous {
        set_confirmed_similarity(connection, speaker_id, None)?;
        refresh_voice(connection, person_id)?;
        follow_me(connection, speaker.meeting_id, speaker_id)?;
    }
    Ok(speaker)
}

/// The voice match's score for an answer about `speaker` and `person`:
/// the similarity Steno suggested `person` with, else the cosine of the
/// voice and the person's stored voice; `None` without either.
fn answer_similarity(speaker: &Speaker, person: &Person) -> Option<f32> {
    match speaker.assignment {
        SpeakerAssignment::Suggested {
            person_id,
            similarity,
        } if person_id == person.id => Some(similarity),
        _ => Some(
            speaker
                .embedding
                .as_ref()?
                .cosine_similarity(person.embedding.as_ref()?),
        ),
    }
}

/// `speaker.confirmedSimilarity`: the voice match's score when the user
/// said Yes ([`answer_similarity`]), a labelled positive for the floor as a
/// No's score is a labelled negative. Not in [`Speaker`]: only
/// `confirm_speaker` sets it, and every save keeps it; it is cleared when
/// the confirmation goes and carried when Process again keeps it.
pub(super) fn set_confirmed_similarity(
    connection: &Connection,
    speaker_id: Uuid,
    similarity: Option<f32>,
) -> Result<()> {
    execute_cached(
        connection,
        "UPDATE speaker SET confirmedSimilarity = ?1 WHERE id = ?2",
        params![similarity.map(f64::from), DbUuid(speaker_id)],
    )
}

/// The score [`set_confirmed_similarity`] kept for `speaker_id`.
pub(super) fn confirmed_similarity(
    connection: &Connection,
    speaker_id: Uuid,
) -> Result<Option<f32>> {
    let similarity: Option<Option<f64>> = connection
        .query_row(
            "SELECT confirmedSimilarity FROM speaker WHERE id = ?1",
            [DbUuid(speaker_id)],
            |row| row.get(0),
        )
        .optional()?;
    // An `f32` widened to `f64`, as `speaker.similarity`.
    #[allow(clippy::cast_possible_truncation)]
    Ok(similarity.flatten().map(|value| value as f32))
}

const REJECTION_COLUMNS: &str = "speakerID, personID, meetingID, similarity";

/// One No: the person the user said is not this voice, and the voice
/// match's score then, when there was one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Rejection {
    pub speaker_id: Uuid,
    pub person_id: Uuid,
    pub similarity: Option<f32>,
}

/// The meeting's Noes, by speaker id then person id.
pub(super) fn suggestion_rejections_of_meeting(
    connection: &Connection,
    meeting_id: Uuid,
) -> Result<Vec<Rejection>> {
    query_all(
        connection,
        &format!(
            "SELECT {REJECTION_COLUMNS} FROM speakerSuggestionRejection WHERE meetingID = ?1 \
             ORDER BY speakerID, personID"
        ),
        [DbUuid(meeting_id)],
        |row| {
            let similarity: Option<f64> = row.get("similarity")?;
            // An `f32` widened to `f64`, as `speaker.similarity`.
            #[allow(clippy::cast_possible_truncation)]
            Ok(Rejection {
                speaker_id: row.col::<DbUuid>("speakerID")?,
                person_id: row.col::<DbUuid>("personID")?,
                similarity: similarity.map(|value| value as f32),
            })
        },
    )
}

fn insert_rejection(
    connection: &Connection,
    meeting_id: Uuid,
    rejection: &Rejection,
) -> Result<()> {
    execute_cached(
        connection,
        &format!(
            "INSERT OR IGNORE INTO speakerSuggestionRejection ({REJECTION_COLUMNS}) \
             SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (SELECT 1 FROM speaker WHERE id = ?1) \
             AND EXISTS (SELECT 1 FROM person WHERE id = ?2)"
        ),
        params![
            DbUuid(rejection.speaker_id),
            DbUuid(rejection.person_id),
            DbUuid(meeting_id),
            rejection.similarity.map(f64::from)
        ],
    )
}

/// Writes rejections back for the meeting's speakers and persons that
/// exist; the others went with their rows.
pub(super) fn restore_suggestion_rejections(
    connection: &Connection,
    meeting_id: Uuid,
    rejections: &[Rejection],
) -> Result<()> {
    for rejection in rejections {
        insert_rejection(connection, meeting_id, rejection)?;
    }
    Ok(())
}

/// The meeting's voices: every speaker not combined into another.
pub(super) fn speakers_of_meeting(
    connection: &Connection,
    meeting_id: Uuid,
) -> Result<Vec<Speaker>> {
    query_all(
        connection,
        &format!(
            "SELECT {SPEAKER_COLUMNS} FROM speaker WHERE meetingID = ?1 AND mergedInto IS NULL \
             ORDER BY clusterLabel, id"
        ),
        [DbUuid(meeting_id)],
        speaker_from_row,
    )
}

/// Every speaker row of the meeting, combined ones included: what names
/// files on the disk.
pub(super) fn all_speakers_of_meeting(
    connection: &Connection,
    meeting_id: Uuid,
) -> Result<Vec<Speaker>> {
    query_all(
        connection,
        &format!(
            "SELECT {SPEAKER_COLUMNS} FROM speaker WHERE meetingID = ?1 ORDER BY clusterLabel, id"
        ),
        [DbUuid(meeting_id)],
        speaker_from_row,
    )
}

/// A speaker combined into another voice of its meeting.
#[derive(Debug, Clone, PartialEq)]
pub struct MergedSpeaker {
    /// The row as it is, with its own assignment, clip and embedding.
    pub speaker: Speaker,
    /// The voice it reads as: the voice it joined, since combining
    /// flattens ([`merge_speaker_rows`]).
    pub root: Uuid,
}

pub(super) fn merged_speakers_of_meeting(
    connection: &Connection,
    meeting_id: Uuid,
) -> Result<Vec<MergedSpeaker>> {
    query_all(
        connection,
        &format!(
            "SELECT {SPEAKER_COLUMNS}, mergedInto FROM speaker \
             WHERE meetingID = ?1 AND mergedInto IS NOT NULL ORDER BY clusterLabel, id"
        ),
        [DbUuid(meeting_id)],
        |row| {
            Ok(MergedSpeaker {
                speaker: speaker_from_row(row)?,
                root: row.col::<DbUuid>("mergedInto")?,
            })
        },
    )
}

const SUGGESTION_COLUMNS: &str = "speakerID, meetingID, name, confidence, evidence";

/// Replaces the meeting's suggestions with the named ones that point at
/// one of its speakers; ascending by confidence, so a duplicate speaker
/// ends with its strongest.
pub(super) fn replace_name_suggestions(
    connection: &Connection,
    meeting_id: Uuid,
    suggestions: &[SpeakerNameSuggestion],
) -> Result<()> {
    connection.execute(
        "DELETE FROM speakerNameSuggestion WHERE meetingID = ?1",
        [DbUuid(meeting_id)],
    )?;
    let speaker_ids = query_all(
        connection,
        "SELECT id FROM speaker WHERE meetingID = ?1",
        [DbUuid(meeting_id)],
        |row| row.col::<DbUuid>("id"),
    )?;
    let mut sorted: Vec<&SpeakerNameSuggestion> = suggestions
        .iter()
        .filter(|suggestion| speaker_ids.contains(&suggestion.speaker_id))
        .collect();
    sorted.sort_by(|left, right| left.confidence.total_cmp(&right.confidence));
    for suggestion in sorted {
        let Some(name) = suggestion
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        execute_cached(
            connection,
            &upsert_sql("speakerNameSuggestion", SUGGESTION_COLUMNS),
            params![
                DbUuid(suggestion.speaker_id),
                DbUuid(meeting_id),
                name,
                suggestion.confidence,
                suggestion.evidence,
            ],
        )?;
    }
    Ok(())
}

/// The meeting's suggestions, at most one per speaker, in speaker id order.
pub(super) fn name_suggestions_of_meeting(
    connection: &Connection,
    meeting_id: Uuid,
) -> Result<Vec<SpeakerNameSuggestion>> {
    query_all(
        connection,
        "SELECT speakerID, name, confidence, evidence FROM speakerNameSuggestion \
         WHERE meetingID = ?1 ORDER BY speakerID",
        [DbUuid(meeting_id)],
        |row| {
            Ok(SpeakerNameSuggestion {
                speaker_id: row.col::<DbUuid>("speakerID")?,
                name: Some(row.get("name")?),
                confidence: row.get("confidence")?,
                evidence: row.get("evidence")?,
            })
        },
    )
}

/// The meeting's participants by display name, ties by id: the order
/// `meeting.json` lists them in.
pub(super) fn participants_of_meeting(
    connection: &Connection,
    meeting_id: Uuid,
) -> Result<Vec<Participant>> {
    query_all(
        connection,
        &format!(
            "SELECT {PARTICIPANT_COLUMNS} FROM participant WHERE meetingID = ?1 \
             ORDER BY displayName, id"
        ),
        [DbUuid(meeting_id)],
        participant_from_row,
    )
}

/// The persons among `ids`, by display name, ties by id. Empty `ids` is an
/// empty result without a query.
pub(super) fn persons_with_ids(connection: &Connection, ids: &[Uuid]) -> Result<Vec<Person>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders: Vec<String> = (1..=ids.len()).map(|index| format!("?{index}")).collect();
    let sql = format!(
        "SELECT {PERSON_COLUMNS} FROM person WHERE id IN ({}) ORDER BY displayName, id",
        placeholders.join(", ")
    );
    query_all(
        connection,
        &sql,
        rusqlite::params_from_iter(ids.iter().copied().map(DbUuid)),
        person_from_row,
    )
}

/// The voice of this meeting confirmed as `person_id`, other than
/// `except`: the one a confirmation combines into.
pub(super) fn owner_of(
    connection: &Connection,
    meeting_id: Uuid,
    person_id: Uuid,
    except: Uuid,
) -> Result<Option<Uuid>> {
    Ok(connection
        .query_row(
            "SELECT id FROM speaker WHERE meetingID = ?1 AND personID = ?2 \
             AND assignment = ?3 AND id != ?4 AND mergedInto IS NULL \
             ORDER BY clusterLabel, id LIMIT 1",
            params![
                DbUuid(meeting_id),
                DbUuid(person_id),
                SpeakerAssignmentKind::Confirmed.as_str(),
                DbUuid(except)
            ],
            |row| row.col::<DbUuid>("id"),
        )
        .optional()?)
}

/// Whether a segment carries `speaker_id`, or the id of a speaker combined
/// into it.
fn owns_segments(connection: &Connection, speaker_id: Uuid) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM transcriptSegment WHERE speakerID = ?1 \
         OR speakerID IN (SELECT id FROM speaker WHERE mergedInto = ?1))",
        [DbUuid(speaker_id)],
        |row| row.get(0),
    )?)
}

/// Deletes each of `candidates`, and then each deleted row's former
/// `mergedInto`, that no segment carries and no row is combined into: a
/// voice a paragraph move left empty. Recomputes the voice of every person
/// a deleted row counted for (its own, and its root's for a combined row).
/// `keeping` (the move's destination) and the lane rows, the "me" speaker
/// and the room row, are never deleted. Returns the deleted rows' clip URLs for removal once the transaction
/// committed.
pub(super) fn prune_emptied_speakers(
    connection: &Connection,
    candidates: BTreeSet<Uuid>,
    keeping: Uuid,
) -> Result<Vec<String>> {
    let mut queue: Vec<Uuid> = candidates.into_iter().collect();
    let mut clips = Vec::new();
    let mut person_ids = BTreeSet::new();
    while let Some(id) = queue.pop() {
        let Some(speaker) = fetch_speaker(connection, id)? else {
            continue;
        };
        // The move's destination and the lanes (the "me" speaker, the room
        // row of a run without diarization) stay, segments or not.
        if id == keeping
            || id == me_speaker_id(speaker.meeting_id)
            || id == room_speaker_id(speaker.meeting_id)
        {
            continue;
        }
        let used: bool = connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM transcriptSegment WHERE speakerID = ?1) \
             OR EXISTS (SELECT 1 FROM speaker WHERE mergedInto = ?1)",
            [DbUuid(id)],
            |row| row.get(0),
        )?;
        if used {
            continue;
        }
        let into = merged_into(connection, id)?;
        person_ids.extend(confirmed_person(&speaker));
        if into.is_some()
            && let Some(root) = fetch_speaker(connection, root_of(connection, id)?)?
        {
            person_ids.extend(confirmed_person(&root));
        }
        connection.execute("DELETE FROM speaker WHERE id = ?1", [DbUuid(id)])?;
        clips.extend(speaker.sample_clip_url);
        queue.extend(into);
    }
    for person_id in person_ids {
        refresh_voice(connection, person_id)?;
    }
    Ok(clips)
}

impl Store {
    pub fn persons(&self) -> Result<Vec<Person>> {
        self.read(|connection| {
            query_all(
                connection,
                &format!("SELECT {PERSON_COLUMNS} FROM person ORDER BY displayName, id"),
                [],
                person_from_row,
            )
        })
    }

    pub fn person(&self, id: Uuid) -> Result<Option<Person>> {
        self.read(|connection| {
            Ok(connection
                .query_row(
                    &format!("SELECT {PERSON_COLUMNS} FROM person WHERE id = ?1"),
                    [DbUuid(id)],
                    person_from_row,
                )
                .optional()?)
        })
    }

    pub fn save_person(&self, person: &Person) -> Result<()> {
        self.write(|transaction| save_person(transaction, person))
    }

    pub fn save_participant(&self, participant: &Participant) -> Result<()> {
        self.write(|transaction| save_participant(transaction, participant))
    }

    /// Removes one participant; a missing id is not an error.
    pub fn delete_participant(&self, id: Uuid) -> Result<()> {
        self.write(|transaction| {
            transaction.execute("DELETE FROM participant WHERE id = ?1", [DbUuid(id)])?;
            Ok(())
        })
    }

    /// The meeting's participants by display name.
    pub fn participants(&self, meeting_id: Uuid) -> Result<Vec<Participant>> {
        self.read(|connection| participants_of_meeting(connection, meeting_id))
    }

    /// The meeting's speakers by cluster label.
    pub fn speakers(&self, meeting_id: Uuid) -> Result<Vec<Speaker>> {
        self.read(|connection| speakers_of_meeting(connection, meeting_id))
    }

    /// Inserts or replaces the speaker.
    pub fn save_speaker(&self, speaker: &Speaker) -> Result<()> {
        self.write(|transaction| save_speaker(transaction, speaker))
    }

    /// The speakers of every meeting in `meeting_ids` in one read, keyed by
    /// meeting; a meeting without speakers has no key. Within a meeting the
    /// order is [`Store::speakers`]'.
    pub fn speakers_for_meetings(
        &self,
        meeting_ids: &[Uuid],
    ) -> Result<BTreeMap<Uuid, Vec<Speaker>>> {
        if meeting_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        self.read(|connection| {
            let placeholders = vec!["?"; meeting_ids.len()].join(", ");
            let speakers = query_all(
                connection,
                &format!(
                    "SELECT {SPEAKER_COLUMNS} FROM speaker WHERE meetingID IN ({placeholders}) \
                     AND mergedInto IS NULL ORDER BY meetingID, clusterLabel, id"
                ),
                params_from_iter(meeting_ids.iter().map(|id| DbUuid(*id))),
                speaker_from_row,
            )?;
            let mut grouped: BTreeMap<Uuid, Vec<Speaker>> = BTreeMap::new();
            for speaker in speakers {
                grouped.entry(speaker.meeting_id).or_default().push(speaker);
            }
            Ok(grouped)
        })
    }

    /// People who own a confirmed speaker, most recently met first, then by
    /// name: what the speaker picker lists before anything is typed.
    pub fn recent_persons(&self) -> Result<Vec<Person>> {
        self.read(|connection| {
            query_all(
                connection,
                "SELECT person.id, person.displayName, person.email, person.embedding, \
                 person.sampleCount, person.createdAt FROM person \
                 JOIN speaker ON speaker.personID = person.id AND speaker.assignment = ?1 \
                 AND speaker.dismissed = 0 AND speaker.mergedInto IS NULL \
                 JOIN meeting ON meeting.id = speaker.meetingID \
                 GROUP BY person.id \
                 ORDER BY MAX(meeting.startedAt) DESC, person.displayName COLLATE NOCASE, person.id",
                [SpeakerAssignmentKind::Confirmed.as_str()],
                person_from_row,
            )
        })
    }

    /// The person a typed or calendar name stands for: the stored person
    /// whose name matches ignoring case and diacritics ("jerome" finds
    /// Jérôme), else a new, unsaved person. Nothing is written. A blank
    /// name is [`StoreError::BlankPersonName`].
    pub fn resolve_person(
        &self,
        name: &str,
        email: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Person> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(StoreError::BlankPersonName);
        }
        if let Some(existing) = self
            .persons()?
            .into_iter()
            .find(|person| Person::names_match(&person.display_name, trimmed))
        {
            return Ok(existing);
        }
        Ok(Person {
            id: Uuid::new_v4(),
            display_name: trimmed.to_owned(),
            email: email.map(str::to_owned),
            embedding: None,
            sample_count: 0,
            created_at: now,
        })
    }

    /// The one operation that sets `confirmed`. Saves an unknown person.
    /// When the person already owns another voice of this meeting, the
    /// speaker is combined into it ([`merge_speaker_rows`]): its row is
    /// kept as it is, with `mergedInto` set, so
    /// [`Store::separate_speaker`] undoes it. Otherwise it drops the
    /// speaker's name suggestion and any No to this person, undoes a
    /// dismissal, and the speaker becomes `confirmed(person)`; the same
    /// person again changes nothing else. Naming, renaming or combining
    /// the voice the "me" speaker reads as (the "me" speaker itself, or the
    /// voice it is combined into) renames the meeting's "me" participant
    /// ([`follow_me`]). The voices of the new and the
    /// previous person are recomputed. The clip stays while the meeting's
    /// master exists. Returns the clip URLs to remove once the transaction
    /// committed.
    pub fn confirm_speaker(&self, speaker_id: Uuid, person: &Person) -> Result<Vec<String>> {
        self.write(|transaction| {
            let mut speaker = fetch_speaker(transaction, speaker_id)?
                .ok_or(StoreError::SpeakerNotFound(speaker_id))?;
            if fetch_person(transaction, person.id)?.is_none() {
                save_person(transaction, person)?;
            }
            let confirmed_as_person = speaker.assignment
                == (SpeakerAssignment::Confirmed {
                    person_id: person.id,
                });
            let owner = if confirmed_as_person {
                None
            } else {
                owner_of(transaction, speaker.meeting_id, person.id, speaker_id)?
            };
            // Naming the voice the "me" speaker reads as answers the Noes
            // Remove name recorded on the "me" speaker.
            if me_root(transaction, speaker.meeting_id)? == speaker_id {
                transaction.execute(
                    "DELETE FROM speakerSuggestionRejection WHERE speakerID = ?1",
                    [DbUuid(me_speaker_id(speaker.meeting_id))],
                )?;
            }
            if let Some(owner) = owner {
                let mut person_ids =
                    merge_speaker_rows(transaction, speaker_id, owner, speaker.meeting_id)?;
                person_ids.insert(person.id);
                follow_me(transaction, speaker.meeting_id, speaker_id)?;
                for person_id in person_ids {
                    refresh_voice(transaction, person_id)?;
                }
                return Ok(Vec::new());
            }
            transaction.execute(
                "DELETE FROM speakerNameSuggestion WHERE speakerID = ?1",
                [DbUuid(speaker_id)],
            )?;
            // Naming a voice answers an earlier No to this person and makes
            // a dismissed voice a person again.
            transaction.execute(
                "DELETE FROM speakerSuggestionRejection WHERE speakerID = ?1 AND personID = ?2",
                params![DbUuid(speaker_id), DbUuid(person.id)],
            )?;
            if speaker.dismissed {
                speaker.dismissed = false;
                save_speaker(transaction, &speaker)?;
            }
            if confirmed_as_person {
                return Ok(Vec::new());
            }
            let mut person_ids = BTreeSet::from([person.id]);
            person_ids.extend(confirmed_person(&speaker));
            let stored = fetch_person(transaction, person.id)?.unwrap_or_else(|| person.clone());
            let similarity = answer_similarity(&speaker, &stored);
            speaker.assignment = SpeakerAssignment::Confirmed {
                person_id: person.id,
            };
            save_speaker(transaction, &speaker)?;
            set_confirmed_similarity(transaction, speaker_id, similarity)?;
            let clips: Vec<String> = drop_clip_when_audio_is_gone(transaction, speaker_id)?
                .into_iter()
                .collect();
            follow_me(transaction, speaker.meeting_id, speaker_id)?;
            for person_id in person_ids {
                refresh_voice(transaction, person_id)?;
            }
            Ok(clips)
        })
    }

    /// The meeting's voices combined into others, by cluster label: each
    /// with the voice it reads as.
    pub fn merged_speakers(&self, meeting_id: Uuid) -> Result<Vec<MergedSpeaker>> {
        self.read(|connection| merged_speakers_of_meeting(connection, meeting_id))
    }

    /// Undo a combine, or "Separate again": the combined speaker is its own
    /// voice again, exactly as it was before (its row was kept), with the
    /// paragraphs that still carry it; a paragraph moved away since stays
    /// where it was moved. The voices of the person it read as and of its
    /// own person are recomputed, and for the "me" speaker the meeting's
    /// "me" participant follows its own assignment again. Only this row
    /// separates: rows that were combined into it before it was combined
    /// itself were re-pointed at the root then ([`merge_speaker_rows`]),
    /// stay with the root, and each separates on its own. Refused with
    /// [`StoreError::NotCombined`] when it is not combined, and with
    /// [`StoreError::StaleTranscript`] when nothing of it is left.
    pub fn separate_speaker(&self, speaker_id: Uuid) -> Result<()> {
        self.write(|transaction| {
            let speaker = fetch_speaker(transaction, speaker_id)?
                .ok_or(StoreError::NotCombined(speaker_id))?;
            if merged_into(transaction, speaker_id)?.is_none() {
                return Err(StoreError::NotCombined(speaker_id));
            }
            if !owns_segments(transaction, speaker_id)? {
                return Err(StoreError::StaleTranscript);
            }
            let root = root_of(transaction, speaker_id)?;
            let mut person_ids: BTreeSet<Uuid> = confirmed_person(&speaker).into_iter().collect();
            if let Some(root_speaker) = fetch_speaker(transaction, root)? {
                person_ids.extend(confirmed_person(&root_speaker));
            }
            transaction.execute(
                "UPDATE speaker SET mergedInto = NULL WHERE id = ?1",
                [DbUuid(speaker_id)],
            )?;
            follow_me(transaction, speaker.meeting_id, speaker_id)?;
            for person_id in person_ids {
                refresh_voice(transaction, person_id)?;
            }
            Ok(())
        })
    }

    /// Confirms as `person` every meeting's still-unknown "me" speaker and
    /// names its "me" participant; rows only, nothing is exported. A meeting
    /// where `person` owns another speaker is skipped: that would be a
    /// merge, which only the user decides. So is one where the user removed
    /// `person`'s name from the "me" speaker ([`Store::clear_speaker`]).
    pub fn confirm_unnamed_me_speakers(&self, person: &Person) -> Result<()> {
        self.write(|transaction| {
            if fetch_person(transaction, person.id)?.is_none() {
                save_person(transaction, person)?;
            }
            let unknown = query_all(
                transaction,
                &format!(
                    "SELECT {SPEAKER_COLUMNS} FROM speaker WHERE assignment = ?1 AND dismissed = 0 \
                     AND mergedInto IS NULL AND NOT EXISTS (SELECT 1 FROM speaker AS owner \
                     WHERE owner.meetingID = speaker.meetingID AND owner.personID = ?2 \
                     AND owner.assignment = ?3 AND owner.mergedInto IS NULL) \
                     AND NOT EXISTS (SELECT 1 FROM speakerSuggestionRejection AS cleared \
                     WHERE cleared.speakerID = speaker.id AND cleared.personID = ?2) \
                     ORDER BY meetingID"
                ),
                params![
                    SpeakerAssignmentKind::Unknown.as_str(),
                    DbUuid(person.id),
                    SpeakerAssignmentKind::Confirmed.as_str()
                ],
                speaker_from_row,
            )?;
            for mut speaker in unknown
                .into_iter()
                .filter(|speaker| speaker.id == me_speaker_id(speaker.meeting_id))
            {
                transaction.execute(
                    "DELETE FROM speakerNameSuggestion WHERE speakerID = ?1",
                    [DbUuid(speaker.id)],
                )?;
                speaker.assignment = SpeakerAssignment::Confirmed {
                    person_id: person.id,
                };
                save_speaker(transaction, &speaker)?;
                name_me_participant(transaction, speaker.meeting_id, person)?;
            }
            Ok(())
        })
    }

    /// The model's guess who each speaker is, at most one per speaker, in
    /// speaker id order.
    pub fn name_suggestions(&self, meeting_id: Uuid) -> Result<Vec<SpeakerNameSuggestion>> {
        self.read(|connection| {
            // A combined voice keeps its suggestion for Separate again, but
            // reads as the voice it joined meanwhile.
            let roots: BTreeSet<Uuid> = speakers_of_meeting(connection, meeting_id)?
                .into_iter()
                .map(|speaker| speaker.id)
                .collect();
            Ok(name_suggestions_of_meeting(connection, meeting_id)?
                .into_iter()
                .filter(|suggestion| roots.contains(&suggestion.speaker_id))
                .collect())
        })
    }

    /// "Remove name", beside [`Store::confirm_speaker`]: the speaker goes
    /// back to `unknown` in this meeting. The person it was confirmed to
    /// keeps their name and has their voice recomputed without it; for the
    /// "me" speaker the meeting's "me" participant is unnamed again, while
    /// who the user is (`Settings::self_person_id`) stays. A suggestion is
    /// dropped the same way, without a No. Clearing the voice the "me"
    /// speaker reads as (the "me" speaker, or the voice it is combined
    /// into) is recorded on the "me" speaker as a No to the person it named
    /// and to the user's own person, so processing again leaves the mic
    /// lane unnamed rather than name it after the user
    /// (`ensure_me_participant` in `steno-pipeline`,
    /// [`Store::confirm_unnamed_me_speakers`]); naming that voice again
    /// answers those Noes.
    pub fn clear_speaker(&self, speaker_id: Uuid) -> Result<()> {
        self.write(|transaction| {
            let previous = fetch_speaker(transaction, speaker_id)?
                .ok_or(StoreError::SpeakerNotFound(speaker_id))?
                .assignment;
            let speaker = unassign_speaker(transaction, speaker_id)?;
            let meeting_id = speaker.meeting_id;
            if let SpeakerAssignment::Confirmed { person_id } = previous
                && me_root(transaction, meeting_id)? == speaker_id
            {
                let user = settings::self_person_id(transaction);
                for person_id in std::iter::once(person_id).chain(user) {
                    insert_rejection(
                        transaction,
                        meeting_id,
                        &Rejection {
                            speaker_id: me_speaker_id(meeting_id),
                            person_id,
                            similarity: None,
                        },
                    )?;
                }
            }
            Ok(())
        })
    }

    /// No to a suggestion: `person_id` is recorded as not this voice and is
    /// not suggested for it again, here or when the meeting is processed
    /// again. The voice match's score is kept with the No: the suggested
    /// similarity, else the cosine of the voice and the person's stored
    /// voice. A speaker still suggested as that person goes back to
    /// `unknown`. Voice memory is not touched: nobody's voice learns from a
    /// No. A speaker confirmed as `person_id` is
    /// [`StoreError::RejectedConfirmedPerson`]; that is
    /// [`Store::clear_speaker`].
    pub fn reject_suggestion(&self, speaker_id: Uuid, person_id: Uuid) -> Result<()> {
        self.write(|transaction| {
            let mut speaker = fetch_speaker(transaction, speaker_id)?
                .ok_or(StoreError::SpeakerNotFound(speaker_id))?;
            let person = fetch_person(transaction, person_id)?
                .ok_or(StoreError::PersonNotFound(person_id))?;
            if speaker.assignment == (SpeakerAssignment::Confirmed { person_id }) {
                return Err(StoreError::RejectedConfirmedPerson(speaker_id, person_id));
            }
            let suggested = matches!(
                speaker.assignment,
                SpeakerAssignment::Suggested { person_id: suggested, .. } if suggested == person_id
            );
            let similarity = answer_similarity(&speaker, &person);
            insert_rejection(
                transaction,
                speaker.meeting_id,
                &Rejection {
                    speaker_id,
                    person_id,
                    similarity,
                },
            )?;
            if suggested {
                speaker.assignment = SpeakerAssignment::Unknown;
                save_speaker(transaction, &speaker)?;
            }
            Ok(())
        })
    }

    /// The persons the user answered No to, per speaker of the meeting.
    pub fn suggestion_rejections(
        &self,
        meeting_id: Uuid,
    ) -> Result<BTreeMap<Uuid, BTreeSet<Uuid>>> {
        self.read(|connection| {
            let mut rejections: BTreeMap<Uuid, BTreeSet<Uuid>> = BTreeMap::new();
            for rejection in suggestion_rejections_of_meeting(connection, meeting_id)? {
                rejections
                    .entry(rejection.speaker_id)
                    .or_default()
                    .insert(rejection.person_id);
            }
            Ok(rejections)
        })
    }

    /// "Not a person", or a person again. Dismissing also takes back a
    /// suggestion or confirmation as [`Store::clear_speaker`] does, so the
    /// voice leaves voice memory; its segments keep their text and their
    /// speaker. Restoring leaves the voice unnamed. The "me" speaker, the
    /// user's own mic, is [`StoreError::MeSpeakerNotDismissable`].
    pub fn set_speaker_dismissed(&self, speaker_id: Uuid, dismissed: bool) -> Result<()> {
        self.write(|transaction| {
            if dismissed
                && let Some(speaker) = fetch_speaker(transaction, speaker_id)?
                && speaker_id == me_speaker_id(speaker.meeting_id)
            {
                return Err(StoreError::MeSpeakerNotDismissable(speaker_id));
            }
            let mut speaker = if dismissed {
                unassign_speaker(transaction, speaker_id)?
            } else {
                fetch_speaker(transaction, speaker_id)?
                    .ok_or(StoreError::SpeakerNotFound(speaker_id))?
            };
            if speaker.dismissed != dismissed {
                speaker.dismissed = dismissed;
                save_speaker(transaction, &speaker)?;
            }
            Ok(())
        })
    }
}

impl Store {
    /// Every `sampleClipURL` a speaker row names, of every meeting: what a
    /// clip folder sweep keeps.
    pub fn sample_clip_urls(&self) -> Result<Vec<String>> {
        self.read(sample_clip_urls)
    }

    /// Clears `sampleClipURL` on the listed speakers once their clips are
    /// gone.
    pub fn clear_sample_clips(&self, meeting_id: Uuid, speaker_ids: &[Uuid]) -> Result<()> {
        self.write(|transaction| clear_sample_clips(transaction, meeting_id, speaker_ids))
    }
}

pub(super) fn sample_clip_urls(connection: &Connection) -> Result<Vec<String>> {
    query_all(
        connection,
        "SELECT sampleClipURL FROM speaker WHERE sampleClipURL IS NOT NULL",
        [],
        |row| row.get(0),
    )
}

pub(super) fn clear_sample_clips(
    connection: &Connection,
    meeting_id: Uuid,
    speaker_ids: &[Uuid],
) -> Result<()> {
    for id in speaker_ids {
        connection.execute(
            "UPDATE speaker SET sampleClipURL = NULL WHERE id = ?1 AND meetingID = ?2",
            params![DbUuid(*id), DbUuid(meeting_id)],
        )?;
    }
    Ok(())
}
