//! Whether a recording is under way, as the jobs see it. An abort in an
//! in-process speech engine ends the app and the recording, so while one
//! runs a job waits before its next in-process speech call
//! ([`MeetingEvent::WaitingForRecording`](steno_core::MeetingEvent::WaitingForRecording)).
//! Sidecar jobs do not wait.

use std::sync::Arc;

use tokio::sync::watch;

/// Whether a recording is under way; clones share it. Never live from
/// [`Default`].
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
    /// The recorder's state changed: `live` from the start of a recording to
    /// the end of its stop.
    pub fn set(&self, live: bool) {
        self.0
            .send_if_modified(|now| std::mem::replace(now, live) != live);
    }

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
