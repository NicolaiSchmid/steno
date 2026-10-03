//! The export every adapter receives, read in one transaction.
//! Swift: `Sources/StenoCore/Storage/MeetingStore+Export.swift`.

use std::collections::BTreeSet;

use rusqlite::Transaction;
use uuid::Uuid;

use super::{Result, Store, StoreError, assets, meetings, people, tasks, transcript};
use crate::model::{MeetingExport, Speaker};

/// Everything an adapter receives: the meeting, its participants by display
/// name, speakers by cluster label, the persons any of them or any task
/// points at (by display name), segments by start, tasks and decisions by
/// id, and the asset. `None` when there is no such meeting. Takes the
/// transaction rather than the connection so no query can run outside
/// one snapshot.
pub(super) fn export_rows(
    transaction: &Transaction<'_>,
    meeting_id: Uuid,
) -> Result<Option<MeetingExport>> {
    let Some(meeting) = meetings::fetch(transaction, meeting_id)? else {
        return Ok(None);
    };
    let participants = people::participants_of_meeting(transaction, meeting_id)?;
    let speakers = people::speakers_of_meeting(transaction, meeting_id)?;
    let segments = transcript::segments_of_meeting(transaction, meeting_id)?;
    let tasks = tasks::tasks_of_meeting(transaction, meeting_id)?;
    let decisions = tasks::decisions_of_meeting(transaction, meeting_id)?;

    let mut person_ids = BTreeSet::new();
    person_ids.extend(speakers.iter().filter_map(Speaker::person_id));
    person_ids.extend(
        participants
            .iter()
            .filter_map(|participant| participant.person_id),
    );
    person_ids.extend(tasks.iter().filter_map(|task| task.assignee_person_id));
    let person_ids: Vec<Uuid> = person_ids.into_iter().collect();
    let persons = people::persons_with_ids(transaction, &person_ids)?;

    let audio = assets::assets_of_meeting(transaction, meeting_id)?
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
    /// Everything an adapter receives, read in one deferred transaction:
    /// one snapshot across every query while the Swift app may be
    /// writing the same file (GRDB's `writer.read` on the Swift side),
    /// released when the rows are in hand. The `StenoJSON` pretty form of
    /// the result is `meeting.json`.
    ///
    /// ```no_run
    /// use steno_core::{StenoPaths, Store};
    /// use uuid::Uuid;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let store = Store::open(StenoPaths::create_default()?.database_path())?;
    /// let export = store.export(Uuid::parse_str("0D6F1C2E-0000-4000-8000-000000000001")?)?;
    /// println!("{}: {} segments", export.meeting.title, export.segments.len());
    /// # Ok(())
    /// # }
    /// ```
    pub fn export(&self, meeting_id: Uuid) -> Result<MeetingExport> {
        self.read(|connection| {
            let transaction = connection.unchecked_transaction()?;
            export_rows(&transaction, meeting_id)?.ok_or(StoreError::MeetingNotFound(meeting_id))
        })
    }
}
