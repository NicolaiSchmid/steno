//! The list column, after `Main/MeetingListViewModel.swift`: every meeting
//! from the store, filtered by state, tag and an FTS query, grouped by
//! calendar day for the cards. The selection survives list updates and
//! clears only when a meeting the list had is gone.
//!
//! Swift debounced the query 200 ms on the clock before searching; the
//! blocking host searches when the command arrives, and the page debounces
//! typing (the parity list notes it).

use std::collections::{BTreeMap, BTreeSet};

use chrono::FixedOffset;
use steno_bridge::ListFilter;
use steno_core::paths::path_from_file_url;
use steno_core::{Meeting, MeetingStateKind, Person, Speaker, Store};
use uuid::Uuid;

use crate::labels::day_string;

/// Whether `filter` shows `meeting`. Swift: `StateFilter.matches`.
#[must_use]
pub fn filter_matches(filter: ListFilter, meeting: &Meeting) -> bool {
    match filter {
        ListFilter::All => true,
        ListFilter::Processing => matches!(
            meeting.state.kind(),
            MeetingStateKind::Recording | MeetingStateKind::Queued | MeetingStateKind::Processing
        ),
        ListFilter::Ready => meeting.state.kind() == MeetingStateKind::Ready,
        ListFilter::Failed => meeting.state.is_failed(),
    }
}

/// One card: the meetings that started on `day` (`YYYY-MM-DD` in the
/// model's zone), newest first. Swift: `MeetingListViewModel.DayGroup`.
#[derive(Debug, Clone, PartialEq)]
pub struct DayGroup {
    pub day: String,
    pub meetings: Vec<Meeting>,
}

impl DayGroup {
    /// Buckets `meetings` (already newest first) by their day in `zone`,
    /// keeping the order, so the groups run newest first too.
    #[must_use]
    pub fn group(meetings: &[Meeting], zone: FixedOffset) -> Vec<DayGroup> {
        let mut groups: Vec<DayGroup> = Vec::new();
        for meeting in meetings {
            let day = day_string(meeting.started_at, zone);
            match groups.last_mut() {
                Some(last) if last.day == day => last.meetings.push(meeting.clone()),
                _ => groups.push(DayGroup {
                    day,
                    meetings: vec![meeting.clone()],
                }),
            }
        }
        groups
    }
}

/// Swift: `MeetingListViewModel`.
#[derive(Debug)]
pub struct MeetingListViewModel {
    pub all: Vec<Meeting>,
    pub meetings: Vec<Meeting>,
    pub day_groups: Vec<DayGroup>,
    pub search_hits: Option<BTreeSet<Uuid>>,
    pub error: Option<String>,
    /// The speakers of every listed meeting and the people they resolve
    /// to, for the rows' speaker chips.
    pub speakers_by_meeting: BTreeMap<Uuid, Vec<Speaker>>,
    pub persons_by_id: BTreeMap<Uuid, Person>,
    pub query: String,
    pub state_filter: ListFilter,
    pub tag_filter: Option<String>,
    pub selection: Option<Uuid>,
    /// Day boundaries for the cards; the viewer's zone.
    pub zone: FixedOffset,
    /// How many hits a search reads at most. Swift: `limit: 200`.
    pub search_limit: i64,
}

impl MeetingListViewModel {
    #[must_use]
    pub fn new(zone: FixedOffset) -> Self {
        MeetingListViewModel {
            all: Vec::new(),
            meetings: Vec::new(),
            day_groups: Vec::new(),
            search_hits: None,
            error: None,
            speakers_by_meeting: BTreeMap::new(),
            persons_by_id: BTreeMap::new(),
            query: String::new(),
            state_filter: ListFilter::All,
            tag_filter: None,
            selection: None,
            zone,
            search_limit: 200,
        }
    }

    /// The store's meeting table as of now: what `observeMeetings()`
    /// delivered. Each update names the meetings it removed, so a selection
    /// made ahead of its row survives an emission snapshotted before the
    /// insert. The speakers and persons reload with it.
    pub fn reload(&mut self, store: &Store) {
        match store.all_meetings() {
            Ok(mut meetings) => {
                meetings.sort_by(|left, right| {
                    right
                        .started_at
                        .cmp(&left.started_at)
                        .then_with(|| left.id.cmp(&right.id))
                });
                let current: BTreeSet<Uuid> = meetings.iter().map(|meeting| meeting.id).collect();
                let removed: Vec<Uuid> = self
                    .all
                    .iter()
                    .map(|meeting| meeting.id)
                    .filter(|id| !current.contains(id))
                    .collect();
                self.all = meetings;
                self.apply(&removed);
                self.reload_speakers(store);
            }
            Err(error) => self.error = Some(format!("Meetings could not be loaded: {error}")),
        }
    }

    fn reload_speakers(&mut self, store: &Store) {
        let ids: Vec<Uuid> = self.all.iter().map(|meeting| meeting.id).collect();
        self.speakers_by_meeting = store.speakers_for_meetings(&ids).unwrap_or_default();
        self.persons_by_id = store
            .persons()
            .unwrap_or_default()
            .into_iter()
            .map(|person| (person.id, person))
            .collect();
    }

    /// How many of every meeting a filter row would show, before the tag
    /// filter and the query; the nav column's counts.
    #[must_use]
    pub fn count_for(&self, filter: ListFilter) -> i64 {
        i64::try_from(
            self.all
                .iter()
                .filter(|meeting| filter_matches(filter, meeting))
                .count(),
        )
        .unwrap_or(i64::MAX)
    }

    /// The selected meeting, when it is still stored.
    #[must_use]
    pub fn selected_meeting(&self) -> Option<&Meeting> {
        let selection = self.selection?;
        self.all.iter().find(|meeting| meeting.id == selection)
    }

    #[must_use]
    pub fn contains(&self, id: Uuid) -> bool {
        self.all.iter().any(|meeting| meeting.id == id)
    }

    pub fn set_state_filter(&mut self, filter: ListFilter) {
        self.state_filter = filter;
        self.apply(&[]);
    }

    pub fn set_tag_filter(&mut self, tag: Option<String>) {
        self.tag_filter = tag;
        self.apply(&[]);
    }

    /// Sets the query and searches: a blank query shows every meeting, any
    /// other narrows the list to the meetings the FTS index returns.
    pub fn set_query(&mut self, query: String, store: &Store) {
        if query == self.query {
            return;
        }
        self.query = query;
        let trimmed = self.query.trim();
        if trimmed.is_empty() {
            self.search_hits = None;
        } else {
            let hits = store.search(trimmed, self.search_limit).unwrap_or_default();
            self.search_hits = Some(hits.into_iter().map(|hit| hit.meeting_id).collect());
        }
        self.apply(&[]);
    }

    /// Rebuilds the visible list; `removed` are the meetings the last store
    /// update dropped, the only thing that clears the selection.
    fn apply(&mut self, removed: &[Uuid]) {
        let filtered: Vec<Meeting> = self
            .all
            .iter()
            .filter(|meeting| filter_matches(self.state_filter, meeting))
            .filter(|meeting| {
                self.tag_filter
                    .as_ref()
                    .is_none_or(|tag| meeting.tags.contains(tag))
            })
            .filter(|meeting| {
                self.search_hits
                    .as_ref()
                    .is_none_or(|hits| hits.contains(&meeting.id))
            })
            .cloned()
            .collect();
        self.day_groups = DayGroup::group(&filtered, self.zone);
        self.meetings = filtered;
        if let Some(selection) = self.selection
            && removed.contains(&selection)
        {
            self.selection = None;
        }
    }

    /// The store refuses while the capture writer or the pipeline holds
    /// the meeting's files; the controls say so before the attempt.
    #[must_use]
    pub fn can_delete(meeting: &Meeting) -> bool {
        !matches!(
            meeting.state.kind(),
            MeetingStateKind::Recording | MeetingStateKind::Processing
        )
    }

    /// The store's delete: rows, receipt and the meeting's files go; a
    /// meeting still recording or processing is refused and the reason
    /// shown. A deleted selection clears itself when the list reloads.
    pub fn delete(&mut self, id: Uuid, store: &Store) {
        match store.delete_meeting(id) {
            Ok(deleted) => {
                for url in deleted
                    .assets
                    .iter()
                    .flat_map(steno_core::AudioAsset::expirable_files)
                    .chain(deleted.clips.iter().cloned())
                {
                    if let Some(path) = path_from_file_url(&url) {
                        let _ = std::fs::remove_file(&path);
                        if let Some(folder) = path.parent() {
                            let _ = std::fs::remove_dir(folder);
                        }
                    }
                }
                self.error = None;
            }
            Err(error) => self.error = Some(format!("Meeting could not be deleted: {error}")),
        }
    }
}
