//! Phone handover: the paired device, its state and the receipt.
//! Swift: `Sources/StenoCore/Model/Handover.swift`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::json::{
    self,
    case_coding::{self, Case},
};
use crate::string_enum;

/// A phone paired with this computer. The bearer token itself is never
/// stored; the store keeps its SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDevice {
    #[serde(with = "json::uuid_text")]
    pub id: Uuid,
    pub name: String,
    #[serde(with = "json::iso_time")]
    pub paired_at: DateTime<Utc>,
    #[serde(
        default,
        with = "json::iso_time_opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_seen_at: Option<DateTime<Utc>>,
}

string_enum! {
    /// The case names of [`HandoverState`].
    pub enum HandoverStateKind {
        Receiving = "receiving",
        Verifying = "verifying",
        Complete = "complete",
        Failed = "failed",
    }
}

/// Where one phone recording's handover stands.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HandoverState {
    Receiving,
    Verifying,
    Complete { meeting_id: Uuid },
    Failed(String),
}

#[derive(Serialize, Deserialize)]
struct Complete {
    #[serde(rename = "meetingID", with = "json::uuid_text")]
    meeting_id: Uuid,
}

impl HandoverState {
    #[must_use]
    pub fn kind(&self) -> HandoverStateKind {
        match self {
            HandoverState::Receiving => HandoverStateKind::Receiving,
            HandoverState::Verifying => HandoverStateKind::Verifying,
            HandoverState::Complete { .. } => HandoverStateKind::Complete,
            HandoverState::Failed(_) => HandoverStateKind::Failed,
        }
    }

    #[must_use]
    pub fn meeting_id(&self) -> Option<Uuid> {
        match self {
            HandoverState::Complete { meeting_id } => Some(*meeting_id),
            _ => None,
        }
    }

    /// The state from its three columns; a `complete` row without a meeting
    /// id reads as a failure, as in Swift.
    #[must_use]
    pub fn from_columns(
        kind: HandoverStateKind,
        meeting_id: Option<Uuid>,
        failure_message: Option<String>,
    ) -> Self {
        match (kind, meeting_id) {
            (HandoverStateKind::Receiving, _) => HandoverState::Receiving,
            (HandoverStateKind::Verifying, _) => HandoverState::Verifying,
            (HandoverStateKind::Complete, Some(meeting_id)) => {
                HandoverState::Complete { meeting_id }
            }
            (HandoverStateKind::Complete, None) => {
                HandoverState::Failed("complete without a meeting id".to_owned())
            }
            (HandoverStateKind::Failed, _) => {
                HandoverState::Failed(failure_message.unwrap_or_default())
            }
        }
    }
}

impl Serialize for HandoverState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let name = self.kind().as_str();
        match self {
            HandoverState::Complete { meeting_id } => case_coding::serialize_payload(
                name,
                &Complete {
                    meeting_id: *meeting_id,
                },
                serializer,
            ),
            HandoverState::Failed(message) => {
                case_coding::serialize_payload(name, message, serializer)
            }
            _ => case_coding::serialize_bare(name, serializer),
        }
    }
}

impl<'de> Deserialize<'de> for HandoverState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (name, payload) = Case::deserialize(deserializer)?.split()?;
        let kind: HandoverStateKind = name.parse().map_err(serde::de::Error::custom)?;
        Ok(match kind {
            HandoverStateKind::Receiving => HandoverState::Receiving,
            HandoverStateKind::Verifying => HandoverState::Verifying,
            HandoverStateKind::Complete => {
                let Complete { meeting_id } = case_coding::payload(&name, payload)?;
                HandoverState::Complete { meeting_id }
            }
            HandoverStateKind::Failed => {
                HandoverState::Failed(case_coding::payload(&name, payload)?)
            }
        })
    }
}

/// Progress of one phone recording being handed over; the idempotency key
/// of the intake.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoverReceipt {
    #[serde(rename = "recordingID", with = "json::uuid_text")]
    pub recording_id: Uuid,
    #[serde(rename = "deviceID", with = "json::uuid_text")]
    pub device_id: Uuid,
    pub state: HandoverState,
    pub byte_count: i64,
    #[serde(with = "json::base64_bytes")]
    pub sha256: Vec<u8>,
    pub chunk_size: i64,
    /// Indexes of the chunks received so far, ascending.
    pub received_chunks: Vec<i64>,
    #[serde(with = "json::iso_time")]
    pub created_at: DateTime<Utc>,
    #[serde(with = "json::iso_time")]
    pub updated_at: DateTime<Utc>,
}
