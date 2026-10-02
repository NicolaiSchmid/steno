//! The domain types, one for one with `Sources/StenoCore/Model` and the
//! row encodings of `Sources/StenoCore/Storage/Records.swift`, plus the
//! value types the pipeline boundaries exchange (`speech`, `stage_io`,
//! `template`; the speech crate's intermediates come along from
//! `Sources/StenoSpeech`).

mod audio;
mod audio_buffer;
mod delivery;
mod derived_uuid;
mod handover;
mod language;
mod llm;
mod meeting;
mod people;
mod settings;
mod speech;
mod stage;
mod stage_io;
mod summary;
mod tasks;
mod template;
mod transcript;

pub use audio::{AudioAsset, AudioFormat, AudioRetention, AudioRetentionKind};
pub use audio_buffer::AudioBuffer16k;
pub use delivery::{
    DeliveredFile, Delivery, DeliveryReceipt, DeliveryStatus, DeliveryStatusKind, FileOwnership,
};
pub use derived_uuid::derived_uuid;
pub use handover::{
    HandoverReceipt, HandoverState, HandoverStateKind, PairedDevice, RecordingMetadata,
};
pub use language::LanguageTag;
pub use llm::{
    LlmFinishReason, LlmMessage, LlmRequest, LlmResponse, LlmResponseFormat, LlmResponseFormatKind,
    LlmRole, LlmUsage,
};
pub use meeting::{
    Meeting, MeetingSource, MeetingState, MeetingStateKind, RecordingEndReason,
    RecordingEndReasonKind, TitleOrigin,
};
pub use people::{
    Embedding, Participant, ParticipantRole, Person, Speaker, SpeakerAssignment,
    SpeakerAssignmentKind, SpeakerMatch, SpeakerNameSuggestion, TimeRange,
};
pub use settings::{LlmProvider, ObsidianSettings, Settings};
pub use speech::{
    ClusterChunk, DiarizationResult, RawSegment, SpeakerCluster, SpeakerTurn, TimedWord, WordTiming,
};
pub use stage::{PipelineStage, StageRate};
pub use stage_io::{CleanupInput, CleanupOutput, MeetingExport, SummaryInput, SummaryOutput};
pub use summary::{SummaryBullet, SummaryDocument, SummarySection};
pub use tasks::{Decision, MeetingTask, TaskPriority};
pub use template::{SummaryTemplate, TemplateSection};
pub use transcript::{AudioLane, TranscriptSegment};
