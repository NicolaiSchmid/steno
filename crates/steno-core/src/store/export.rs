//! The export every adapter receives, read in one transaction.
//! Swift: `Sources/StenoCore/Storage/MeetingStore+Export.swift`.

use std::collections::BTreeSet;

use rusqlite::Connection;
use uuid::Uuid;

use super::{Result, Store, StoreError, assets, meetings, people, tasks, transcript};
use crate::model::{MeetingExport, Speaker};

/// Everything an adapter receives: the meeting, its participants by display
/// name, speakers by cluster label, the persons any of them or any task
/// points at (by display name), segments by start, tasks and decisions by
/// id, and the asset. `None` when there is no such meeting.
pub(super) fn export_rows(
    connection: &Connection,
    meeting_id: Uuid,
) -> Result<Option<MeetingExport>> {
    let Some(meeting) = meetings::fetch(connection, meeting_id)? else {
        return Ok(None);
    };
    let participants = people::participants_of_meeting(connection, meeting_id)?;
    let speakers = people::speakers_of_meeting(connection, meeting_id)?;
    let segments = transcript::segments_of_meeting(connection, meeting_id)?;
    let tasks = tasks::tasks_of_meeting(connection, meeting_id)?;
    let decisions = tasks::decisions_of_meeting(connection, meeting_id)?;

    let mut person_ids = BTreeSet::new();
    person_ids.extend(speakers.iter().filter_map(Speaker::person_id));
    person_ids.extend(
        participants
            .iter()
            .filter_map(|participant| participant.person_id),
    );
    person_ids.extend(tasks.iter().filter_map(|task| task.assignee_person_id));
    let person_ids: Vec<Uuid> = person_ids.into_iter().collect();
    let persons = people::persons_with_ids(connection, &person_ids)?;

    let audio = assets::assets_of_meeting(connection, meeting_id)?
        .into_iter()
        .next();

    Ok(Some(MeetingExport {
        schema_version: MeetingExport::CURRENT_SCHEMA_VERSION,
        meeting,
        participants,
        speakers,
        persons,
        segments,
        tasks,
        decisions,
        audio,
    }))
}

impl Store {
    /// Everything an adapter receives, read in one transaction. The
    /// `StenoJSON` pretty form of the result is `meeting.json`.
    pub fn export(&self, meeting_id: Uuid) -> Result<MeetingExport> {
        self.read(|connection| {
            export_rows(connection, meeting_id)?.ok_or(StoreError::MeetingNotFound(meeting_id))
        })
    }
}
