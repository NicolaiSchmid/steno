//! Who spoke when, for one lane.
//! Swift: `Sources/StenoCore/Protocols/Diarizer.swift`.

use async_trait::async_trait;

use super::BoundaryResult;
use crate::{AudioBuffer16k, DiarizationResult};

/// Speaker diarization of one lane. Clusters carry ranges, an embedding,
/// the cluster confidence and the diarizer-chosen sample clip range.
#[async_trait]
pub trait Diarizer: Send + Sync {
    /// Downloads or loads models; idempotent like
    /// [`SpeechEngine::prepare`](super::SpeechEngine::prepare).
    async fn prepare(&self) -> BoundaryResult<()>;

    async fn diarize(&self, audio: &AudioBuffer16k) -> BoundaryResult<DiarizationResult>;
}
