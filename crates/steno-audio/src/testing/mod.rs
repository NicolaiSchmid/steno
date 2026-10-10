//! Test doubles and fixtures that ship in the library so other crates (the
//! pipeline, the CLI, the shell) test against them: deterministic 48 kHz
//! signals, the synthetic capture backend, a scripted process-activity
//! source, a manual clock and the counting allocator behind the real-time
//! proof. Swift: `Sources/StenoAudio/Testing/`.
//!
//! Not feature-gated, unlike `steno-core`'s `testing`: the synthetic
//! backend is the production path's test double on Linux CI and the
//! shell's capture source where no HAL exists, and nothing here pulls in
//! a dependency the crate does not already have.

pub mod fake_activity;
pub mod fixtures;
pub mod manual_clock;
pub mod rt;
pub mod synthetic;

pub use fake_activity::FakeProcessAudioActivity;
pub use fixtures::{AudioFixtures, SplitMix64};
pub use manual_clock::ManualClock;
pub use synthetic::{SyntheticCaptureBackend, SyntheticEcho, SyntheticLane};

/// Whether a backend's `start` or `stop` on this thread logs its own lines
/// at `debug`: inside the session's restarts once their streak logged a
/// failure (`capture::start_log`). For a test backend that logs as the
/// live ones do, so that a test counting the capture's lines sees a start
/// or a stop the session left loud. Rust only.
#[must_use]
pub fn start_log_is_quiet() -> bool {
    crate::capture::start_log::is_quiet()
}
