//! Speech to text for one lane.

use std::collections::BTreeSet;

use async_trait::async_trait;

use super::BoundaryResult;
use crate::{AudioBuffer16k, LanguageTag, RawSegment};

/// Speech to text for one lane. `id` and `supported_languages` are fixed for
/// the engine's lifetime.
#[async_trait]
pub trait SpeechEngine: Send + Sync {
    fn id(&self) -> &str;

    fn supported_languages(&self) -> &BTreeSet<LanguageTag>;

    /// Downloads or loads models; idempotent. Concurrent calls are serialised by
    /// the pipeline.
    async fn prepare(&self) -> BoundaryResult<()>;

    /// `hint` is the other lane's dominant language, or `None` for the
    /// first lane.
    async fn transcribe(
        &self,
        audio: &AudioBuffer16k,
        hint: Option<&LanguageTag>,
    ) -> BoundaryResult<Vec<RawSegment>>;

    /// Frees what `prepare` loaded; the next `prepare` or `transcribe` loads
    /// again. The in-process engines keep this default so the next job starts
    /// warm; an engine that wraps another forwards the call.
    async fn release(&self) -> BoundaryResult<()> {
        Ok(())
    }
}
