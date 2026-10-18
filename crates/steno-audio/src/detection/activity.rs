//! One process's microphone state and the seam the detector reads it
//! through.

use std::sync::mpsc::Receiver;

/// One process the HAL knows and its microphone state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProcessAudioActivity {
    pub pid: i32,
    pub bundle_id: Option<String>,
    pub is_running_input: bool,
    /// It has an output stream running; not used for detection.
    pub is_running_output: bool,
}

impl ProcessAudioActivity {
    /// Output not running.
    #[must_use]
    pub fn new(pid: i32, bundle_id: Option<&str>, is_running_input: bool) -> Self {
        Self {
            pid,
            bundle_id: bundle_id.map(str::to_owned),
            is_running_input,
            is_running_output: false,
        }
    }
}

/// The source could not read the HAL.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActivityError {
    /// The HAL call that failed, as text.
    #[error("process audio activity unavailable: {0}")]
    Failed(String),
}

/// The seam under [`MeetingDetector`](super::MeetingDetector): a snapshot
/// of every process's microphone state, plus a channel that fires whenever
/// it may have changed.
pub trait ProcessAudioActivitySource: Send + Sync {
    fn snapshot(&self) -> Result<Vec<ProcessAudioActivity>, ActivityError>;

    /// One payload-free message per notification; the detector re-reads
    /// `snapshot()`. The registration lives as long as the source.
    fn changes(&self) -> Receiver<()>;
}
