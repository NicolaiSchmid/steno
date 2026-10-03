//! What the unit tests share: a store in a temp directory and the
//! pipeline's dependencies over core's fakes.

use std::sync::Arc;

use steno_adapters::DeliveryCoordinator;
use steno_audio::SymphoniaAudioCodec;
use steno_core::Store;
use steno_core::testing::{FakeDiarizer, FakeSpeechEngine, InMemorySpeakerMemory};
use steno_pipeline::{MeetingEventBus, PipelineDependencies};

/// A fresh store; the directory lives as long as the guard.
pub fn temp_store() -> (tempfile::TempDir, Arc<Store>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path().join("steno.sqlite")).unwrap());
    (dir, store)
}

/// Fakes over `store`, the engine named `engine_id` so a run shows which
/// pipeline it went through.
pub fn fake_dependencies(store: &Arc<Store>, engine_id: &str) -> PipelineDependencies {
    PipelineDependencies::new(
        Arc::new(SymphoniaAudioCodec::new()),
        Arc::new(FakeSpeechEngine {
            id: engine_id.to_owned(),
            ..FakeSpeechEngine::default()
        }),
        Arc::new(FakeDiarizer::default()),
        Arc::new(InMemorySpeakerMemory::new(Vec::new())),
        Arc::new(DeliveryCoordinator::new(store.clone())),
        store.clone(),
        MeetingEventBus::new(),
    )
}
