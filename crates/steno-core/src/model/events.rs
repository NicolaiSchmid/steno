//! What the pipeline tells the UI beyond row changes.
//! Swift: `Sources/StenoCore/Events/MeetingEvent.swift` and
//! `Sources/StenoCore/Pipeline/ProcessingProgress.swift`.

use uuid::Uuid;

use crate::PipelineStage;

/// What a `progress` event carries: one fraction weighted by how long each
/// stage is expected to take on this machine, and the time the run is
/// expected to still need. Computed by the pipeline from its run; nothing
/// else computes a fraction or a pace.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessingProgress {
    pub stage: PipelineStage,
    /// elapsed / (elapsed + expected remaining) at the moment of posting,
    /// clamped so it never decreases within one run.
    pub fraction: f64,
    /// Where the next progress event is expected to land: the end of the
    /// stage's share, or the lane boundary inside transcribe. At most 1,
    /// never below `fraction`.
    pub next_fraction: f64,
    /// Wall clock the run is expected to still take.
    pub estimated_remaining_seconds: f64,
    /// True while any rate behind the estimate is still a seed.
    pub is_estimate_seeded: bool,
    /// The lane this event starts inside `transcribe`, zero based; 0 for
    /// every other stage.
    pub lane: usize,
    /// How many lanes `transcribe` runs over; 1 for every other stage.
    pub lane_count: usize,
}

impl ProcessingProgress {
    /// `(next_fraction - fraction) / (1 - fraction)` of the remaining time,
    /// so no presenter divides.
    #[must_use]
    pub fn expected_seconds_to_next_event(&self) -> f64 {
        if self.fraction >= 1.0 {
            return 0.0;
        }
        let share = ((self.next_fraction - self.fraction) / (1.0 - self.fraction)).clamp(0.0, 1.0);
        self.estimated_remaining_seconds * share
    }
}

/// What the pipeline and the store tell the UI beyond row changes.
/// Swift: `MeetingEvent`.
#[derive(Debug, Clone, PartialEq)]
pub enum MeetingEvent {
    /// Posted as each stage starts, once per lane inside `transcribe`.
    Progress {
        meeting_id: Uuid,
        progress: ProcessingProgress,
    },
    /// Posted after persist when any speaker is not confirmed.
    SpeakersNeedReview {
        meeting_id: Uuid,
        speaker_ids: Vec<Uuid>,
    },
    /// Posted once the `retention` stage has written the asset's expiry
    /// (`None` for keep forever): the run is over and the files may be due.
    /// Not posted while any delivery of the meeting is pending or failed.
    RetentionApplied { meeting_id: Uuid },
    /// Posted once a meeting's rows are gone.
    Deleted { meeting_id: Uuid },
    /// Posted when a run is refused because a model it needs is not
    /// installed: the meeting stays `queued` without a reason and is
    /// processed once the models are installed. Rust only: the Swift
    /// pipeline downloaded the model inside the run.
    ModelsMissing { meeting_id: Uuid },
    /// Posted when a summary re-run or a re-export fails after the
    /// pipeline accepted it. The services start both in the background and
    /// return at once, so this is how the failure reaches the detail's
    /// error line; a refusal (meeting busy, no summarizer) is returned to
    /// the caller instead. Swift had no event: `MeetingDetailViewModel`
    /// awaited the call and showed its error.
    OperationFailed {
        meeting_id: Uuid,
        operation: MeetingOperation,
        /// The stage that failed.
        stage: PipelineStage,
        /// The pipeline's failure as text, `stage: reason`.
        failure: String,
    },
}

/// The operations on a finished meeting that run in the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeetingOperation {
    SummaryRerun,
    Reexport,
}

impl MeetingOperation {
    /// The words the detail puts before `failed: <reason>`, as
    /// `MeetingDetailViewModel` names the operation.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            MeetingOperation::SummaryRerun => "Summary re-run",
            MeetingOperation::Reexport => "Re-export",
        }
    }
}

impl MeetingEvent {
    #[must_use]
    pub fn meeting_id(&self) -> Uuid {
        match self {
            MeetingEvent::Progress { meeting_id, .. }
            | MeetingEvent::SpeakersNeedReview { meeting_id, .. }
            | MeetingEvent::RetentionApplied { meeting_id }
            | MeetingEvent::Deleted { meeting_id }
            | MeetingEvent::ModelsMissing { meeting_id }
            | MeetingEvent::OperationFailed { meeting_id, .. } => *meeting_id,
        }
    }
}
