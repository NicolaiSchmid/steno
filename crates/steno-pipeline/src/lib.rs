//! The post-meeting pipeline: [`ProcessingPipeline`] runs a meeting's
//! stages, posts its progress, learns stage rates, and owns the phone and
//! local recording intakes and the retention sweep. It sits above
//! `steno-core` and knows the boundaries and the store, never an engine.
//! Async store calls go through the store's facade, on the blocking pool.
//!
//! A pipeline over core's fakes, as the CLI runs it without `--engine`:
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use steno_core::testing::{FakeDiarizer, FakeSpeechEngine, InMemorySpeakerMemory};
//! use steno_core::{AudioAsset, AudioDecoder, DeliveryDispatcher, Meeting, Store};
//! use steno_pipeline::{MeetingEventBus, PipelineDependencies, ProcessingPipeline};
//!
//! # async fn run(
//! #     decoder: Arc<dyn AudioDecoder>,
//! #     dispatcher: Arc<dyn DeliveryDispatcher>,
//! #     meeting: Meeting,
//! #     asset: AudioAsset,
//! # ) -> Result<(), Box<dyn std::error::Error>> {
//! let store = Arc::new(Store::open("steno.sqlite")?);
//! let dependencies = PipelineDependencies::new(
//!     decoder,
//!     Arc::new(FakeSpeechEngine::default()),
//!     Arc::new(FakeDiarizer::default()),
//!     Arc::new(InMemorySpeakerMemory::new(store.persons()?)),
//!     dispatcher,
//!     store.clone(),
//!     MeetingEventBus::new(),
//! );
//! let pipeline = ProcessingPipeline::new(dependencies);
//! pipeline.enqueue(&meeting, &asset)?;
//! pipeline.wait_until_idle().await;
//! # Ok(())
//! # }
//! ```

#![deny(unsafe_code)]

pub mod estimator;
pub mod events;
pub mod files;
pub mod fixtures;
pub mod intake;
pub mod lane_merger;
pub mod live_recording;
pub mod pipeline;
pub mod retention;
pub mod run;
pub mod sample_clips;
pub mod speaker_memory;

pub use estimator::{ProcessingEstimator, StageRates, StageSample};
pub use events::{EventReceiver, MeetingEventBus};
pub use intake::{
    AdmissionFolders, Attendee, LocalRecordingIntake, LocalRecordingIntakeError, NotedUpload,
    RecordingIntake, RecordingResult,
};
pub use lane_merger::LaneMerger;
pub use live_recording::LiveRecording;
pub use pipeline::{
    BACKGROUND_RUN_LOG, CLOSED_WHILE_PROCESSING, FailureKind, InFlight, JobHold, ModelWaits,
    MonotonicClock, Now, OPERATION_PANICKED, Operation, PipelineDependencies, PipelineFailure,
    ProcessingPipeline, QuitLatch, REDELIVER_AFTER, ReprocessError, SharedSpeechEngine,
    SystemClock, WeakSpeechEngine,
};
pub use retention::{RetentionSweep, SweepIncomplete};
pub use sample_clips::{ClipProbe, ClipStep};
pub use speaker_memory::StoreSpeakerMemory;
pub use steno_core::{MeetingEvent, MeetingOperation, PipelineStage, ProcessingProgress};
