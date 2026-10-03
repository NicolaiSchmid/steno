//! Participants, persons, speakers and their embeddings.
//! Swift: `Sources/StenoCore/Model/People.swift`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::json::{
    self,
    case_coding::{self, Case},
};
use crate::string_enum;

string_enum! {
    /// Whether a participant is the user or somebody else.
    pub enum ParticipantRole {
        Me = "me",
        Them = "them",
    }
}

/// Somebody in the meeting. Calendar attendees are participants with
/// `role == them` written at recording start.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Participant {
    #[serde(with = "json::uuid_text")]
    pub id: Uuid,
    #[serde(rename = "meetingID", with = "json::uuid_text")]
    pub meeting_id: Uuid,
    #[serde(
        rename = "personID",
        default,
        with = "json::uuid_text_opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub person_id: Option<Uuid>,
    pub display_name: String,
    pub role: ParticipantRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// A known voice across meetings. `embedding` is persisted as a BLOB and
/// never written to JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    #[serde(with = "json::uuid_text")]
    pub id: Uuid,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip)]
    pub embedding: Option<Embedding>,
    pub sample_count: i64,
    #[serde(with = "json::iso_time")]
    pub created_at: DateTime<Utc>,
}

/// A speaker embedding: [`Embedding::DIMENSION`] `f32` values, L2-normalised
/// when produced by the diarizer. Stored as little-endian `f32` bytes and
/// never written to JSON, so it has no serde form.
#[derive(Debug, Clone, PartialEq)]
pub struct Embedding(pub Vec<f32>);

impl Embedding {
    /// `WeSpeaker` embeddings from the diarizer.
    pub const DIMENSION: usize = 256;

    /// Little-endian `f32` bytes, the BLOB column format.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        self.0
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }

    /// Reads little-endian `f32` bytes; `None` when the length is not a
    /// multiple of four.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let (chunks, rest) = bytes.as_chunks::<4>();
        rest.is_empty()
            .then(|| Embedding(chunks.iter().copied().map(f32::from_le_bytes).collect()))
    }

    #[must_use]
    pub fn magnitude(&self) -> f32 {
        self.0.iter().map(|value| value * value).sum::<f32>().sqrt()
    }

    /// The same direction with unit length; the zero vector stays zero.
    #[must_use]
    pub fn normalized(&self) -> Embedding {
        let magnitude = self.magnitude();
        if magnitude > 0.0 {
            Embedding(self.0.iter().map(|value| value / magnitude).collect())
        } else {
            self.clone()
        }
    }

    /// The dot product over the shared prefix of the two vectors.
    #[must_use]
    pub fn dot(&self, other: &Embedding) -> f32 {
        self.0
            .iter()
            .zip(&other.0)
            .map(|(left, right)| left * right)
            .sum()
    }

    /// Cosine similarity in `-1...1`; zero when either vector is zero or the
    /// dimensions differ.
    #[must_use]
    pub fn cosine_similarity(&self, other: &Embedding) -> f32 {
        if self.0.len() != other.0.len() {
            return 0.0;
        }
        let denominator = self.magnitude() * other.magnitude();
        if denominator > 0.0 {
            self.dot(other) / denominator
        } else {
            0.0
        }
    }
}

/// A known person ranked against a new voice, what `SpeakerMemory` returns.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerMatch {
    pub person: Person,
    pub similarity: f32,
}

string_enum! {
    /// The case names of [`SpeakerAssignment`], shared by `meeting.json`
    /// and the `speaker.assignment` column.
    pub enum SpeakerAssignmentKind {
        Unknown = "unknown",
        Suggested = "suggested",
        Confirmed = "confirmed",
    }
}

/// How a diarization cluster maps to a person.
#[derive(Debug, Clone, PartialEq)]
pub enum SpeakerAssignment {
    Unknown,
    Suggested { person_id: Uuid, similarity: f32 },
    Confirmed { person_id: Uuid },
}

#[derive(Serialize, Deserialize)]
struct Suggested {
    #[serde(rename = "personID", with = "json::uuid_text")]
    person_id: Uuid,
    similarity: f32,
}

#[derive(Serialize, Deserialize)]
struct Confirmed {
    #[serde(rename = "personID", with = "json::uuid_text")]
    person_id: Uuid,
}

impl SpeakerAssignment {
    #[must_use]
    pub fn kind(&self) -> SpeakerAssignmentKind {
        match self {
            SpeakerAssignment::Unknown => SpeakerAssignmentKind::Unknown,
            SpeakerAssignment::Suggested { .. } => SpeakerAssignmentKind::Suggested,
            SpeakerAssignment::Confirmed { .. } => SpeakerAssignmentKind::Confirmed,
        }
    }

    /// The person behind the assignment; `None` for `Unknown`.
    #[must_use]
    pub fn person_id(&self) -> Option<Uuid> {
        match self {
            SpeakerAssignment::Unknown => None,
            SpeakerAssignment::Suggested { person_id, .. }
            | SpeakerAssignment::Confirmed { person_id } => Some(*person_id),
        }
    }

    #[must_use]
    pub fn is_confirmed(&self) -> bool {
        matches!(self, SpeakerAssignment::Confirmed { .. })
    }

    /// The `suggested` score; `None` otherwise.
    #[must_use]
    pub fn similarity(&self) -> Option<f32> {
        match self {
            SpeakerAssignment::Suggested { similarity, .. } => Some(*similarity),
            _ => None,
        }
    }

    /// The assignment from its three columns. A `suggested` or `confirmed`
    /// row without a person (only reachable by hand-edited SQL) reads as
    /// `Unknown` rather than inventing a person.
    #[must_use]
    pub fn from_columns(
        kind: SpeakerAssignmentKind,
        person_id: Option<Uuid>,
        similarity: Option<f32>,
    ) -> Self {
        match (kind, person_id) {
            (SpeakerAssignmentKind::Suggested, Some(person_id)) => SpeakerAssignment::Suggested {
                person_id,
                similarity: similarity.unwrap_or(0.0),
            },
            (SpeakerAssignmentKind::Confirmed, Some(person_id)) => {
                SpeakerAssignment::Confirmed { person_id }
            }
            _ => SpeakerAssignment::Unknown,
        }
    }
}

impl Serialize for SpeakerAssignment {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let name = self.kind().as_str();
        match *self {
            SpeakerAssignment::Unknown => case_coding::serialize_bare(name, serializer),
            SpeakerAssignment::Suggested {
                person_id,
                similarity,
            } => case_coding::serialize_payload(
                name,
                &Suggested {
                    person_id,
                    similarity,
                },
                serializer,
            ),
            SpeakerAssignment::Confirmed { person_id } => {
                case_coding::serialize_payload(name, &Confirmed { person_id }, serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for SpeakerAssignment {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (name, payload) = Case::deserialize(deserializer)?.split()?;
        let kind: SpeakerAssignmentKind = name.parse().map_err(serde::de::Error::custom)?;
        Ok(match kind {
            SpeakerAssignmentKind::Unknown => SpeakerAssignment::Unknown,
            SpeakerAssignmentKind::Suggested => {
                let Suggested {
                    person_id,
                    similarity,
                } = case_coding::payload(&name, payload)?;
                SpeakerAssignment::Suggested {
                    person_id,
                    similarity,
                }
            }
            SpeakerAssignmentKind::Confirmed => {
                let Confirmed { person_id } = case_coding::payload(&name, payload)?;
                SpeakerAssignment::Confirmed { person_id }
            }
        })
    }
}

/// A closed range of seconds, Swift's `ClosedRange<TimeInterval>`; JSON
/// form `[lower, upper]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeRange {
    pub lower: f64,
    pub upper: f64,
}

impl Serialize for TimeRange {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        (self.lower, self.upper).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TimeRange {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (lower, upper) = <(f64, f64)>::deserialize(deserializer)?;
        Ok(TimeRange { lower, upper })
    }
}

/// One diarization cluster inside a meeting. `cluster_label` ("Speaker 1")
/// is stable across re-exports and is what the LLM sees.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Speaker {
    #[serde(with = "json::uuid_text")]
    pub id: Uuid,
    #[serde(rename = "meetingID", with = "json::uuid_text")]
    pub meeting_id: Uuid,
    pub cluster_label: String,
    pub assignment: SpeakerAssignment,
    #[serde(skip)]
    pub embedding: Option<Embedding>,
    /// The diarizer-chosen range of the cluster's clearest speech.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_clip_range: Option<TimeRange>,
    /// The 10 s 16 kHz WAV clip written by the pipeline, as a file URL.
    #[serde(
        rename = "sampleClipURL",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sample_clip_url: Option<String>,
    /// Diarizer cluster quality in `0...1`, not the match score.
    pub cluster_confidence: f32,
}

impl Speaker {
    #[must_use]
    pub fn person_id(&self) -> Option<Uuid> {
        self.assignment.person_id()
    }
}

/// The model's guess who a speaker is (#78), one per speaker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerNameSuggestion {
    #[serde(rename = "speakerID", with = "json::uuid_text")]
    pub speaker_id: Uuid,
    /// `None` when the model found no evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub confidence: f64,
    pub evidence: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::to_column_string;

    #[test]
    fn embeddings_round_trip_as_little_endian_bytes() {
        let embedding = Embedding(vec![1.0, -0.5, 0.25]);
        let bytes = embedding.to_bytes();
        assert_eq!(bytes.len(), 12);
        assert_eq!(&bytes[..4], &[0, 0, 0x80, 0x3f]);
        assert_eq!(Embedding::from_bytes(&bytes), Some(embedding));
        assert_eq!(Embedding::from_bytes(&bytes[..5]), None);
    }

    #[test]
    fn cosine_similarity_is_zero_for_zero_or_mismatched_vectors() {
        let x = Embedding(vec![1.0, 0.0]);
        let y = Embedding(vec![0.0, 2.0]);
        let diagonal = Embedding(vec![3.0, 3.0]);
        assert_eq!(x.cosine_similarity(&x), 1.0);
        assert_eq!(x.cosine_similarity(&y), 0.0);
        assert!((x.cosine_similarity(&diagonal) - 0.707_106_77).abs() < 1e-6);
        assert_eq!(x.cosine_similarity(&Embedding(vec![0.0, 0.0])), 0.0);
        assert_eq!(x.cosine_similarity(&Embedding(vec![1.0])), 0.0);
    }

    #[test]
    fn normalized_has_unit_magnitude_except_for_the_zero_vector() {
        assert_eq!(Embedding(vec![3.0, 3.0]).normalized().magnitude(), 1.0);
        assert_eq!(Embedding(vec![0.0]).normalized(), Embedding(vec![0.0]));
    }

    #[test]
    fn assignments_use_the_case_coding_shape() {
        let person = Uuid::parse_str("516EADE8-40E5-4434-8AAF-9214A21A604E").unwrap();
        assert_eq!(
            to_column_string(&SpeakerAssignment::Unknown).unwrap(),
            r#""unknown""#
        );
        assert_eq!(
            to_column_string(&SpeakerAssignment::Confirmed { person_id: person }).unwrap(),
            r#"{"confirmed":{"personID":"516EADE8-40E5-4434-8AAF-9214A21A604E"}}"#
        );
        let suggested = SpeakerAssignment::Suggested {
            person_id: person,
            similarity: 0.7,
        };
        let text = to_column_string(&suggested).unwrap();
        assert_eq!(
            text,
            r#"{"suggested":{"personID":"516EADE8-40E5-4434-8AAF-9214A21A604E","similarity":0.7}}"#
        );
        assert_eq!(
            serde_json::from_str::<SpeakerAssignment>(&text).unwrap(),
            suggested
        );
    }
}
