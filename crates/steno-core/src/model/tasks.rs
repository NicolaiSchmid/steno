use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::json;

string_enum! {
    pub enum TaskPriority {
        Low = "low",
        Normal = "normal",
        High = "high",
    }
}

/// A task the LLM extracted from the meeting.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingTask {
    #[serde(with = "json::uuid_text")]
    pub id: Uuid,
    #[serde(rename = "meetingID", with = "json::uuid_text")]
    pub meeting_id: Uuid,
    pub text: String,
    #[serde(
        rename = "assigneePersonID",
        default,
        with = "json::uuid_text_opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub assignee_person_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_name: Option<String>,
    pub priority: TaskPriority,
    #[serde(
        default,
        with = "json::iso_time_opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub due_date: Option<DateTime<Utc>>,
    pub done: bool,
}

/// A decision the LLM extracted from the meeting. Ids derive from the
/// meeting id (`decision-<index>`) so re-runs are stable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    #[serde(with = "json::uuid_text")]
    pub id: Uuid,
    #[serde(rename = "meetingID", with = "json::uuid_text")]
    pub meeting_id: Uuid,
    pub text: String,
}
