//! Speech to text for one lane.
//! Swift: `Sources/StenoCore/Protocols/SpeechEngine.swift`.

use std::collections::BTreeSet;

use async_trait::async_trait;

use super::BoundaryResult;
use crate::{AudioBuffer16k, LanguageTag, RawSegment};

/// Speech-to-text behind one trait; v1 ships `parakeet-v3` and
/// `whisperkit-large-v3-turbo`. `id` and `supported_languages` are fixed
/// for the engine's lifetime.
#[async_trait]
pub trait SpeechEngine: Send + Sync {
    fn id(&self) -> &str;

    fn supported_languages(&self) -> &BTreeSet<LanguageTag>;

    /// Downloads or loads models. Called before the first `transcribe` and
    /// again by later runs and warm-ups: idempotent, a call on a loaded
    /// engine returns at once. Concurrent calls are serialised by the
    /// pipeline.
    async fn prepare(&self) -> BoundaryResult<()>;

    /// `hint` is the other lane's dominant language, or `None` for the
    /// first lane.
    async fn transcribe(
        &self,
        audio: &AudioBuffer16k,
        hint: Option<&LanguageTag>,
    ) -> BoundaryResult<Vec<RawSegment>>;
}
