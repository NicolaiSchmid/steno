//! Test doubles and fixtures that ship in the library so other crates test
//! against them: deterministic 48 kHz signals, the synthetic capture
//! backend, a scripted process-activity source, a manual clock and the
//! counting allocator. Only what a test holds and releases (held outages,
//! `HeldSyncs`) sits behind `test-support`.

use std::time::Duration;

pub mod fake_activity;
pub mod fixtures;
#[cfg(feature = "test-support")]
pub mod held_syncs;
pub mod manual_clock;
pub mod rt;
pub mod synthetic;

pub use fake_activity::FakeProcessAudioActivity;
pub use fixtures::{AudioFixtures, SplitMix64};
#[cfg(feature = "test-support")]
pub use held_syncs::HeldSyncs;
pub use manual_clock::ManualClock;
pub use synthetic::{SyntheticCaptureBackend, SyntheticEcho, SyntheticLane};

/// How long a test waits for real work before it fails as hung. It only
/// turns a hang into a failure, so no verdict rests on it: a claim about
/// when something happens steps a [`ManualClock`] or counts.
/// `steno_services::testing::HANG_GUARD` takes this value.
pub const HANG_GUARD: Duration = Duration::from_secs(120);

/// Whether a backend's `start` or `stop` on this thread logs its own lines
/// at `debug`, inside the session's quiet restarts; for a test backend
/// that logs as the live ones do.
#[must_use]
pub fn start_log_is_quiet() -> bool {
    crate::capture::start_log::is_quiet()
}
