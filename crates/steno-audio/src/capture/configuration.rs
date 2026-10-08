//! The value types around a capture: mode, configuration, errors, states,
//! levels, notices and statistics.
//! Swift: `Sources/StenoAudio/Capture/CaptureConfiguration.swift`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use steno_core::{AudioAsset, AudioLane};

steno_core::string_enum! {
    /// How a meeting is captured; the raw values are Swift's `CaptureMode`
    /// cases as `Settings` and the bridge spell them.
    pub enum CaptureMode {
        /// Two lanes, `Mic` ("me") and `System` ("them"), with echo
        /// cancellation; the pipeline diarizes the mic lane instead when
        /// the tap stayed silent.
        Call = "call",
        /// One `Mixed` room lane from the microphone: no tap, no echo
        /// cancellation.
        InPerson = "inPerson",
    }
}

impl CaptureMode {
    /// The lanes a session in this mode records, in master channel order.
    #[must_use]
    pub fn lanes(self) -> Vec<AudioLane> {
        match self {
            CaptureMode::Call => vec![AudioLane::Mic, AudioLane::System],
            CaptureMode::InPerson => vec![AudioLane::Mixed],
        }
    }
}

/// What a session records and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureConfiguration {
    /// Call or in person: decides the lanes and the echo cancellation.
    pub mode: CaptureMode,
    /// `None`: the default input device.
    pub input_device_uid: Option<String>,
    /// Default true in `Call`; ignored in `InPerson`.
    pub echo_cancellation: bool,
    /// Debug: writes `mic.raw.caf` (the microphone before echo cancellation)
    /// next to the master.
    pub keep_raw_mic_lane: bool,
    /// The audio folder; the per-meeting folder
    /// (`RecordingLayout::new(audio_folder, meeting_id)`) is created inside.
    pub output_directory: PathBuf,
    /// Developer tools only: record these lanes instead of the mode's
    /// (`[System]` for the Continuity spike). The app never sets it.
    pub lane_override: Option<Vec<AudioLane>>,
}

impl CaptureConfiguration {
    /// The defaults: default input device, echo cancellation on, no raw
    /// microphone lane, no lane override.
    #[must_use]
    pub fn new(mode: CaptureMode, output_directory: impl Into<PathBuf>) -> Self {
        Self {
            mode,
            input_device_uid: None,
            echo_cancellation: true,
            keep_raw_mic_lane: false,
            output_directory: output_directory.into(),
            lane_override: None,
        }
    }

    /// The lanes a session records, in master channel order.
    #[must_use]
    pub fn lanes(&self) -> Vec<AudioLane> {
        self.lane_override
            .clone()
            .unwrap_or_else(|| self.mode.lanes())
    }

    /// Echo cancellation runs only with both a mic and a system lane.
    #[must_use]
    pub fn uses_echo_cancellation(&self) -> bool {
        let lanes = self.lanes();
        self.echo_cancellation
            && lanes.contains(&AudioLane::Mic)
            && lanes.contains(&AudioLane::System)
    }
}

/// Everything a capture can fail with; `Display` is the user-facing text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, thiserror::Error)]
pub enum CaptureError {
    /// A Core Audio call failed: which one, and its `OSStatus` (rendered as
    /// the four-character code when it is one).
    #[error("{operation} failed: {}", four_char_code(*status))]
    CoreAudio {
        /// The HAL function or property.
        operation: String,
        /// The `OSStatus` it returned.
        status: i32,
    },
    /// No input device resolves: none plugged in, or the stored UID is gone.
    #[error("the input device is not available")]
    InputDeviceUnavailable,
    /// No default system output device.
    #[error("the output device is not available")]
    OutputDeviceUnavailable,
    /// The aggregate's input streams did not match the expected lanes.
    #[error("unexpected input stream layout: {0}")]
    UnexpectedStreamLayout(String),
    /// The aggregate runs at a rate the processing thread cannot convert
    /// to [`SAMPLE_RATE`](crate::SAMPLE_RATE) (outside
    /// [`RateConverter::supports`](crate::realtime::RateConverter::supports),
    /// or 0 for an aggregate that is gone). The rate is carried as whole
    /// hertz.
    #[error("the audio devices run at {actual} Hz, which Steno cannot record")]
    UnsupportedSampleRate {
        /// The rate the devices run at, whole hertz.
        actual: u32,
    },
    /// The tap never rose above [`LaneLevel::SILENT_PEAK_LINEAR`] during
    /// the whole session.
    #[error("the system lane stayed silent")]
    SystemAudioSilent,
    /// A device changed or disappeared and the backend could not be
    /// restarted within `CaptureSession::RESTART_ATTEMPTS`.
    #[error("an audio device disappeared")]
    DeviceLost,
    /// A file write failed (its description); the recording is finalised as
    /// far as it got.
    #[error("writing the recording failed: {0}")]
    WriterFailed(String),
    /// The meeting's folder already exists: a start with the id of a
    /// meeting that has one, which never writes over its files. Rust only.
    #[error("the folder {} already exists, so nothing was recorded", .0.display())]
    RecordingExists(PathBuf),
    /// A backend error that is none of the above (its description).
    #[error("capture backend failed: {0}")]
    BackendFailed(String),
    /// The graph did not run: the devices were linked but delivered no
    /// audio before the start's deadline (its description). On Linux,
    /// PipeWire ran no first cycle, as for a source whose owner stalls, a
    /// Bluetooth headset still switching profile, or a sink whose monitor
    /// does not run yet; which node held the graph up is not known. The
    /// session answers it on a chosen microphone by trying the default
    /// input at once. Rust only; reads as [`Self::BackendFailed`].
    #[error("capture backend failed: {0}")]
    DidNotRun(String),
    /// `start` while not idle, `stop` while not recording.
    #[error("{0}")]
    InvalidState(String),
}

/// Renders an `OSStatus` as its four-character code when it is one
/// (`'!obj'`, `'who?'`), else as the number.
#[must_use]
pub(crate) fn four_char_code(status: i32) -> String {
    // The bit pattern is what the four characters are packed in.
    let bytes = (status as u32).to_be_bytes();
    if bytes.iter().all(|b| (0x20..0x7f).contains(b)) {
        format!("'{}'", String::from_utf8_lossy(&bytes))
    } else {
        status.to_string()
    }
}

/// What `stop()` returns: the finished master with its sidecars (retention
/// `KeepForever` until the caller sets it from `Settings`), the session's
/// statistics, and what cut the recording short.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureResult {
    /// The master with its sidecars.
    pub asset: AudioAsset,
    /// Duration, drops, silence and device changes.
    pub statistics: CaptureStatistics,
    /// What ended the recording early or failed its close, the recording
    /// up to it kept: a device that stayed lost, a write or a close that
    /// failed (a full disk). `None` when it ended as asked and closed
    /// cleanly. Rust only: Swift's `stop()` threw the close's failure away.
    pub failure: Option<CaptureError>,
}

/// The session's state machine; see the `session` module doc.
#[derive(Debug, Clone, PartialEq)]
pub enum CaptureState {
    /// Nothing is recording.
    Idle,
    /// `start` is bringing the backend up.
    Starting,
    /// Frames are flowing.
    Recording {
        /// When the backend began delivering, wall time.
        started_at: DateTime<Utc>,
    },
    /// `stop` is tearing down.
    Stopping,
    /// `recording` is `None` when the start produced nothing, the
    /// finalised partial recording when a device stayed lost or the writer
    /// failed mid-meeting, and the whole recording when a close failed or,
    /// with nothing else ending the recording, a sync failed while
    /// recording; `stop()` returns the same value or fails when it is
    /// `None`.
    Failed {
        /// What ended the recording.
        error: CaptureError,
        /// The finalised partial recording, when there is one.
        recording: Option<Box<CaptureResult>>,
    },
}

impl CaptureState {
    /// The failure, when in `Failed`.
    #[must_use]
    pub fn failure(&self) -> Option<&CaptureError> {
        match self {
            CaptureState::Failed { error, .. } => Some(error),
            _ => None,
        }
    }

    /// `idle`, `starting`, `recording`, `stopping` or `failed`.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            CaptureState::Idle => "idle",
            CaptureState::Starting => "starting",
            CaptureState::Recording { .. } => "recording",
            CaptureState::Stopping => "stopping",
            CaptureState::Failed { .. } => "failed",
        }
    }
}

/// RMS and peak of one lane over the last metering window, in dBFS.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LaneLevel {
    /// RMS over the window, dBFS.
    pub rms: f32,
    /// Peak over the window, dBFS.
    pub peak: f32,
}

impl LaneLevel {
    /// Digital silence: the floor every meter reports for zeros.
    pub const SILENCE: LaneLevel = LaneLevel {
        rms: -160.0,
        peak: -160.0,
    };

    /// -80 dBFS, linear: a lane whose peak never exceeds it is "silent" for
    /// [`CaptureStatistics::system_lane_silent`] and the permission probe.
    pub const SILENT_PEAK_LINEAR: f32 = 1e-4;
}

/// Published at 10 Hz; `system` is `None` in `InPerson`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LaneLevels {
    /// The microphone, or the room lane in person.
    pub mic: LaneLevel,
    /// The tap; `None` in person.
    pub system: Option<LaneLevel>,
}

/// What the backend's listener found different after a notification burst
/// settled. The synthetic backend reports `DefaultInputChanged`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceChangeReason {
    /// The default output device moved; the system lane follows it once
    /// the session rebuilt the capture.
    DefaultOutputChanged,
    /// The input the capture would record now moved: the default input,
    /// or a chosen microphone that came back while the default was
    /// recorded in its place.
    DefaultInputChanged,
    /// The output device the capture started on is gone.
    OutputDeviceGone,
    /// The input device the capture started on is gone.
    InputDeviceGone,
    /// The aggregate no longer runs at the rate the capture started at
    /// (a Bluetooth headset entering or leaving the hands-free profile).
    /// macOS only: PipeWire's adapter and the WASAPI engine resample, so the
    /// Linux and Windows backends never report it.
    SampleRateChanged,
    /// The capture stopped delivering: no frame reached the sink for
    /// longer than `CaptureSession::STALL_TIMEOUT` after it had delivered
    /// (a device whose driver or owner hangs, a graph that stopped
    /// running). The session's watchdog reports it, on every platform.
    /// Rust only.
    DeliveryStalled,
    /// The audio service restarted (`coreaudiod` on macOS), taking the
    /// capture's aggregate device with it. macOS only. Rust only.
    AudioServiceRestarted,
    /// The session asks again for the chosen microphone it replaced with
    /// the default input because the chosen one did not open
    /// (`CaptureSession::FALLBACK_RECHECK`). Rust only.
    ChosenInputRecheck,
}

/// What `CaptureSession::notices` carries while the state stays
/// `Recording`: the rebuild beginning and the new backend running. Device
/// loss is not a notice; `states` carries `Failed(DeviceLost)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CaptureNotice {
    /// A change was reported; the rebuild begins.
    DeviceChanged(DeviceChangeReason),
    /// `attempt` is the restart that succeeded (1 when the first did);
    /// `gap_seconds` the silence written for this gap.
    DeviceResumed {
        /// Restarts it took.
        attempt: usize,
        /// Silence written for the gap, seconds.
        gap_seconds: f64,
    },
}

/// What `stop()` reports beside the asset.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureStatistics {
    /// Seconds of audio written to the master.
    pub duration: f64,
    /// Frames lost per lane: ring overruns, a stalled writer's full relay,
    /// whole frames a stop left undrained in the rings and, on Windows,
    /// clock-drift slips; should be empty.
    pub dropped_frames: BTreeMap<AudioLane, usize>,
    /// True when the tap never exceeded [`LaneLevel::SILENT_PEAK_LINEAR`].
    pub system_lane_silent: bool,
    /// True when a device change could not be survived (every restart
    /// failed) and the session finalised the recording early.
    pub ended_on_device_loss: bool,
    /// Device changes the recording survived by rebuilding in place.
    pub device_changes: usize,
    /// Seconds of silence written to keep the master on wall time across
    /// those rebuilds, and across one a stop overtook while it wrote the
    /// gap (that one is not in `device_changes`).
    pub gap_seconds: f64,
}
