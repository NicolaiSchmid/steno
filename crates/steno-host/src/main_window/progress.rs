//! Where the pipeline is with every meeting that is queued or processing,
//! after `Main/ProcessingProgressModel.swift`, and the two event types it
//! consumes, after `Sources/StenoCore/{Pipeline/ProcessingProgress,Events/MeetingEvent}.swift`.
//! The event types live in the core (`steno_core::MeetingEvent`,
//! `steno_core::ProcessingProgress`) and are re-exported here.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use steno_core::{Meeting, MeetingStateKind, PipelineStage};
use uuid::Uuid;

use crate::labels::stage_label;

pub use steno_core::{MeetingEvent, ProcessingProgress};

/// One queued or processing meeting as the model tracks it.
/// Swift: `ProcessingProgressModel.Entry`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgressEntry {
    pub meeting_id: Uuid,
    /// The last `progress` event of the run; `None` before the run's first
    /// event, while the meeting waits in the queue or the engines load.
    pub progress: Option<ProcessingProgress>,
    /// When `progress` landed, or when the entry was created.
    pub since: DateTime<Utc>,
    /// The last run was refused because a model is not installed
    /// ([`MeetingEvent::ModelsMissing`]); the meeting waits in the queue
    /// until the next run's first event. Rust only.
    pub models_missing: bool,
}

impl ProgressEntry {
    /// The card, chip, list entry and menu bar row before a run's first
    /// event.
    pub const WAITING_TITLE: &'static str = "Waiting to process";

    /// The title of a meeting whose run was refused for a missing model,
    /// `PipelineFailure::MODELS_MISSING` in `steno-pipeline`.
    pub const MODELS_MISSING_TITLE: &'static str = "Download the speech models in Settings";

    /// The bridge stage of such a meeting, beside `waiting` and the
    /// pipeline's stages; the page drops "starts when the current meeting
    /// finishes" for it.
    pub const MODELS_MISSING_STAGE: &'static str = "modelsMissing";

    #[must_use]
    pub fn stage(&self) -> Option<PipelineStage> {
        self.progress.as_ref().map(|progress| progress.stage)
    }

    /// `stage_label` plus an ellipsis, "Transcribing…"; [`Self::WAITING_TITLE`]
    /// before the first event, [`Self::MODELS_MISSING_TITLE`] after a
    /// refusal for a missing model.
    #[must_use]
    pub fn title(&self) -> String {
        if self.models_missing {
            return Self::MODELS_MISSING_TITLE.to_owned();
        }
        self.progress.as_ref().map_or_else(
            || Self::WAITING_TITLE.to_owned(),
            |progress| format!("{}…", stage_label(progress.stage)),
        )
    }

    /// The bar's value; 0 before the first event.
    #[must_use]
    pub fn fraction(&self) -> f64 {
        self.progress
            .as_ref()
            .map_or(0.0, |progress| progress.fraction)
    }

    #[must_use]
    pub fn estimated_remaining_seconds(&self) -> Option<f64> {
        self.progress
            .as_ref()
            .map(|progress| progress.estimated_remaining_seconds)
    }
}

/// Swift: `ProcessingProgressModel`.
#[derive(Debug, Default)]
pub struct ProcessingProgressModel {
    pub entries: BTreeMap<Uuid, ProgressEntry>,
}

impl ProcessingProgressModel {
    #[must_use]
    pub fn entry(&self, meeting_id: Uuid) -> Option<&ProgressEntry> {
        self.entries.get(&meeting_id)
    }

    /// `progress` at `decode`, the first event of a run, starts a new entry
    /// even when an earlier run's entry is still there; any other stage
    /// updates the meeting's entry and never lowers its fraction. Progress
    /// for a meeting without an entry drives nothing. `deleted` evicts.
    /// `modelsMissing` starts an entry without progress that says so,
    /// until the next run's first event.
    pub fn apply(&mut self, event: &MeetingEvent, now: DateTime<Utc>) {
        match event {
            MeetingEvent::Progress {
                meeting_id,
                progress,
            } => {
                if progress.stage == PipelineStage::Decode {
                    self.entries.insert(
                        *meeting_id,
                        ProgressEntry {
                            meeting_id: *meeting_id,
                            progress: Some(progress.clone()),
                            since: now,
                            models_missing: false,
                        },
                    );
                } else if let Some(current) = self.entries.get(meeting_id) {
                    let mut next = progress.clone();
                    next.fraction = next.fraction.max(current.fraction());
                    next.next_fraction = next.next_fraction.max(next.fraction);
                    self.entries.insert(
                        *meeting_id,
                        ProgressEntry {
                            meeting_id: *meeting_id,
                            progress: Some(next),
                            since: now,
                            models_missing: false,
                        },
                    );
                }
            }
            MeetingEvent::Deleted { meeting_id } => {
                self.entries.remove(meeting_id);
            }
            MeetingEvent::ModelsMissing { meeting_id } => {
                self.entries.insert(
                    *meeting_id,
                    ProgressEntry {
                        meeting_id: *meeting_id,
                        progress: None,
                        since: now,
                        models_missing: true,
                    },
                );
            }
            MeetingEvent::SpeakersNeedReview { .. }
            | MeetingEvent::RetentionApplied { .. }
            | MeetingEvent::OperationFailed { .. } => {}
        }
    }

    /// The meeting list as the store delivers it: every meeting in `queued`
    /// or `processing` has an entry (without progress until its first
    /// event) and a meeting listed in another state loses its entry. A
    /// meeting absent from the list keeps its entry; deletion evicts
    /// through the `deleted` event.
    pub fn meetings_changed(&mut self, meetings: &[Meeting], now: DateTime<Utc>) {
        for meeting in meetings {
            match meeting.state.kind() {
                MeetingStateKind::Queued | MeetingStateKind::Processing => {
                    self.entries
                        .entry(meeting.id)
                        .or_insert_with(|| ProgressEntry {
                            meeting_id: meeting.id,
                            progress: None,
                            since: now,
                            models_missing: false,
                        });
                }
                MeetingStateKind::Recording
                | MeetingStateKind::Ready
                | MeetingStateKind::Failed => {
                    self.entries.remove(&meeting.id);
                }
            }
        }
    }
}
