//! The domain types, one for one with `Sources/StenoCore/Model` and the
//! row encodings of `Sources/StenoCore/Storage/Records.swift`.

mod audio;
mod delivery;
mod derived_uuid;
mod handover;
mod language;
mod llm;
mod meeting;
mod people;
mod settings;
mod stage;
mod summary;
mod tasks;
mod transcript;

pub use audio::{AudioAsset, AudioFormat, AudioRetention, AudioRetentionKind};
pub use delivery::{
    DeliveredFile, Delivery, DeliveryReceipt, DeliveryStatus, DeliveryStatusKind, FileOwnership,
};
pub use derived_uuid::derived_uuid;
pub use handover::{HandoverReceipt, HandoverState, HandoverStateKind, PairedDevice};
pub use language::LanguageTag;
pub use llm::LlmUsage;
pub use meeting::{
    Meeting, MeetingSource, MeetingState, MeetingStateKind, RecordingEndReason,
    RecordingEndReasonKind, TitleOrigin,
};
pub use people::{
    Embedding, Participant, ParticipantRole, Person, Speaker, SpeakerAssignment,
    SpeakerAssignmentKind, SpeakerNameSuggestion, TimeRange,
};
pub use settings::{LlmProvider, ObsidianSettings, Settings};
pub use stage::{PipelineStage, StageRate};
pub use summary::{SummaryBullet, SummaryDocument, SummarySection};
pub use tasks::{Decision, MeetingTask, TaskPriority};
pub use transcript::{AudioLane, TranscriptSegment};
