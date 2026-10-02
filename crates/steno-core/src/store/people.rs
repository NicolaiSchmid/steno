//! `person`, `participant`, `speaker` and `speakerNameSuggestion` rows.
//! Swift: `Sources/StenoCore/Storage/MeetingStore+People.swift`.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use uuid::Uuid;

use super::convert::{DbDate, DbEmbedding, DbEnum, DbUuid, RowExt as _};
use super::{Result, Store, StoreError, assets, execute_cached, insert_sql, query_all, upsert_sql};
use crate::model::{
    Embedding, Participant, Person, Speaker, SpeakerAssignment, SpeakerAssignmentKind,
    SpeakerNameSuggestion, TimeRange,
};
use crate::paths::path_from_file_url;

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

fn fetch_person(connection: &Connection, id: Uuid) -> Result<Option<Person>> {
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

fn fetch_speaker(connection: &Connection, id: Uuid) -> Result<Option<Speaker>> {
    Ok(connection
        .query_row(
            &format!("SELECT {SPEAKER_COLUMNS} FROM speaker WHERE id = ?1"),
            [DbUuid(id)],
            speaker_from_row,
        )
        .optional()?)
}

/// What a merge leaves for the caller: the clip file to remove once the
/// transaction commits (`None` when it moved to the target) and the persons
/// whose voices the merge touched. Swift: `MeetingStore.SpeakerMerge`.
struct SpeakerMerge {
    clip_to_remove: Option<String>,
    person_ids: BTreeSet<Uuid>,
}

/// Two clusters inside one meeting: moves `source`'s segments to `target`,
/// averages the embeddings, keeps `target`'s assignment (or takes
/// `source`'s when `target` is unknown), deletes the `source` speaker.
/// `source`'s sample clip moves to `target` when `target` has none and is
/// returned for removal otherwise. Voices are not refreshed here.
/// Swift: `MeetingStore.mergeSpeakerRows`.
fn merge_speaker_rows(
    connection: &Connection,
    source: Uuid,
    target: Uuid,
    meeting_id: Uuid,
) -> Result<SpeakerMerge> {
    let mut merge = SpeakerMerge {
        clip_to_remove: None,
        person_ids: BTreeSet::new(),
    };
    if source == target {
        return Ok(merge);
    }
    let merged = fetch_speaker(connection, source)?.ok_or(StoreError::SpeakerNotFound(source))?;
    let mut kept = fetch_speaker(connection, target)?.ok_or(StoreError::SpeakerNotFound(target))?;
    if merged.meeting_id != meeting_id || kept.meeting_id != meeting_id {
        return Err(StoreError::SpeakersInDifferentMeetings(source, target));
    }
    if let SpeakerAssignment::Confirmed { person_id } = merged.assignment {
        merge.person_ids.insert(person_id);
    }
    kept.embedding = match (&kept.embedding, &merged.embedding) {
        (Some(lhs), Some(rhs)) => Some(Embedding::weighted_mean(lhs, 1.0, rhs, 1.0)),
        (None, Some(rhs)) => Some(rhs.clone()),
        (lhs, None) => lhs.clone(),
    };
    if kept.assignment == SpeakerAssignment::Unknown {
        kept.assignment = merged.assignment.clone();
    }
    if let SpeakerAssignment::Confirmed { person_id } = kept.assignment {
        merge.person_ids.insert(person_id);
    }
    merge.clip_to_remove.clone_from(&merged.sample_clip_url);
    if kept.sample_clip_range.is_none() {
        kept.sample_clip_range = merged.sample_clip_range;
        kept.sample_clip_url = merged.sample_clip_url;
        merge.clip_to_remove = None;
    }
    kept.cluster_confidence = kept.cluster_confidence.max(merged.cluster_confidence);
    save_speaker(connection, &kept)?;
    connection.execute(
        "UPDATE transcriptSegment SET speakerID = ?1 WHERE speakerID = ?2",
        params![DbUuid(target), DbUuid(source)],
    )?;
    connection.execute("DELETE FROM speaker WHERE id = ?1", [DbUuid(source)])?;
    Ok(merge)
}

/// Nulls a confirmed speaker's `sampleClipURL` and returns it for removal
/// when the meeting's master recording no longer exists (or the meeting
/// never had an asset); `None` otherwise.
/// Swift: `MeetingStore.dropClipWhenAudioIsGone`.
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
        .and_then(|asset| path_from_file_url(&asset.url))
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

/// Recomputes a person's voice from the speakers confirmed to them: the
/// normalised mean of the newest `VOICE_WINDOW` embeddings by
/// `meeting.startedAt`, `sampleCount` their number; none and 0 without
/// any. A missing person is ignored. Swift: `MeetingStore.refreshVoice`.
fn refresh_voice(connection: &Connection, person_id: Uuid) -> Result<()> {
    let Some(mut person) = fetch_person(connection, person_id)? else {
        return Ok(());
    };
    let embeddings: Vec<Embedding> = query_all(
        connection,
        "SELECT speaker.embedding AS embedding FROM speaker \
         JOIN meeting ON meeting.id = speaker.meetingID \
         WHERE speaker.personID = ?1 AND speaker.assignment = ?2 AND speaker.embedding IS NOT NULL \
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
    query_all(
        connection,
        &sql,
        rusqlite::params_from_iter(ids.iter().copied().map(DbUuid)),
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

    /// The speakers of every meeting in `meeting_ids`, keyed by meeting, in
    /// one read; a meeting without speakers has no key. Within a meeting
    /// the order is [`Store::speakers`]'. The meeting list reads its speaker
    /// chips through this once per list update instead of once per row.
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
                     ORDER BY meetingID, clusterLabel, id"
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

    /// People who own a confirmed speaker, most recently met first (the
    /// latest `meeting.startedAt` among their confirmed speakers), then by
    /// name. What the speaker picker lists before anything is typed.
    pub fn recent_persons(&self) -> Result<Vec<Person>> {
        self.read(|connection| {
            query_all(
                connection,
                "SELECT person.id, person.displayName, person.email, person.embedding, \
                 person.sampleCount, person.createdAt FROM person \
                 JOIN speaker ON speaker.personID = person.id AND speaker.assignment = ?1 \
                 JOIN meeting ON meeting.id = speaker.meetingID \
                 GROUP BY person.id \
                 ORDER BY MAX(meeting.startedAt) DESC, person.displayName COLLATE NOCASE, person.id",
                [SpeakerAssignmentKind::Confirmed.as_str()],
                person_from_row,
            )
        })
    }

    /// The person a typed or calendar name stands for: the stored person
    /// whose display name matches ignoring case and diacritics ("jerome"
    /// finds Jérôme), else a new, unsaved person with the trimmed name and
    /// `email`. Nothing is written; [`Store::confirm_speaker`] saves a new
    /// person. A blank name is [`StoreError::BlankPersonName`].
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

    /// The one operation that sets `confirmed`. Saves the person when the
    /// store does not know them and drops the speaker's name suggestion,
    /// which the confirmation answered. Then: the same person again changes
    /// nothing else; a person who already owns another speaker of this
    /// meeting takes this speaker over (a merge); otherwise the speaker
    /// becomes `confirmed(person)`. The voices of the new person and of
    /// whoever the speaker was confirmed to before are recomputed. The
    /// sample clip stays while the meeting's master recording exists; once
    /// the audio is gone the clip goes with the confirmation. Returns the
    /// clip file URLs the caller removes once the transaction has committed.
    /// Swift: `MeetingStore.confirm(speakerID:person:)`.
    pub fn confirm_speaker(&self, speaker_id: Uuid, person: &Person) -> Result<Vec<String>> {
        self.write(|transaction| {
            let mut speaker = fetch_speaker(transaction, speaker_id)?
                .ok_or(StoreError::SpeakerNotFound(speaker_id))?;
            transaction.execute(
                "DELETE FROM speakerNameSuggestion WHERE speakerID = ?1",
                [DbUuid(speaker_id)],
            )?;
            if fetch_person(transaction, person.id)?.is_none() {
                save_person(transaction, person)?;
            }
            if speaker.assignment
                == (SpeakerAssignment::Confirmed {
                    person_id: person.id,
                })
            {
                return Ok(Vec::new());
            }
            let mut person_ids = BTreeSet::from([person.id]);
            if let SpeakerAssignment::Confirmed { person_id } = speaker.assignment {
                person_ids.insert(person_id);
            }
            let mut clips = Vec::new();
            let owner: Option<Uuid> = transaction
                .query_row(
                    "SELECT id FROM speaker WHERE meetingID = ?1 AND personID = ?2 \
                     AND assignment = ?3 AND id != ?4 ORDER BY clusterLabel, id LIMIT 1",
                    params![
                        DbUuid(speaker.meeting_id),
                        DbUuid(person.id),
                        SpeakerAssignmentKind::Confirmed.as_str(),
                        DbUuid(speaker_id)
                    ],
                    |row| row.col::<DbUuid>("id"),
                )
                .optional()?;
            let kept = if let Some(owner) = owner {
                let merge = merge_speaker_rows(transaction, speaker_id, owner, speaker.meeting_id)?;
                clips.extend(merge.clip_to_remove);
                person_ids.extend(merge.person_ids);
                owner
            } else {
                speaker.assignment = SpeakerAssignment::Confirmed {
                    person_id: person.id,
                };
                save_speaker(transaction, &speaker)?;
                speaker_id
            };
            clips.extend(drop_clip_when_audio_is_gone(transaction, kept)?);
            for person_id in person_ids {
                refresh_voice(transaction, person_id)?;
            }
            Ok(clips)
        })
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
