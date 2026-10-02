//! The post-meeting pipeline of the Rust port: `process` with the Swift
//! stage order and semantics, the `progress` events the host shows, the
//! learned stage rates, the two recording intakes and the retention sweep.
//! Ported from `Sources/StenoCore/Pipeline/`, `Storage/RecordingIntake.swift`,
//! `Storage/LocalRecordingIntake.swift` and `Storage/RetentionSweep.swift`.
//! Plan: `.plans/2026-10-02-rust-core-and-tauri-shell.md`, `WP6b`.
//!
//! The crate sits above `steno-core` and below the services assembly: it
//! knows the boundaries (`steno_core::protocols`) and the store, never an
//! engine. The Swift package kept this in `StenoCore`; here it is its own
//! crate because it needs an async runtime (`tokio`) that the core, which
//! every crate depends on, must not pull in, and because the host crate
//! (`WP6a`) already set the precedent of one crate per layer above the core.
//!
//! One operation runs per meeting at a time; stage durations feed the
//! `stageRate` table only when the meeting was alone in flight; every
//! stage attributes its own errors and `process` marks the meeting failed
//! in one place. See [`ProcessingPipeline`].

pub mod estimator;
pub mod events;
pub mod fixtures;
pub mod intake;
pub mod lane_merger;
pub mod pipeline;
pub mod retention;
pub mod run;

pub use estimator::{ProcessingEstimator, StageRates, StageSample};
pub use events::{EventReceiver, MeetingEventBus};
pub use intake::{
    Attendee, LocalRecordingIntake, LocalRecordingIntakeError, RecordingIntake, RecordingResult,
};
pub use lane_merger::LaneMerger;
pub use pipeline::{
    MonotonicClock, Now, PipelineDependencies, PipelineFailure, ProcessingPipeline, SystemClock,
};
pub use retention::{RetentionSweep, SweepIncomplete};
pub use steno_core::{MeetingEvent, PipelineStage, ProcessingProgress};
