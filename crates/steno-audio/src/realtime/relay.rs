//! The bounded hand-off from the processing thread to the writer thread.
//! Swift: `Sources/StenoAudio/RealTime/FrameRelay.swift`.
//!
//! [`LaneRings`] with one channel per written channel (the lanes, plus the
//! raw mic when kept) and a fixed frame size. The processing thread never
//! blocks on it: when a frame does not fit, every channel of that frame is
//! refused and counted.

use super::rings::LaneRings;
use super::wake::Wake;

/// The processing-to-writer hand-off; see the module doc.
#[derive(Debug)]
pub struct FrameRelay {
    channels: usize,
    frame_size: usize,
    rings: LaneRings,
    capacity_frames: usize,
}

impl FrameRelay {
    /// `capacity_frames` whole frames of headroom per channel.
    #[must_use]
    pub fn new(channels: usize, frame_size: usize, capacity_frames: usize) -> Self {
        Self {
            channels,
            frame_size,
            rings: LaneRings::new(channels, frame_size * capacity_frames),
            capacity_frames,
        }
    }

    /// Channels per frame.
    #[must_use]
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Samples per channel per frame.
    #[must_use]
    pub fn frame_size(&self) -> usize {
        self.frame_size
    }

    /// Whole frames the relay holds at most, as asked for; the ring rounds
    /// up to a power of two, see [`Self::ring_capacity_frames`].
    #[must_use]
    pub fn capacity_frames(&self) -> usize {
        self.capacity_frames
    }

    /// Whole frames the underlying ring actually holds.
    #[must_use]
    pub fn ring_capacity_frames(&self) -> usize {
        self.rings.ring(0).capacity() / self.frame_size
    }

    /// The rings underneath.
    #[must_use]
    pub fn rings(&self) -> &LaneRings {
        &self.rings
    }

    /// Signalled once per committed frame; the writer thread waits on it.
    #[must_use]
    pub fn wake(&self) -> &Wake {
        self.rings.wake()
    }

    // Producer (processing thread)

    /// Whether one more frame fits right now. `begin_frame` counts a
    /// refusal as a drop, so a producer that would rather wait asks first.
    #[must_use]
    pub fn has_room(&self) -> bool {
        self.rings.has_room(self.frame_size)
    }

    /// Reserves one frame on every channel, or counts the refusal on every
    /// channel and returns `false`.
    #[inline(always)]
    pub fn begin_frame(&self) -> bool {
        self.rings.reserve(self.frame_size)
    }

    /// One channel of the frame begun; `source` holds `frame_size` samples.
    #[inline(always)]
    pub fn write(&self, channel: usize, source: &[f32]) {
        self.rings
            .ring(channel)
            .write_slice(&source[..self.frame_size]);
    }

    /// Publishes the frame begun and wakes the writer.
    #[inline(always)]
    pub fn end_frame(&self) {
        self.rings.commit();
    }

    // Consumer (writer thread)

    /// Whole frames every channel has queued.
    #[must_use]
    pub fn available_frames(&self) -> usize {
        self.rings.available_to_read() / self.frame_size
    }

    /// One frame of one channel into `destination` (`frame_size` long).
    #[inline(always)]
    pub fn read(&self, channel: usize, destination: &mut [f32]) -> bool {
        self.rings
            .ring(channel)
            .read(&mut destination[..self.frame_size])
    }

    /// Frames refused per channel.
    #[must_use]
    pub fn dropped_frames(&self) -> Vec<usize> {
        self.rings
            .dropped_samples()
            .into_iter()
            .map(|samples| samples / self.frame_size)
            .collect()
    }
}
