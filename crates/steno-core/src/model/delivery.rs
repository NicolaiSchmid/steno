//! Deliveries of a meeting to a destination and their receipts.
//! Swift: `Sources/StenoCore/Model/Delivery.swift`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use super::derived_uuid;
use crate::json::{
    self,
    case_coding::{self, Case},
};
use crate::string_enum;

string_enum! {
    /// The case names of [`DeliveryStatus`].
    pub enum DeliveryStatusKind {
        Pending = "pending",
        Delivered = "delivered",
        Failed = "failed",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DeliveryStatus {
    Pending,
    Delivered,
    Failed(String),
}

impl DeliveryStatus {
    #[must_use]
    pub fn kind(&self) -> DeliveryStatusKind {
        match self {
            DeliveryStatus::Pending => DeliveryStatusKind::Pending,
            DeliveryStatus::Delivered => DeliveryStatusKind::Delivered,
            DeliveryStatus::Failed(_) => DeliveryStatusKind::Failed,
        }
    }

    #[must_use]
    pub fn failure_message(&self) -> Option<&str> {
        match self {
            DeliveryStatus::Failed(message) => Some(message),
            _ => None,
        }
    }

    #[must_use]
    pub fn from_columns(kind: DeliveryStatusKind, failure_message: Option<String>) -> Self {
        match kind {
            DeliveryStatusKind::Pending => DeliveryStatus::Pending,
            DeliveryStatusKind::Delivered => DeliveryStatus::Delivered,
            DeliveryStatusKind::Failed => {
                DeliveryStatus::Failed(failure_message.unwrap_or_default())
            }
        }
    }
}

impl Serialize for DeliveryStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            DeliveryStatus::Failed(message) => {
                case_coding::serialize_payload(self.kind().as_str(), message, serializer)
            }
            _ => case_coding::serialize_bare(self.kind().as_str(), serializer),
        }
    }
}

impl<'de> Deserialize<'de> for DeliveryStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (name, payload) = Case::deserialize(deserializer)?.split()?;
        let kind: DeliveryStatusKind = name.parse().map_err(serde::de::Error::custom)?;
        Ok(match kind {
            DeliveryStatusKind::Failed => {
                DeliveryStatus::Failed(case_coding::payload(&name, payload)?)
            }
            other => DeliveryStatus::from_columns(other, None),
        })
    }
}

/// Per-destination delivery state of one meeting, one row per (meeting,
/// destination); the id derives from the pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delivery {
    #[serde(with = "json::uuid_text")]
    pub id: Uuid,
    #[serde(rename = "meetingID", with = "json::uuid_text")]
    pub meeting_id: Uuid,
    #[serde(rename = "destinationID")]
    pub destination_id: String,
    pub status: DeliveryStatus,
    #[serde(
        default,
        with = "json::iso_time_opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_attempt_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<DeliveryReceipt>,
}

impl Delivery {
    /// The one id of the (meeting, destination) pair.
    #[must_use]
    pub fn id_for(meeting_id: Uuid, destination_id: &str) -> Uuid {
        derived_uuid(meeting_id, &format!("delivery-{destination_id}"))
    }
}

string_enum! {
    /// Whether a delivered file belongs to Steno outright or is a block
    /// inside a file the user also edits.
    pub enum FileOwnership {
        Owned = "owned",
        ManagedBlock = "managedBlock",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveredFile {
    pub relative_path: String,
    pub ownership: FileOwnership,
    #[serde(with = "json::base64_bytes")]
    pub sha256: Vec<u8>,
}

/// What a destination wrote, passed back on re-delivery so it overwrites
/// its own files and never touches anything else.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryReceipt {
    /// The destination root at delivery time, for example the vault path.
    pub root: String,
    /// The meeting folder relative to `root`, pinned at first delivery.
    pub folder: String,
    pub files: Vec<DeliveredFile>,
    pub renderer_version: i64,
    /// What this delivery could not do although it succeeded, in words for
    /// the user: the meeting's export line and `steno deliver` show them
    /// until the next delivery, which starts without any. Empty, and absent
    /// from the JSON, when there is nothing to say, so a receipt without
    /// warnings reads and encodes as Swift's. Swift: none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}
