//! The recorder's state as the shell reads it off the `recording` snapshots
//! that pass through `bridge::emit`: the tray's labels and the floating
//! panels follow it. The shell never asks the host for the state; it
//! observes what the host publishes to the main window, as the Swift menu
//! bar item observes `RecordingController.recording`.

use serde_json::Value;
pub use steno_bridge::RecordingState;

/// What the shell makes of the contract's `RecordingState`.
pub trait RecorderState: Sized {
    /// Reads the state off a `recording` snapshot; `None` when the payload
    /// is not one (so a malformed snapshot changes nothing in the shell).
    fn from_snapshot(payload: &Value) -> Option<Self>;

    /// Anything but idle: the recorder is busy and the bubble shows.
    fn is_busy(&self) -> bool;
}

impl RecorderState for RecordingState {
    fn from_snapshot(payload: &Value) -> Option<Self> {
        payload.get("state")?.as_str()?.parse().ok()
    }

    fn is_busy(&self) -> bool {
        !matches!(self, Self::Idle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recorded_snapshots_read() {
        let live: Value = serde_json::from_str(include_str!(
            "../../../macos/web/fixtures/bridge/recording.live.json"
        ))
        .unwrap();
        assert_eq!(
            RecordingState::from_snapshot(&live),
            Some(RecordingState::Recording)
        );
        for (text, state) in [
            ("idle", RecordingState::Idle),
            ("starting", RecordingState::Starting),
            ("stopping", RecordingState::Stopping),
        ] {
            let payload = serde_json::json!({ "state": text, "deniedPermissions": [] });
            assert_eq!(RecordingState::from_snapshot(&payload), Some(state));
        }
    }

    #[test]
    fn anything_else_is_no_state() {
        assert_eq!(
            RecordingState::from_snapshot(&serde_json::json!({ "state": "paused" })),
            None
        );
        assert_eq!(RecordingState::from_snapshot(&Value::Null), None);
        assert_eq!(
            RecordingState::from_snapshot(&serde_json::json!({ "version": "1" })),
            None
        );
    }

    #[test]
    fn only_idle_is_not_busy() {
        assert!(!RecordingState::Idle.is_busy());
        assert!(RecordingState::Starting.is_busy());
        assert!(RecordingState::Recording.is_busy());
        assert!(RecordingState::Stopping.is_busy());
    }
}
