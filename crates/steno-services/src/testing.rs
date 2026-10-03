//! What the unit tests share: a store in a temp directory, the
//! pipeline's dependencies over core's fakes, and the waits that fail a
//! test instead of hanging it.

use std::sync::Arc;
use std::time::Duration;

use steno_adapters::DeliveryCoordinator;
use steno_audio::SymphoniaAudioCodec;
use steno_core::Store;
use steno_core::testing::{FakeDiarizer, FakeSpeechEngine, InMemorySpeakerMemory};
use steno_pipeline::{MeetingEventBus, PipelineDependencies, ProcessingPipeline};

use crate::pipeline::{CurrentPipeline, MakeDependencies};

/// How long a test waits for background work before it fails.
pub const PATIENCE: Duration = Duration::from_secs(5);

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

/// A pipeline on the current runtime whose reload builds `dependencies`
/// again.
pub fn current_pipeline(dependencies: PipelineDependencies) -> Arc<CurrentPipeline> {
    let make: MakeDependencies = {
        let dependencies = dependencies.clone();
        Arc::new(move || Ok(dependencies.clone()))
    };
    Arc::new(CurrentPipeline::new(
        ProcessingPipeline::new(dependencies),
        make,
        tokio::runtime::Handle::current(),
    ))
}

/// Waits until `done` holds, failing the test with `what` after
/// [`PATIENCE`].
pub async fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    tokio::time::timeout(PATIENCE, async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect(what);
}

/// Calls `f` on a thread of its own, as the host calls its services,
/// failing the test with `what` unless it returns within `patience`. Not
/// a `spawn_blocking` task: the runtime waits for those when it shuts
/// down, so a call stuck on the work would hang the test instead of
/// failing it.
pub fn on_own_thread<T: Send + 'static>(
    patience: Duration,
    what: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(f());
    });
    match receiver.recv_timeout(patience) {
        Ok(value) => value,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!("{what}"),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => panic!("the call panicked"),
    }
}

/// Log lines written into a buffer at `warn` and above while the guard
/// lives, on this thread.
#[derive(Clone, Default)]
pub struct CapturedLog(Arc<std::sync::Mutex<Vec<u8>>>);

impl CapturedLog {
    /// Starts capturing on this thread.
    pub fn warnings() -> (Self, tracing::subscriber::DefaultGuard) {
        let log = CapturedLog::default();
        let guard = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_writer(log.clone())
                .with_max_level(tracing::Level::WARN)
                .finish(),
        );
        (log, guard)
    }

    pub fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl std::io::Write for CapturedLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLog {
    type Writer = CapturedLog;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
