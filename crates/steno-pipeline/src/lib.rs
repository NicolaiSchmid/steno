//! The post-meeting pipeline of the Rust port: `process` with the Swift
//! stage order and semantics, the `progress` events the host shows, the
//! learned stage rates, the two recording intakes and the retention sweep.
//! Ported from `Sources/StenoCore/Pipeline/`, `Storage/RecordingIntake.swift`,
//! `Storage/LocalRecordingIntake.swift` and `Storage/RetentionSweep.swift`.
//! Plan: `.plans/2026-10-02-rust-core-and-tauri-shell.md` (`WP6b`).
//!
//! | Module | What it holds |
//! |--------|---------------|
//! | [`pipeline`] | [`ProcessingPipeline`]: `enqueue` and `enqueue_saved`, `reprocess` (refused with a [`ReprocessError`]), `process`, `rerun_summary` and `redeliver` (with their `claim_` halves), `resume_unfinished` and `redeliver_unfinished` for the launch, the in-flight set ([`InFlight`]) a reload shares, `warm_up` and `warm_up_diarizer`, the speech engine's release after a job's lanes (its claims in [`SharedSpeechEngine`], found again through [`WeakSpeechEngine`]), `quit` and its [`QuitLatch`] for the app's exit, [`PipelineFailure`] and its [`FailureKind`], a run refused for missing models and [`ModelWaits`] with `resume_waiting` for it, `apply_retention`, the stages |
//! | [`damaged_audio`] | [`DamagedAudio`], what of each meeting's recording the decoder replaced by silence (the parts and their seconds), in `damaged-audio.json` |
//! | [`export_retries`] | [`ExportRetries`], the launch re-exports in a row per meeting that did not deliver every row, in `export-retries.json` |
//! | [`estimator`] | The learned stage rates, their seeds and the arithmetic behind `progress` |
//! | [`run`] | One run's progress state with the monotonic clamp |
//! | [`crash_loop`] | Launch recovery's guard against a crash loop: the runs that ended with the app, counted in the meeting's folder |
//! | [`events`] | [`MeetingEventBus`], the broadcast of `MeetingEvent` |
//! | [`intake`] | The phone intake ([`RecordingIntake`], which notes its copies' folders in [`AdmissionFolders`]) and the Mac one ([`LocalRecordingIntake`]) |
//! | [`lane_merger`] | The lanes into one ordered transcript |
//! | [`speaker_memory`] | The store-backed cosine `SpeakerMemory` |
//! | [`retention`] | [`RetentionSweep`] over expired assets |
//! | [`sample_clips`] | The speakers' sample clips written under each run's own names, named by the merge, and the files of the meeting's speakers that no row names swept after it |
//! | [`fixtures`] | The synthetic audio fixtures and the WAV writer |
//! | [`files`] | Durable writes: a file replaced in one step, a recording copied, new folders, a JSON file read and written, set aside when corrupt; its Windows module is the one place in the crate allowed `unsafe` |
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
//! in one place, or leaves it `queued` when a model is missing, until an
//! install, a reload that finds the models, or the launch resumes it
//! ([`ModelWaits`]). A pipeline over core's fakes, as the CLI runs it
//! without `--engine`:
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

pub mod crash_loop;
pub mod damaged_audio;
pub mod estimator;
pub mod events;
pub mod export_retries;
pub mod files;
pub mod fixtures;
pub mod intake;
pub mod lane_merger;
pub mod pipeline;
pub mod retention;
pub mod run;
pub mod sample_clips;
pub mod speaker_memory;

pub use damaged_audio::DamagedAudio;
pub use estimator::{ProcessingEstimator, StageRates, StageSample};
pub use events::{EventReceiver, MeetingEventBus};
pub use export_retries::ExportRetries;
pub use intake::{
    AdmissionFolders, Attendee, LocalRecordingIntake, LocalRecordingIntakeError, RecordingIntake,
    RecordingResult,
};
pub use lane_merger::LaneMerger;
pub use pipeline::{
    BACKGROUND_RUN_LOG, FailureKind, InFlight, ModelWaits, MonotonicClock, Now, OPERATION_PANICKED,
    Operation, PipelineDependencies, PipelineFailure, ProcessingPipeline, QuitLatch,
    ReprocessError, SharedSpeechEngine, SystemClock, WeakSpeechEngine,
};
pub use retention::{RetentionSweep, SweepIncomplete};
pub use sample_clips::{ClipProbe, ClipStep};
pub use speaker_memory::StoreSpeakerMemory;
pub use steno_core::{MeetingEvent, MeetingOperation, PipelineStage, ProcessingProgress};
