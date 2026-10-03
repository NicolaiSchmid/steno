//! `meetingTask` and `decision` rows.
//! Swift: the task and decision methods of
//! `Sources/StenoCore/Storage/MeetingStore.swift`.

use rusqlite::{Connection, Row, params};
use uuid::Uuid;

use super::convert::{DbDate, DbEnum, DbUuid, RowExt as _};
use super::{Result, Store, execute_cached, insert_sql, query_all};
use crate::model::{Decision, MeetingTask};

const TASK_COLUMNS: &str =
    "id, meetingID, text, assigneePersonID, assigneeName, priority, dueDate, done";

fn task_from_row(row: &Row<'_>) -> rusqlite::Result<MeetingTask> {
    Ok(MeetingTask {
        id: row.col::<DbUuid>("id")?,
        meeting_id: row.col::<DbUuid>("meetingID")?,
        text: row.get("text")?,
        assignee_person_id: row.col::<Option<DbUuid>>("assigneePersonID")?,
        assignee_name: row.get("assigneeName")?,
        priority: row.col::<DbEnum<_>>("priority")?,
        due_date: row.col::<Option<DbDate>>("dueDate")?,
        done: row.get("done")?,
    })
}

pub(super) fn insert_task(connection: &Connection, task: &MeetingTask) -> Result<()> {
    execute_cached(
        connection,
        &insert_sql("meetingTask", TASK_COLUMNS),
        params![
            DbUuid(task.id),
            DbUuid(task.meeting_id),
            task.text,
            task.assignee_person_id.map(DbUuid),
            task.assignee_name,
            DbEnum(task.priority),
            task.due_date.map(DbDate),
            task.done,
        ],
    )
}

const DECISION_COLUMNS: &str = "id, meetingID, text";

fn decision_from_row(row: &Row<'_>) -> rusqlite::Result<Decision> {
    Ok(Decision {
        id: row.col::<DbUuid>("id")?,
        meeting_id: row.col::<DbUuid>("meetingID")?,
        text: row.get("text")?,
    })
}

pub(super) fn insert_decision(connection: &Connection, decision: &Decision) -> Result<()> {
    execute_cached(
        connection,
        &insert_sql("decision", DECISION_COLUMNS),
        params![
            DbUuid(decision.id),
            DbUuid(decision.meeting_id),
            decision.text
        ],
    )
}

/// The meeting's tasks in id order.
pub(super) fn tasks_of_meeting(
    connection: &Connection,
    meeting_id: Uuid,
) -> Result<Vec<MeetingTask>> {
    query_all(
        connection,
        &format!("SELECT {TASK_COLUMNS} FROM meetingTask WHERE meetingID = ?1 ORDER BY id"),
        [DbUuid(meeting_id)],
        task_from_row,
    )
}

/// The meeting's decisions in id order.
pub(super) fn decisions_of_meeting(
    connection: &Connection,
    meeting_id: Uuid,
) -> Result<Vec<Decision>> {
    query_all(
        connection,
        &format!("SELECT {DECISION_COLUMNS} FROM decision WHERE meetingID = ?1 ORDER BY id"),
        [DbUuid(meeting_id)],
        decision_from_row,
    )
}

impl Store {
    /// The meeting's tasks in id order.
    pub fn tasks(&self, meeting_id: Uuid) -> Result<Vec<MeetingTask>> {
        self.read(|connection| tasks_of_meeting(connection, meeting_id))
    }

    /// The meeting's decisions in id order.
    pub fn decisions(&self, meeting_id: Uuid) -> Result<Vec<Decision>> {
        self.read(|connection| decisions_of_meeting(connection, meeting_id))
    }
}
