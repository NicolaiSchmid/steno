use rusqlite::{Connection, Row, params};
use uuid::Uuid;

use super::convert::{DbDate, DbEnum, DbUuid, Unwrap as _};
use super::{Result, Store};
use crate::model::{Decision, MeetingTask};

const TASK_COLUMNS: &str =
    "id, meetingID, text, assigneePersonID, assigneeName, priority, dueDate, done";

fn task_from_row(row: &Row<'_>) -> rusqlite::Result<MeetingTask> {
    Ok(MeetingTask {
        id: row.get::<_, DbUuid>("id")?.0,
        meeting_id: row.get::<_, DbUuid>("meetingID")?.0,
        text: row.get("text")?,
        assignee_person_id: row
            .get::<_, Option<DbUuid>>("assigneePersonID")?
            .unwrap_db(),
        assignee_name: row.get("assigneeName")?,
        priority: row.get::<_, DbEnum<_>>("priority")?.0,
        due_date: row.get::<_, Option<DbDate>>("dueDate")?.unwrap_db(),
        done: row.get("done")?,
    })
}

pub(super) fn insert_task(connection: &Connection, task: &MeetingTask) -> Result<()> {
    connection.execute(
        &format!(
            "INSERT INTO meetingTask ({TASK_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
        ),
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
    )?;
    Ok(())
}

pub(super) fn insert_decision(connection: &Connection, decision: &Decision) -> Result<()> {
    connection.execute(
        "INSERT INTO decision (id, meetingID, text) VALUES (?1, ?2, ?3)",
        params![
            DbUuid(decision.id),
            DbUuid(decision.meeting_id),
            decision.text
        ],
    )?;
    Ok(())
}

impl Store {
    /// The meeting's tasks in id order.
    pub fn tasks(&self, meeting_id: Uuid) -> Result<Vec<MeetingTask>> {
        self.read(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {TASK_COLUMNS} FROM meetingTask WHERE meetingID = ?1 ORDER BY id"
            ))?;
            let rows = statement.query_map([DbUuid(meeting_id)], task_from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    /// The meeting's decisions in id order.
    pub fn decisions(&self, meeting_id: Uuid) -> Result<Vec<Decision>> {
        self.read(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, meetingID, text FROM decision WHERE meetingID = ?1 ORDER BY id",
            )?;
            let rows = statement.query_map([DbUuid(meeting_id)], |row| {
                Ok(Decision {
                    id: row.get::<_, DbUuid>("id")?.0,
                    meeting_id: row.get::<_, DbUuid>("meetingID")?.0,
                    text: row.get("text")?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
}
