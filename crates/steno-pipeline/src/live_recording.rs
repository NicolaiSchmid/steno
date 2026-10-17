//! Whether a recording is under way, as the jobs see it (A19 of
//! `.plans/2026-10-07-stable-promotion.md`). An in-process speech engine
//! (`CoreML` on the Mac) runs in the app's process, so an abort inside it
//! that no `@catch` stops ends the app, and a recording under way with it:
//! everything recorded after the abort is lost (D3). So while a recording
//! starts, runs or stops, a job loads and runs no in-process speech engine.
//! It waits at its next call, its meeting saying "Waiting for the
//! recording to finish" ([`MeetingEvent::WaitingForRecording`]), and goes
//! on once the recorder is idle. The speech sidecar runs in a child, whose
//! abort ends only the child, so its jobs do not wait. Rust only: the
//! Swift pipeline ran `CoreML` beside a recording.
//!
//! - **What waits.** Each in-process speech call of a job: the load at
//!   its start and each lane's transcription. A call under way when a
//!   recording starts runs to its end; the job's next call waits. The
//!   recorder's own warm-up while it records
//!   ([`ProcessingPipeline::warm_up_while_recording`]) does not wait: it is
//!   guarded by the [`WarmUpMark`](crate::WarmUpMark) instead.
//! - **The recorder's side.** The services' recorder sets
//!   [`LiveRecording::set`] on every change of its state: live from
//!   `Starting` to `Idle`. The pipeline reads its state, never the
//!   recorder.
//!
//! [`MeetingEvent::WaitingForRecording`]: steno_core::MeetingEvent::WaitingForRecording
//! [`ProcessingPipeline::warm_up_while_recording`]: crate::ProcessingPipeline::warm_up_while_recording

use std::sync::Arc;

use tokio::sync::watch;

/// Whether a recording is under way; clones share it. Never live from
/// [`Default`], as for the `steno` command and the tests without a
/// recorder.
///
/// ```
/// use steno_pipeline::LiveRecording;
///
/// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
/// let live = LiveRecording::default();
/// let recorder = live.clone();
/// recorder.set(true);
/// assert!(live.is_live());
/// let waiting = tokio::spawn({
///     let live = live.clone();
///     async move { live.until_idle().await }
/// });
/// recorder.set(false);
/// waiting.await.unwrap();
/// # });
/// ```
#[derive(Debug, Clone)]
pub struct LiveRecording(Arc<watch::Sender<bool>>);

impl Default for LiveRecording {
    fn default() -> Self {
        LiveRecording(Arc::new(watch::Sender::new(false)))
    }
}

impl LiveRecording {
    /// The recorder's state changed: `live` from the start of a recording
    /// to the end of its stop.
    pub fn set(&self, live: bool) {
        self.0
            .send_if_modified(|now| std::mem::replace(now, live) != live);
    }

    /// Whether a recording is under way now.
    #[must_use]
    pub fn is_live(&self) -> bool {
        *self.0.borrow()
    }

    /// Returns once no recording is under way: at once when none is.
    pub async fn until_idle(&self) {
        let mut receiver = self.0.subscribe();
        // The sender lives in `self`, so the wait cannot end with an error.
        let _ = receiver.wait_for(|live| !live).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_wait_ends_only_once_the_recording_is_over() {
        let live = LiveRecording::default();
        live.until_idle().await;
        live.set(true);
        live.set(true);
        let waiting = tokio::spawn({
            let live = live.clone();
            async move { live.until_idle().await }
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        live.set(false);
        waiting.await.unwrap();
        assert!(!live.is_live());
    }
}
