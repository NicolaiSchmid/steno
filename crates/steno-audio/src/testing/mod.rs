//! Test doubles and fixtures that ship in the library so other crates (the
//! pipeline, the CLI, the shell) test against them: deterministic 48 kHz
//! signals, the synthetic capture backend, a scripted process-activity
//! source, a manual clock and the counting allocator behind the real-time
//! proof. Swift: `Sources/StenoAudio/Testing/`.
//!
//! Not feature-gated, unlike `steno-core`'s `testing`: the synthetic
//! backend is the production path's test double on Linux CI and the
//! shell's capture source where no HAL exists, and nothing here pulls in
//! a dependency the crate does not already have. Only the synthetic
//! backend's held outages are, behind `test-support`: a test releases
//! them, and no build of the shell or the CLI carries them.

use std::time::Duration;

pub mod fake_activity;
pub mod fixtures;
pub mod manual_clock;
pub mod rt;
pub mod synthetic;

pub use fake_activity::FakeProcessAudioActivity;
pub use fixtures::{AudioFixtures, SplitMix64};
pub use manual_clock::ManualClock;
pub use synthetic::{SyntheticCaptureBackend, SyntheticEcho, SyntheticLane};

/// How long a test waits for real work before it fails as hung: a notice,
/// a state, a level or a frame the synthetic producer delivers, a thread
/// that reaches its sleep on a [`ManualClock`], a stop or a finalise that
/// returns. That work takes milliseconds, but it runs on real threads at
/// the host's pace, and a loaded runner has stalled a test binary for
/// longer than ten seconds, so this bound only turns a hang into a
/// failure and no verdict rests on it. A claim about when something
/// happens steps a [`ManualClock`] or counts. Three kinds of claim still
/// read wall time:
///
/// - That nothing arrives (no notice, no restart, no second timer): a
///   short window, which fails only by an extra event, so it stays short.
/// - The capture session tests' `stop_at_the_gate`, which holds the gate
///   for 200 ms so that a `stop()` that does not wait is caught returning;
///   it is a detection window of the same kind.
/// - One measurement outside a measurement test: a capture session test
///   checks that the master is within 0.1 s of the wall time the test
///   ran, which a stall of the test thread before its `stop()` can break.
///
/// `steno_services::testing::HANG_GUARD` takes this value. Rust only.
pub const HANG_GUARD: Duration = Duration::from_secs(120);

/// Whether a backend's `start` or `stop` on this thread logs its own lines
/// at `debug`: inside the session's restarts once their streak logged a
/// failure (`capture::start_log`). For a test backend that logs as the
/// live ones do, so that a test counting the capture's lines sees a start
/// or a stop the session left loud. Rust only.
#[must_use]
pub fn start_log_is_quiet() -> bool {
    crate::capture::start_log::is_quiet()
}
