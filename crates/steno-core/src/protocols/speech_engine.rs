//! Speech to text for one lane.
//! Swift: `Sources/StenoCore/Protocols/SpeechEngine.swift`.

use std::collections::BTreeSet;

use async_trait::async_trait;

use super::BoundaryResult;
use crate::{AudioBuffer16k, LanguageTag, RawSegment};

/// Speech to text behind one trait; the speech crate's engines implement
/// it. `id` and `supported_languages` are fixed for the engine's lifetime.
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

    /// Frees what `prepare` loaded; the next `prepare` or `transcribe` loads
    /// again. The pipeline is to call it once a job's lanes are transcribed
    /// (WP6b; nothing calls it yet). Only `SidecarSpeechEngine` overrides it,
    /// by stopping its child, and a call there waits for a running request
    /// (not for a model download).
    /// The in-process engines (`OnnxSpeechEngine`, `CoreMlParakeetEngine`)
    /// keep this default and their models stay loaded, so the Mac's next job
    /// starts warm. Rust only: Swift's protocol has no counterpart.
    async fn release(&self) -> BoundaryResult<()> {
        Ok(())
    }
}
