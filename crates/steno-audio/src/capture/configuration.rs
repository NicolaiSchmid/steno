//! The value types around a capture: mode, configuration, errors, states,
//! levels, notices and statistics.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use steno_core::{AudioAsset, AudioLane};

steno_core::string_enum! {
    /// How a meeting is captured; the raw values are how `Settings` and the
    /// bridge spell them.
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
    #[must_use]
    pub fn lanes(self) -> Vec<AudioLane> {
        match self {
            CaptureMode::Call => vec![AudioLane::Mic, AudioLane::System],
            CaptureMode::InPerson => vec![AudioLane::Mixed],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureConfiguration {
    pub mode: CaptureMode,
    /// `None`: the default input device.
    pub input_device_uid: Option<String>,
    /// Ignored in `InPerson`.
    pub echo_cancellation: bool,
    /// Debug: writes `mic.raw.caf` (the microphone before echo cancellation)
    /// next to the master.
    pub keep_raw_mic_lane: bool,
    /// The audio folder; the per-meeting folder is created inside.
    pub output_directory: PathBuf,
    /// Developer tools only: record these lanes instead of the mode's. The
    /// app never sets it.
    pub lane_override: Option<Vec<AudioLane>>,
}

impl CaptureConfiguration {
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
    #[error("{operation} failed: {}", four_char_code(*status))]
    CoreAudio {
        /// The HAL function or property.
        operation: String,
        /// The `OSStatus` it returned.
        status: i32,
    },
    #[error("the input device is not available")]
    InputDeviceUnavailable,
    #[error("the output device is not available")]
    OutputDeviceUnavailable,
    #[error("unexpected input stream layout: {0}")]
    UnexpectedStreamLayout(String),
    /// The aggregate runs at a rate the processing thread cannot convert to
    /// [`SAMPLE_RATE`](crate::SAMPLE_RATE), or 0 for an aggregate that is
    /// gone.
    #[error("the audio devices run at {actual} Hz, which Steno cannot record")]
    UnsupportedSampleRate {
        /// The rate the devices run at, whole hertz.
        actual: u32,
    },
    #[error("the system lane stayed silent")]
    SystemAudioSilent,
    /// A device changed or disappeared and the backend could not be
    /// restarted.
    #[error("an audio device disappeared")]
    DeviceLost,
    /// A file write failed; the recording is finalised as far as it got.
    #[error("writing the recording failed: {0}")]
    WriterFailed(String),
    /// The meeting's folder already exists; its files are never written
    /// over.
    #[error("the folder {} already exists, so nothing was recorded", .0.display())]
    RecordingExists(PathBuf),
    #[error("capture backend failed: {0}")]
    BackendFailed(String),
    /// The graph did not run: the devices were linked but delivered no
    /// audio before the start's deadline (on Linux, PipeWire ran no first
    /// cycle), or a rebuild's restart offered no frame within
    /// `CaptureSession::STALL_TIMEOUT`. The session answers it on a chosen
    /// microphone by trying the default input, unless
    /// `CaptureBackend::stall_may_be_the_microphone` says otherwise, and
    /// keeps restarting with backoff until one runs. Reads as
    /// [`Self::BackendFailed`].
    #[error("capture backend failed: {0}")]
    DidNotRun(String),
    #[error("{0}")]
    InvalidState(String),
}

/// Renders an `OSStatus` as its four-character code when it is one
/// (`'!obj'`), else as the number.
#[must_use]
pub(crate) fn four_char_code(status: i32) -> String {
    let bytes = (status as u32).to_be_bytes();
    if bytes.iter().all(|b| (0x20..0x7f).contains(b)) {
        format!("'{}'", String::from_utf8_lossy(&bytes))
    } else {
        status.to_string()
    }
}

/// What `stop()` returns: the finished master with its sidecars, the
/// session's statistics, and what cut the recording short.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureResult {
    pub asset: AudioAsset,
    pub statistics: CaptureStatistics,
    /// What ended the recording early or failed its close (a device that
    /// stayed lost, a full disk), the recording up to it kept; `None` when
    /// it ended as asked and closed cleanly.
    pub failure: Option<CaptureError>,
}

/// The session's state machine; see the `session` module doc.
#[derive(Debug, Clone, PartialEq)]
pub enum CaptureState {
    Idle,
    Starting,
    Recording {
        /// When the backend began delivering, wall time.
        started_at: DateTime<Utc>,
    },
    Stopping,
    /// `recording` is `None` when the start produced nothing, else the
    /// finalised recording as far as it got; `stop()` returns the same
    /// value or fails when it is `None`.
    Failed {
        error: CaptureError,
        recording: Option<Box<CaptureResult>>,
    },
}

impl CaptureState {
    #[must_use]
    pub fn failure(&self) -> Option<&CaptureError> {
        match self {
            CaptureState::Failed { error, .. } => Some(error),
            _ => None,
        }
    }

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
    pub rms: f32,
    pub peak: f32,
}

impl LaneLevel {
    pub const SILENCE: LaneLevel = LaneLevel {
        rms: -160.0,
        peak: -160.0,
    };

    /// -80 dBFS, linear: a lane whose peak never exceeds it is "silent".
    pub const SILENT_PEAK_LINEAR: f32 = 1e-4;
}

/// Published at 10 Hz; `system` is `None` in `InPerson`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LaneLevels {
    pub mic: LaneLevel,
    pub system: Option<LaneLevel>,
}

/// What the backend's listener found different after a notification burst
/// settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceChangeReason {
    DefaultOutputChanged,
    /// The input the capture would record now moved: the default input,
    /// or a chosen microphone that came back.
    DefaultInputChanged,
    OutputDeviceGone,
    InputDeviceGone,
    /// The aggregate no longer runs at the rate the capture started at (a
    /// Bluetooth headset switching profile). macOS only: PipeWire and
    /// WASAPI resample.
    SampleRateChanged,
    /// No frame reached the sink for longer than
    /// `CaptureSession::STALL_TIMEOUT`, over a watched backend
    /// (`CaptureBackend::delivers_continuously`); the rebuild's restarts go
    /// on until one runs.
    DeliveryStalled,
    /// The audio service restarted or went away (`coreaudiod`, or the
    /// PipeWire daemon). The rebuild's restarts go on until one runs. Not
    /// on Windows.
    AudioServiceRestarted,
    /// The chosen microphone, replaced with the default input because it
    /// did not open, delivered to a probe, so the rebuild returns to it.
    /// Linux only.
    ChosenInputRecheck,
}

/// What `CaptureSession::notices` carries while the state stays
/// `Recording`. Device loss is not a notice; `states` carries
/// `Failed(DeviceLost)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CaptureNotice {
    DeviceChanged(DeviceChangeReason),
    /// The first `RESTART_ATTEMPTS` restarts failed in a way that may pass,
    /// so the session goes on restarting until one runs or the stop;
    /// nothing is recorded meanwhile. The restarts count across a streak of
    /// rebuilds whose streams stall again soon after resuming. Sent once
    /// until `Delivering`, then again when the restarts go on once more.
    /// Not sent for a stream whose stall comes 10 s or more after each
    /// resume: each such stall begins a new streak.
    StillRestarting {
        attempt: usize,
    },
    /// `attempt` is the restart that succeeded, counted from the first of
    /// its streak; `gap_seconds` the silence written for this gap. Does not
    /// end the warning; `Delivering` does.
    DeviceResumed {
        attempt: usize,
        gap_seconds: f64,
    },
    /// Audio is back after `StillRestarting`: the resumed stream has
    /// delivered for 10 s, so the warning ends.
    Delivering,
}

/// What `stop()` reports beside the asset.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureStatistics {
    pub duration: f64,
    /// Frames lost per lane: ring overruns, a stalled writer's full relay,
    /// frames a stop left undrained and, on Windows, clock-drift slips.
    pub dropped_frames: BTreeMap<AudioLane, usize>,
    /// True when the tap never exceeded [`LaneLevel::SILENT_PEAK_LINEAR`].
    pub system_lane_silent: bool,
    /// A device change could not be survived and the session finalised the
    /// recording early.
    pub ended_on_device_loss: bool,
    pub device_changes: usize,
    /// Seconds of silence written to keep the master on wall time across
    /// those rebuilds (and one a stop overtook).
    pub gap_seconds: f64,
    /// Seconds of silence written in place of audio the capture's thread
    /// missed ([`Missed::fill`](crate::realtime::Missed::fill)), on the
    /// lane that missed most; part of `duration`.
    pub filled_seconds: f64,
    /// Seconds the capture's thread missed in jumps too long to fill
    /// ([`Missed::unfilled`](crate::realtime::Missed::unfilled)): not in
    /// `duration`, which falls short of wall time by them.
    pub unfilled_seconds: f64,
}
