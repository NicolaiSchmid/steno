use rusqlite::types::ToSql;
use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use super::convert::{DbDate, DbEnum, DbUuid, Unwrap as _};
use super::{Result, Store};
use crate::model::{
    Embedding, Participant, Person, Speaker, SpeakerAssignment, SpeakerNameSuggestion, TimeRange,
};

fn embedding_of(row: &Row<'_>, column: &str) -> rusqlite::Result<Option<Embedding>> {
    let bytes: Option<Vec<u8>> = row.get(column)?;
    Ok(bytes.as_deref().and_then(Embedding::from_bytes))
}

// MARK: persons

const PERSON_COLUMNS: &str = "id, displayName, email, embedding, sampleCount, createdAt";

fn person_from_row(row: &Row<'_>) -> rusqlite::Result<Person> {
    Ok(Person {
        id: row.get::<_, DbUuid>("id")?.0,
        display_name: row.get("displayName")?,
        email: row.get("email")?,
        embedding: embedding_of(row, "embedding")?,
        sample_count: row.get("sampleCount")?,
        created_at: row.get::<_, DbDate>("createdAt")?.0,
    })
}

pub(super) fn save_person(connection: &Connection, person: &Person) -> Result<()> {
    connection.execute(
        "INSERT INTO person (id, displayName, email, embedding, sampleCount, createdAt) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(id) DO UPDATE SET displayName = excluded.displayName, email = excluded.email, \
         embedding = excluded.embedding, sampleCount = excluded.sampleCount, \
         createdAt = excluded.createdAt",
        params![
            DbUuid(person.id),
            person.display_name,
            person.email,
            person.embedding.as_ref().map(Embedding::to_bytes),
            person.sample_count,
            DbDate(person.created_at),
        ],
    )?;
    Ok(())
}

// MARK: participants

const PARTICIPANT_COLUMNS: &str = "id, meetingID, personID, displayName, role, email";

fn participant_from_row(row: &Row<'_>) -> rusqlite::Result<Participant> {
    Ok(Participant {
        id: row.get::<_, DbUuid>("id")?.0,
        meeting_id: row.get::<_, DbUuid>("meetingID")?.0,
        person_id: row.get::<_, Option<DbUuid>>("personID")?.unwrap_db(),
        display_name: row.get("displayName")?,
        role: row.get::<_, DbEnum<_>>("role")?.0,
        email: row.get("email")?,
    })
}

pub(super) fn save_participant(connection: &Connection, participant: &Participant) -> Result<()> {
    connection.execute(
        "INSERT INTO participant (id, meetingID, personID, displayName, role, email) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(id) DO UPDATE SET meetingID = excluded.meetingID, personID = excluded.personID, \
         displayName = excluded.displayName, role = excluded.role, email = excluded.email",
        params![
            DbUuid(participant.id),
            DbUuid(participant.meeting_id),
            participant.person_id.map(DbUuid),
            participant.display_name,
            DbEnum(participant.role),
            participant.email,
        ],
    )?;
    Ok(())
}

// MARK: speakers

const SPEAKER_COLUMNS: &str = "id, meetingID, clusterLabel, assignment, personID, similarity, \
     embedding, sampleClipStart, sampleClipEnd, sampleClipURL, clusterConfidence";

fn speaker_from_row(row: &Row<'_>) -> rusqlite::Result<Speaker> {
    let kind: DbEnum<_> = row.get("assignment")?;
    let person_id = row.get::<_, Option<DbUuid>>("personID")?.unwrap_db();
    let similarity: Option<f64> = row.get("similarity")?;
    let clip_start: Option<f64> = row.get("sampleClipStart")?;
    let clip_end: Option<f64> = row.get("sampleClipEnd")?;
    let confidence: f64 = row.get("clusterConfidence")?;
    // Float columns hold the Swift `Float` widened to `Double`; narrowing it
    // back is exact for every value Swift wrote.
    #[allow(clippy::cast_possible_truncation)]
    let narrow = |value: f64| value as f32;
    Ok(Speaker {
        id: row.get::<_, DbUuid>("id")?.0,
        meeting_id: row.get::<_, DbUuid>("meetingID")?.0,
        cluster_label: row.get("clusterLabel")?,
        assignment: SpeakerAssignment::from_columns(kind.0, person_id, similarity.map(narrow)),
        embedding: embedding_of(row, "embedding")?,
        sample_clip_range: match (clip_start, clip_end) {
            (Some(lower), Some(upper)) if lower <= upper => Some(TimeRange { lower, upper }),
            _ => None,
        },
        sample_clip_url: row.get("sampleClipURL")?,
        cluster_confidence: narrow(confidence),
    })
}

fn speaker_params(speaker: &Speaker) -> [Box<dyn ToSql + '_>; 11] {
    [
        Box::new(DbUuid(speaker.id)),
        Box::new(DbUuid(speaker.meeting_id)),
        Box::new(&speaker.cluster_label),
        Box::new(DbEnum(speaker.assignment.kind())),
        Box::new(speaker.assignment.person_id().map(DbUuid)),
        Box::new(speaker.assignment.similarity().map(f64::from)),
        Box::new(speaker.embedding.as_ref().map(Embedding::to_bytes)),
        Box::new(speaker.sample_clip_range.map(|range| range.lower)),
        Box::new(speaker.sample_clip_range.map(|range| range.upper)),
        Box::new(&speaker.sample_clip_url),
        Box::new(f64::from(speaker.cluster_confidence)),
    ]
}

pub(super) fn insert_speaker(connection: &Connection, speaker: &Speaker) -> Result<()> {
    connection.execute(
        &format!(
            "INSERT INTO speaker ({SPEAKER_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
        ),
        speaker_params(speaker),
    )?;
    Ok(())
}

pub(super) fn save_speaker(connection: &Connection, speaker: &Speaker) -> Result<()> {
    connection.execute(
        &format!(
            "INSERT INTO speaker ({SPEAKER_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
             ON CONFLICT(id) DO UPDATE SET meetingID = excluded.meetingID, \
             clusterLabel = excluded.clusterLabel, assignment = excluded.assignment, \
             personID = excluded.personID, similarity = excluded.similarity, \
             embedding = excluded.embedding, sampleClipStart = excluded.sampleClipStart, \
             sampleClipEnd = excluded.sampleClipEnd, sampleClipURL = excluded.sampleClipURL, \
             clusterConfidence = excluded.clusterConfidence"
        ),
        speaker_params(speaker),
    )?;
    Ok(())
}

pub(super) fn speakers(connection: &Connection, meeting_id: Uuid) -> Result<Vec<Speaker>> {
    let mut statement = connection.prepare(&format!(
        "SELECT {SPEAKER_COLUMNS} FROM speaker WHERE meetingID = ?1 ORDER BY clusterLabel, id"
    ))?;
    let rows = statement.query_map([DbUuid(meeting_id)], speaker_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// MARK: speaker name suggestions

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
    let speaker_ids: Vec<Uuid> = speakers(connection, meeting_id)?
        .into_iter()
        .map(|speaker| speaker.id)
        .collect();
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
        connection.execute(
            "INSERT INTO speakerNameSuggestion (speakerID, meetingID, name, confidence, evidence) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(speakerID) DO UPDATE SET meetingID = excluded.meetingID, \
             name = excluded.name, confidence = excluded.confidence, evidence = excluded.evidence",
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

impl Store {
    /// Every known person, by display name.
    pub fn persons(&self) -> Result<Vec<Person>> {
        self.read(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {PERSON_COLUMNS} FROM person ORDER BY displayName, id"
            ))?;
            let rows = statement.query_map([], person_from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
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

    /// The meeting's participants by display name.
    pub fn participants(&self, meeting_id: Uuid) -> Result<Vec<Participant>> {
        self.read(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {PARTICIPANT_COLUMNS} FROM participant WHERE meetingID = ?1 \
                 ORDER BY displayName, id"
            ))?;
            let rows = statement.query_map([DbUuid(meeting_id)], participant_from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    /// The meeting's speakers by cluster label.
    pub fn speakers(&self, meeting_id: Uuid) -> Result<Vec<Speaker>> {
        self.read(|connection| speakers(connection, meeting_id))
    }

    pub fn save_speaker(&self, speaker: &Speaker) -> Result<()> {
        self.write(|transaction| save_speaker(transaction, speaker))
    }

    /// The model's guess who each speaker is, at most one per speaker, in
    /// speaker id order.
    pub fn name_suggestions(&self, meeting_id: Uuid) -> Result<Vec<SpeakerNameSuggestion>> {
        self.read(|connection| {
            let mut statement = connection.prepare(
                "SELECT speakerID, name, confidence, evidence FROM speakerNameSuggestion \
                 WHERE meetingID = ?1 ORDER BY speakerID",
            )?;
            let rows = statement.query_map([DbUuid(meeting_id)], |row| {
                Ok(SpeakerNameSuggestion {
                    speaker_id: row.get::<_, DbUuid>("speakerID")?.0,
                    name: Some(row.get("name")?),
                    confidence: row.get("confidence")?,
                    evidence: row.get("evidence")?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
}
