//! One process the HAL knows about and whether it currently has an input
//! (microphone) stream running, and the seam the detector reads it through.
//! Swift: `Sources/StenoAudio/Detection/ProcessAudioActivity.swift`.

use std::sync::mpsc::Receiver;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProcessAudioActivity {
    pub pid: i32,
    pub bundle_id: Option<String>,
    pub is_running_input: bool,
    pub is_running_output: bool,
}

impl ProcessAudioActivity {
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActivityError {
    #[error("process audio activity unavailable: {0}")]
    Failed(String),
}

/// The seam under [`MeetingDetector`](super::MeetingDetector): a snapshot
/// of every process's microphone state, plus a channel that fires whenever
/// the snapshot may have changed. [`LiveProcessAudioActivity`](super::LiveProcessAudioActivity)
/// reads the HAL; [`FakeProcessAudioActivity`](crate::testing::FakeProcessAudioActivity)
/// is scripted.
pub trait ProcessAudioActivitySource: Send + Sync {
    fn snapshot(&self) -> Result<Vec<ProcessAudioActivity>, ActivityError>;

    /// One message per HAL notification (`DeviceIsRunningSomewhere` on an
    /// input device, the process list, the device list). Payload-free: the
    /// detector re-reads `snapshot()`. The registration lives as long as
    /// the source.
    fn changes(&self) -> Receiver<()>;
}
