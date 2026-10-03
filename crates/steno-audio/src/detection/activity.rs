//! One process the HAL knows about and whether it currently has an input
//! (microphone) stream running, and the seam the detector reads it through.
//! Swift: `Sources/StenoAudio/Detection/ProcessAudioActivity.swift`.

use std::sync::mpsc::Receiver;

/// One process the HAL knows and its microphone state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProcessAudioActivity {
    /// The process id.
    pub pid: i32,
    /// Its bundle identifier, when the HAL knows one.
    pub bundle_id: Option<String>,
    /// It has an input (microphone) stream running.
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
/// the snapshot may have changed. [`LiveProcessAudioActivity`](super::LiveProcessAudioActivity)
/// reads the HAL; [`FakeProcessAudioActivity`](crate::testing::FakeProcessAudioActivity)
/// is scripted.
pub trait ProcessAudioActivitySource: Send + Sync {
    /// Every process the HAL lists, with its current microphone state.
    fn snapshot(&self) -> Result<Vec<ProcessAudioActivity>, ActivityError>;

    /// One message per HAL notification (`DeviceIsRunningSomewhere` on an
    /// input device, the process list, the device list). Payload-free: the
    /// detector re-reads `snapshot()`. The registration lives as long as
    /// the source.
    fn changes(&self) -> Receiver<()>;
}
