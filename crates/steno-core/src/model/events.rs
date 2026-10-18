//! What the pipeline tells the UI beyond row changes.

use uuid::Uuid;

use crate::PipelineStage;

/// What a `progress` event carries: one fraction weighted by each stage's
/// expected time, and the time still needed. Only the pipeline computes it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessingProgress {
    pub stage: PipelineStage,
    /// elapsed / (elapsed + expected remaining), never decreasing within a run.
    pub fraction: f64,
    /// Where the next progress event is expected to land; at most 1, never below
    /// `fraction`.
    pub next_fraction: f64,
    /// Wall clock the run is expected to still take.
    pub estimated_remaining_seconds: f64,
    /// True while any rate behind the estimate is still a seed.
    pub is_estimate_seeded: bool,
    /// The lane this event starts inside `transcribe`; 0 for other stages.
    pub lane: usize,
    /// How many lanes `transcribe` runs over; 1 for every other stage.
    pub lane_count: usize,
}

impl ProcessingProgress {
    /// The share of the remaining time until `next_fraction`.
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
    /// Posted once retention has written the asset's expiry; not while a delivery
    /// is pending or failed.
    RetentionApplied { meeting_id: Uuid },
    /// Posted once a meeting's rows are gone.
    Deleted { meeting_id: Uuid },
    /// A model the run needs is not installed: the meeting stays `queued`.
    ModelsMissing { meeting_id: Uuid },
    /// Another process held the store past its busy timeout: the meeting stays
    /// `queued` and is processed again later.
    StoreBusy { meeting_id: Uuid },
    /// A job waits for the recording to stop before running an in-process speech
    /// engine, whose abort would end the recording too.
    WaitingForRecording { meeting_id: Uuid },
    /// A background summary re-run or re-export failed after the pipeline
    /// accepted it.
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
    /// The words the detail puts before `failed: <reason>`.
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
            | MeetingEvent::StoreBusy { meeting_id }
            | MeetingEvent::WaitingForRecording { meeting_id }
            | MeetingEvent::OperationFailed { meeting_id, .. } => *meeting_id,
        }
    }
}
