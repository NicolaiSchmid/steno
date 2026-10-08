//! The HAL seam under [`CaptureSession`](super::CaptureSession).
//! Swift: `Sources/StenoAudio/Capture/CaptureBackend.swift`.

use std::sync::Arc;

use steno_core::AudioLane;

use super::configuration::CaptureError;
use super::layout::StreamLayout;
use crate::SAMPLE_RATE;
use crate::realtime::LaneFrameSink;

/// A backend delivers frames for every lane into the [`LaneFrameSink`] from
/// its own real-time context and reports device changes through the sink.
/// `LiveCaptureBackend` is the tap + aggregate + IOProc on macOS, one
/// self-linked PipeWire stream on Linux and two WASAPI streams on Windows
/// (see `capture::live`);
/// [`SyntheticCaptureBackend`](crate::testing::SyntheticCaptureBackend)
/// generates deterministic tones. The session orchestrates a rebuild after
/// a change by calling `stop()` and `start` again on the same backend and
/// the same sink, so a backend must be restartable.
pub trait CaptureBackend: Send + Sync {
    /// Starts delivering `lanes` (in this order) and describes the stream
    /// it opened, at [`SAMPLE_RATE`] or, where the device will not run at
    /// it, at the device's own rate, which the processing thread converts.
    /// `input_device_uid` `None` selects the default input device. A live
    /// backend given a UID that names no connected input records the
    /// default input instead (the fallback), says so in
    /// [`CaptureStream::input`], and reports a change once the chosen
    /// device is back, so the rebuild's `start` returns to it; the
    /// recording never fails or ends because the chosen microphone is
    /// missing while the default input can be opened. A chosen device that
    /// is connected but fails to open fails this `start` (with
    /// [`CaptureError::DidNotRun`] when it was linked but the graph
    /// delivered nothing); the session then starts again without a UID (see
    /// `CaptureSession`).
    fn start(
        &self,
        lanes: &[AudioLane],
        input_device_uid: Option<&str>,
        sink: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError>;

    /// Stops delivering; idempotent. No frame arrives after it returns.
    fn stop(&self);

    /// Whether a capture of `lanes` delivers callbacks on the device's
    /// clock whatever it hears, silence included, so that one that stops
    /// for longer than [`CaptureSession::STALL_TIMEOUT`] has stalled. The
    /// session watches only such a backend (the stall watchdog, a
    /// rebuild's restart held to deliver within that time and, with
    /// [`Self::probes_inputs`], the timed ask for a chosen microphone; see
    /// `CaptureSession`). `true` for the live backends, except a Windows
    /// capture of the system lane alone (endpoint loopback delivers nothing
    /// while nothing plays); `false` by default, so a test backend whose
    /// audio simply ends is not taken for a stalled one. Rust only.
    ///
    /// [`CaptureSession::STALL_TIMEOUT`]: super::CaptureSession::STALL_TIMEOUT
    fn delivers_continuously(&self, lanes: &[AudioLane]) -> bool {
        let _ = lanes;
        false
    }

    /// Whether a capture of `lanes` that delivers continuously may still
    /// deliver nothing at all until something plays: a Mac call capture
    /// without the capture permission, whose IOProc runs only while
    /// another client has the output open. Until the recording's first
    /// frame the watchdog then takes no stream for stalled; after it, every
    /// stream that stops is one, but a rebuild's restart is not held to
    /// deliver at once, and a chosen microphone that stalls again soon is
    /// not given up for the default (see `CaptureSession`). `false` by
    /// default and on Linux and Windows. Rust only.
    fn waits_for_playback(&self, lanes: &[AudioLane]) -> bool {
        let _ = lanes;
        false
    }

    /// Whether [`Self::probe_input`] can ask a microphone without
    /// touching the capture that records. The session asks for a chosen
    /// microphone it replaced with the default input again on a timer
    /// only over such a backend, and otherwise only when a device change
    /// rebuilds the capture. `true` on Linux (a second PipeWire stream on
    /// a connection of its own); `false` by default, and on macOS and
    /// Windows: there the chosen device's own I/O would have to run beside
    /// the capture's (outside the Mac's aggregate, beside the WASAPI
    /// streams), which on a Bluetooth headset switches its profile under
    /// the output the system lane records, and neither is run on hardware
    /// here. Rust only.
    fn probes_inputs(&self) -> bool {
        false
    }

    /// Whether the input `uid` names delivers audio now, asked on a stream
    /// of its own and never through the capture that records: a capture
    /// keeps every frame while this runs. Blocks for as long as the
    /// backend's start deadline at most; never called with the session's
    /// lock held. `false` by default. Rust only.
    fn probe_input(&self, uid: &str) -> bool {
        let _ = uid;
        false
    }
}

/// What one started capture delivers, as the backend found it: the rate
/// the device runs at, the device latencies the far-end delay is built
/// from, and where each lane sits in the HAL's buffers.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureStream {
    /// The confirmed rate the lanes arrive at: [`SAMPLE_RATE`], except on
    /// the Mac when the clock master will not run at it (a Bluetooth
    /// headset in the hands-free profile runs at 24, 16 or 8 kHz).
    pub sample_rate: f64,
    /// Latency plus safety offset of the microphone's input path, in frames
    /// at `sample_rate` (a microphone on its own clock has its latency
    /// rescaled from its own rate).
    pub input_latency_frames: usize,
    /// Latency plus safety offset of the loudspeaker's output path, in
    /// frames at `sample_rate`: the tap sees a sample this long before the
    /// room hears it.
    pub output_latency_frames: usize,
    /// `None` for a backend without HAL buffers (synthetic).
    pub layout: Option<StreamLayout>,
    /// The microphone the capture records; `None` without a microphone
    /// lane, or for a backend without devices (synthetic).
    pub input: Option<CaptureInput>,
}

/// The microphone a started capture records, so the app can name it while
/// it is not the one the user chose. Rust only: Swift fails the start when
/// the chosen microphone is missing, so its stream needs no such field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureInput {
    /// The UID `Settings.input_device_uid` would store for it: the Core
    /// Audio UID, the PipeWire `node.name`, the WASAPI endpoint id.
    pub uid: String,
    /// Its name, as the input picker lists it; `None` when the system
    /// gives it none (an ID is no name to show).
    pub name: Option<String>,
    /// The fallback: the chosen microphone is not connected or did not
    /// open, so this is the default input recorded in its place.
    pub is_fallback: bool,
}

impl CaptureStream {
    /// 48 kHz, no latency, no HAL layout.
    pub const SYNTHETIC: CaptureStream = CaptureStream {
        sample_rate: SAMPLE_RATE,
        input_latency_frames: 0,
        output_latency_frames: 0,
        layout: None,
        input: None,
    };

    /// `samples` counted at the stream's rate as samples at
    /// [`SAMPLE_RATE`], the rate the processing thread converts to, rounded
    /// down ([`Self::rescaled`]).
    #[must_use]
    pub fn at_output_rate(&self, samples: usize) -> usize {
        Self::rescaled(samples, self.sample_rate, SAMPLE_RATE)
    }

    /// `frames` counted at `from` hertz as frames at `to` hertz, rounded
    /// down: an over-delayed far end is the one error the echo canceller
    /// cannot recover from. Unchanged when the rates are equal or either
    /// is not positive (a rate that could not be read).
    #[must_use]
    pub fn rescaled(frames: usize, from: f64, to: f64) -> usize {
        if from == to || !(from > 0.0 && to > 0.0) {
            return frames;
        }
        (frames as f64 * to / from).floor() as usize
    }
}
