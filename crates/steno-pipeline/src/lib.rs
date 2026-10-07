//! The post-meeting pipeline of the Rust port: `process` with the Swift
//! stage order and semantics, the `progress` events the host shows, the
//! learned stage rates, the two recording intakes and the retention sweep.
//! Ported from `Sources/StenoCore/Pipeline/`, `Storage/RecordingIntake.swift`,
//! `Storage/LocalRecordingIntake.swift` and `Storage/RetentionSweep.swift`.
//! Plan: `.plans/2026-10-02-rust-core-and-tauri-shell.md` (`WP6b`).
//!
//! | Module | What it holds |
//! |--------|---------------|
//! | [`pipeline`] | [`ProcessingPipeline`]: `enqueue`, `process`, `rerun_summary` and `redeliver` (with their `claim_` halves), `warm_up` and `warm_up_diarizer`, the speech engine's release after a job's lanes (its claims in [`SharedSpeechEngine`], found again through [`WeakSpeechEngine`]), `quit` and its [`QuitLatch`] for the app's exit, `apply_retention`, the stages |
//! | [`estimator`] | The learned stage rates, their seeds and the arithmetic behind `progress` |
//! | [`run`] | One run's progress state with the monotonic clamp |
//! | [`events`] | [`MeetingEventBus`], the broadcast of `MeetingEvent` |
//! | [`intake`] | The phone intake ([`RecordingIntake`]) and the Mac one ([`LocalRecordingIntake`]) |
//! | [`lane_merger`] | The lanes into one ordered transcript |
//! | [`speaker_memory`] | The store-backed cosine `SpeakerMemory` |
//! | [`retention`] | [`RetentionSweep`] over expired assets |
//! | [`fixtures`] | The synthetic audio fixtures and the WAV writer |
//! | [`files`] | Durable writes: a file replaced in one step, a recording copied, new folders |
//!
//! The crate sits above `steno-core` and below the services assembly: it
//! knows the boundaries (`steno_core::protocols`) and the store, never an
//! engine. The Swift package kept this in `StenoCore`; here it is its own
//! crate because it needs an async runtime (`tokio`) that the core, which
//! every crate depends on, must not pull in, and because the host crate
//! already set the precedent of one crate per layer above the core.
//!
//! One operation runs per meeting at a time; stage durations feed the
//! `stageRate` table only when the meeting was alone in flight; every
//! stage attributes its own errors and `process` marks the meeting failed
//! in one place. A pipeline over core's fakes, as the CLI runs it without
//! `--engine`:
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

pub mod estimator;
pub mod events;
pub mod files;
pub mod fixtures;
pub mod intake;
pub mod lane_merger;
pub mod pipeline;
pub mod retention;
pub mod run;
pub mod speaker_memory;

pub use estimator::{ProcessingEstimator, StageRates, StageSample};
pub use events::{EventReceiver, MeetingEventBus};
pub use intake::{
    Attendee, LocalRecordingIntake, LocalRecordingIntakeError, RecordingIntake, RecordingResult,
};
pub use lane_merger::LaneMerger;
pub use pipeline::{
    BACKGROUND_RUN_LOG, MonotonicClock, Now, OPERATION_PANICKED, Operation, PipelineDependencies,
    PipelineFailure, ProcessingPipeline, QuitLatch, SharedSpeechEngine, SystemClock,
    WeakSpeechEngine,
};
pub use retention::{RetentionSweep, SweepIncomplete};
pub use speaker_memory::StoreSpeakerMemory;
pub use steno_core::{MeetingEvent, MeetingOperation, PipelineStage, ProcessingProgress};
