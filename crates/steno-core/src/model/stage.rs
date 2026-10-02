//! The pipeline's stages and the learned rate per stage.
//! Swift: `Sources/StenoCore/Storage/Records.swift` and
//! `Sources/StenoCore/Storage/MeetingStore+Timings.swift`.

use serde::{Deserialize, Serialize};

use crate::string_enum;

string_enum! {
    /// The pipeline's stages in execution order.
    pub enum PipelineStage {
        Decode = "decode",
        Transcribe = "transcribe",
        Diarize = "diarize",
        MatchSpeakers = "matchSpeakers",
        Merge = "merge",
        Cleanup = "cleanup",
        Summarize = "summarize",
        Persist = "persist",
        Deliver = "deliver",
        Retention = "retention",
    }
}

/// One learned rate of the `stageRate` table: seconds per unit of work and
/// how many measurements it averages.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageRate {
    pub seconds_per_unit: f64,
    pub samples: i64,
}
