//! The level stream while the writer thread is stuck in a sync of the
//! files: A20 of `.plans/2026-10-07-stable-promotion.md`. The writer syncs
//! every 5 s of audio, and a sync can take seconds (`FlushFileBuffers` on
//! Windows, a slow or external disk); the levels go out from a thread of
//! their own, so the meter does not freeze meanwhile. The sync is held
//! through [`HeldSyncs`] (behind `test-support`), so the test does not
//! depend on how slow the disk is. Rust only: Swift synced at the close
//! alone.
//!
//! A test binary of its own, so no other test shares the machine with the
//! one claim here that reads wall time: how late a level reaches the
//! consumer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use steno_audio::capture::{CaptureConfiguration, CaptureMode, CaptureSession};
use steno_audio::testing::synthetic::SyntheticOptions;
use steno_audio::testing::{HANG_GUARD, HeldSyncs, SyntheticCaptureBackend};
use steno_audio::writer::writer_thread::SYNC_INTERVAL_FRAMES;
use steno_audio::{FRAMES_PER_SECOND, SystemClock};
use steno_core::AudioLane;
use uuid::Uuid;

/// The processing thread's level cadence: every 10 frames, 100 ms.
const LEVEL_INTERVAL: Duration = Duration::from_millis(100);
/// How late against that cadence a level may reach the consumer.
const LATE: Duration = Duration::from_millis(100);
/// How long the test watches the levels while the sync is held: ten
/// levels' worth.
const WATCHED: Duration = Duration::from_secs(1);

/// While the writer's first sync is held, a level reaches the consumer at
/// least every [`LEVEL_INTERVAL`] plus [`LATE`], each the live tone's, not
/// a stale one; once the sync is let go the recording loses nothing.
/// Publishing the levels from the writer thread again fails it: no level
/// arrives while the sync is held.
#[test]
fn levels_keep_reaching_the_consumer_while_the_writer_is_held_in_a_sync() {
    let directory = tempfile::tempdir().unwrap();
    let configuration = CaptureConfiguration::new(CaptureMode::InPerson, directory.path());
    let lanes = configuration.lanes();
    assert_eq!(lanes, [AudioLane::Mixed]);
    let seconds = 30.0;
    let backend = Arc::new(SyntheticCaptureBackend::new(
        SyntheticOptions::tones(&lanes, &[(AudioLane::Mixed, 440.0)], seconds).real_time(true),
    ));
    let held = HeldSyncs::default();
    let session = CaptureSession::with_writer_factory(
        configuration,
        backend,
        None,
        CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
        Arc::new(SystemClock::new()),
        held.writer_factory(),
    )
    .unwrap();
    let levels = session.levels();
    session.start(Uuid::new_v4()).unwrap();

    // The first sync comes after 5 s of audio, which the real-time
    // backend takes 5 s to deliver.
    assert_eq!(SYNC_INTERVAL_FRAMES, 5 * FRAMES_PER_SECOND);
    let began = held.wait_for_a_held_sync(HANG_GUARD);
    let since = Instant::now();
    let before_the_hold = levels.try_iter().count();
    let mut arrivals = Vec::new();
    if began {
        while let Ok(level) =
            levels.recv_timeout((since + WATCHED).saturating_duration_since(Instant::now()))
        {
            arrivals.push((Instant::now(), level));
        }
    }
    // Let go before any assertion, so a failure does not leave the stop
    // waiting for the writer thread.
    held.release();
    let result = session.stop().unwrap();

    assert!(began, "the writer's sync began");
    assert!(before_the_hold > 0, "levels came before the hold");
    let mut last = since;
    for (at, level) in &arrivals {
        let gap = at.duration_since(last);
        assert!(
            gap < LEVEL_INTERVAL + LATE,
            "no level reached the consumer for {gap:?}, {:?} into the held sync ({} levels in all)",
            last.duration_since(since),
            arrivals.len()
        );
        // A 440 Hz sine at amplitude 0.5 is -9.03 dBFS RMS.
        assert!((level.mic.rms - -9.03).abs() < 0.2, "{level:?}");
        last = *at;
    }
    let tail = (since + WATCHED).saturating_duration_since(last);
    assert!(
        tail < LEVEL_INTERVAL + LATE,
        "the levels stopped {tail:?} before the watch ended ({} levels)",
        arrivals.len()
    );
    assert!(result.failure.is_none(), "{:?}", result.failure);
    assert!(
        result.statistics.dropped_frames.is_empty(),
        "{:?}",
        result.statistics.dropped_frames
    );
}
