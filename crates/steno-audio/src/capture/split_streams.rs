//! Which capture stream feeds which lane when the microphone and the system
//! audio arrive as two streams (WASAPI, WP10a), and the latency arithmetic
//! for them. Pure, so it is tested on every OS; the Windows backend
//! (`capture::live::wasapi`) builds on it. No Swift equivalent (one IOProc
//! there).
//!
//! The plan describes the two streams in [`StreamLayout`]'s terms, so the
//! master thread delivers through the same [`deliver`](crate::realtime::deliver)
//! as the IOProc: buffer 0 is the master stream's packet, buffer 1 the
//! follower's mono staging copy ([`FollowerLane`](crate::realtime::FollowerLane)).
//! The microphone stream is mono (the engine downmixes on request), the
//! system stream stereo and folded to the mono system lane.

use steno_core::AudioLane;

use super::configuration::CaptureError;
use super::layout::StreamLayout;

/// One of the two capture streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamSource {
    /// The microphone: the default capture endpoint or the selected one.
    Microphone,
    /// What the machine plays: process loopback, or endpoint loopback.
    System,
}

impl StreamSource {
    /// Channels requested from the engine for this stream: mono for the
    /// microphone, stereo for the system audio.
    #[must_use]
    pub fn channels(self) -> usize {
        match self {
            StreamSource::Microphone => 1,
            StreamSource::System => 2,
        }
    }

    /// `mic` or `system`, for thread names and logs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            StreamSource::Microphone => "mic",
            StreamSource::System => "system",
        }
    }
}

/// The streams a capture opens for its lanes, and where each lane reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitStreamPlan {
    /// Buffer 0 is the master's packet, buffer 1 the follower's copy.
    pub layout: StreamLayout,
    /// The stream whose thread produces into the sink: the microphone when
    /// a lane needs it, else the system stream.
    pub master: StreamSource,
    /// The system stream as the follower: both the microphone and the
    /// system lane are recorded.
    pub follower: Option<StreamSource>,
}

impl SplitStreamPlan {
    /// The plan for `lanes`, in sink order.
    pub fn new(lanes: &[AudioLane]) -> Result<Self, CaptureError> {
        if lanes.is_empty() {
            return Err(CaptureError::UnexpectedStreamLayout(
                "no lanes to capture".into(),
            ));
        }
        let needs_mic = lanes.contains(&AudioLane::Mic) || lanes.contains(&AudioLane::Mixed);
        let needs_system = lanes.contains(&AudioLane::System);
        let (layout, master, follower) = if needs_mic {
            let mic = StreamSource::Microphone.channels();
            // The follower's staging copy is one mono buffer after the
            // microphone's, in the place of a tap.
            let tap: &[usize] = if needs_system { &[1] } else { &[] };
            let aggregate: Vec<usize> = std::iter::once(mic).chain(tap.iter().copied()).collect();
            let layout = StreamLayout::resolve(lanes, &aggregate, &[vec![mic]], tap, Some(0))?;
            (
                layout,
                StreamSource::Microphone,
                needs_system.then_some(StreamSource::System),
            )
        } else {
            let system = StreamSource::System.channels();
            let layout = StreamLayout::resolve(lanes, &[system], &[], &[system], None)?;
            (layout, StreamSource::System, None)
        };
        Ok(Self {
            layout,
            master,
            follower,
        })
    }

    /// The streams to open, master first.
    #[must_use]
    pub fn streams(&self) -> Vec<StreamSource> {
        std::iter::once(self.master).chain(self.follower).collect()
    }
}

/// A WASAPI duration (100 ns units, `REFERENCE_TIME`) in frames at
/// `sample_rate`, rounded to the nearest frame; 0 for a negative duration.
#[must_use]
pub fn frames_from_hundred_nanoseconds(duration: i64, sample_rate: f64) -> usize {
    if duration <= 0 {
        return 0;
    }
    (duration as f64 * sample_rate / 10_000_000.0).round() as usize
}

/// The input and output latencies a two-stream capture reports, in frames.
/// The session delays the system lane by their sum before echo
/// cancellation; the follower's staging already delays it by
/// `follower_delay` on the master's clock, so that much comes off the sum
/// (from the output side first). Overstating the delay would put the far
/// end after its echo, which the canceller cannot follow; understating it
/// stays inside the 200 ms tail.
#[must_use]
pub fn far_end_latencies(input: usize, output: usize, follower_delay: usize) -> (usize, usize) {
    let output_left = output.saturating_sub(follower_delay);
    let rest = follower_delay.saturating_sub(output);
    (input.saturating_sub(rest), output_left)
}
