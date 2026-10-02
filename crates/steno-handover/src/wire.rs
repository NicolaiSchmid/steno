//! The JSON bodies of the handover wire (v1).
//! `mobile/modules/steno-link/src/wire.ts` mirrors every name here;
//! `RecordingMetadata` is core's type. The host reads wire values only
//! through `HandoverReceipt` and `PairingPayload`.
//! Swift: `Routing/Wire.swift`.

use serde::{Deserialize, Serialize};
use steno_core::HandoverStateKind;
use steno_core::json::uuid_text;
use uuid::Uuid;

pub const PROTOCOL_VERSION: i64 = 1;
pub const SERVICE_TYPE: &str = "_steno._tcp";
pub const CHUNK_HASH_HEADER: &str = "X-Steno-Chunk-SHA256";

/// `GET /v1/hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    #[serde(rename = "macID", with = "uuid_text")]
    pub mac_id: Uuid,
    pub protocol: i64,
}

impl Hello {
    #[must_use]
    pub fn new(mac_id: Uuid) -> Self {
        Hello {
            mac_id,
            protocol: PROTOCOL_VERSION,
        }
    }
}

/// `POST /v1/pair` body, sent with `Authorization: Pairing <secret>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairRequest {
    #[serde(rename = "deviceID", with = "uuid_text")]
    pub device_id: Uuid,
    pub device_name: String,
}

/// `POST /v1/pair` response; `token` is the bearer for every later call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairResponse {
    pub token: String,
    #[serde(rename = "macID", with = "uuid_text")]
    pub mac_id: Uuid,
    pub mac_name: String,
}

/// `GET /v1/recordings/{id}`, the announce response and the 409 body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStatus {
    pub state: HandoverStateKind,
    pub received_chunks: Vec<i64>,
}

/// `POST /v1/recordings/{id}/complete` 200 body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompleteResponse {
    #[serde(rename = "meetingID", with = "uuid_text")]
    pub meeting_id: Uuid,
}

/// Every error status carries one of these, for logs on the phone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    pub error: String,
}

impl Problem {
    #[must_use]
    pub fn new(error: impl Into<String>) -> Self {
        Problem {
            error: error.into(),
        }
    }
}
