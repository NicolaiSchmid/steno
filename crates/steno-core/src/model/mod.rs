//! The domain types, one for one with `Sources/StenoCore/Model` and the
//! row encodings of `Sources/StenoCore/Storage/Records.swift`.

use thiserror::Error;

/// A case name neither Swift nor Rust knows; an unreadable row fails the
/// fetch instead of becoming a default case.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown {type_name} case {value:?}")]
pub struct UnknownCase {
    pub type_name: &'static str,
    pub value: String,
}

/// A string-backed enum: the case names Swift writes to JSON and to the
/// database, with `as_str`, `FromStr`, `Display` and serde in that form.
macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident = $text:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        $vis enum $name {
            $( $(#[$vmeta])* $variant ),+
        }

        impl $name {
            /// The case name Swift writes to JSON and to the database.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }
        }

        impl ::core::str::FromStr for $name {
            type Err = $crate::model::UnknownCase;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                match text {
                    $($text => Ok($name::$variant),)+
                    other => Err($crate::model::UnknownCase {
                        type_name: stringify!($name),
                        value: other.to_owned(),
                    }),
                }
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = <String as ::serde::Deserialize>::deserialize(deserializer)?;
                text.parse().map_err(::serde::de::Error::custom)
            }
        }
    };
}

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
