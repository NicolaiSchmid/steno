//! `person`, `participant`, `speaker` and `speakerNameSuggestion` rows.
//! Swift: `Sources/StenoCore/Storage/MeetingStore+People.swift`.

use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use super::convert::{DbDate, DbEmbedding, DbEnum, DbUuid, RowExt as _};
use super::{Result, Store, execute_cached, insert_sql, query_all, upsert_sql};
use crate::model::{
    Participant, Person, Speaker, SpeakerAssignment, SpeakerNameSuggestion, TimeRange,
};

// Persons

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

// Participants

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

// Speakers

const SPEAKER_COLUMNS: &str = "id, meetingID, clusterLabel, assignment, personID, similarity, \
     embedding, sampleClipStart, sampleClipEnd, sampleClipURL, clusterConfidence";

fn speaker_from_row(row: &Row<'_>) -> rusqlite::Result<Speaker> {
    let kind = row.col::<DbEnum<_>>("assignment")?;
    let person_id = row.col::<Option<DbUuid>>("personID")?;
    let similarity: Option<f64> = row.get("similarity")?;
    let clip_start: Option<f64> = row.get("sampleClipStart")?;
    let clip_end: Option<f64> = row.get("sampleClipEnd")?;
    let confidence: f64 = row.get("clusterConfidence")?;
    // Float columns hold the Swift `Float` widened to `Double`; narrowing it
    // back is exact for every value Swift wrote.
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
        ],
    )
}

pub(super) fn insert_speaker(connection: &Connection, speaker: &Speaker) -> Result<()> {
    write_speaker(connection, speaker, &insert_sql("speaker", SPEAKER_COLUMNS))
}

pub(super) fn save_speaker(connection: &Connection, speaker: &Speaker) -> Result<()> {
    write_speaker(connection, speaker, &upsert_sql("speaker", SPEAKER_COLUMNS))
}

pub(super) fn speakers_of_meeting(
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

// Speaker name suggestions

const SUGGESTION_COLUMNS: &str = "speakerID, meetingID, name, confidence, evidence";

/// Replaces the meeting's suggestions with those of `suggestions` that carry
/// a name and point at one of the meeting's speakers; ascending by
/// confidence, so a duplicate speaker ends with its strongest one.
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
    let keys: Vec<DbUuid> = ids.iter().copied().map(DbUuid).collect();
    query_all(
        connection,
        &sql,
        rusqlite::params_from_iter(keys.iter()),
        person_from_row,
    )
}

impl Store {
    /// Every known person, by display name.
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

    /// The person with `id`.
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

    /// Inserts or replaces the person (GRDB's `save`).
    pub fn save_person(&self, person: &Person) -> Result<()> {
        self.write(|transaction| save_person(transaction, person))
    }

    /// Inserts or replaces the participant (GRDB's `save`).
    pub fn save_participant(&self, participant: &Participant) -> Result<()> {
        self.write(|transaction| save_participant(transaction, participant))
    }

    /// The meeting's participants by display name.
    pub fn participants(&self, meeting_id: Uuid) -> Result<Vec<Participant>> {
        self.read(|connection| participants_of_meeting(connection, meeting_id))
    }

    /// The meeting's speakers by cluster label.
    pub fn speakers(&self, meeting_id: Uuid) -> Result<Vec<Speaker>> {
        self.read(|connection| speakers_of_meeting(connection, meeting_id))
    }

    /// Inserts or replaces the speaker (GRDB's `save`): the user's
    /// confirmation of who a cluster is.
    pub fn save_speaker(&self, speaker: &Speaker) -> Result<()> {
        self.write(|transaction| save_speaker(transaction, speaker))
    }

    /// The model's guess who each speaker is, at most one per speaker, in
    /// speaker id order.
    pub fn name_suggestions(&self, meeting_id: Uuid) -> Result<Vec<SpeakerNameSuggestion>> {
        self.read(|connection| {
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
        })
    }
}
