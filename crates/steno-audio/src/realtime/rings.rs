//! One ring per channel plus the invariant that keeps channels aligned.
//! Swift: `Sources/StenoAudio/RealTime/LaneRings.swift`.
//!
//! A block is reserved for every ring or refused, and counted as dropped,
//! for every ring. The producer (IOProc, synthetic backend, processing
//! thread) calls `reserve`, one `write`/`write_mixed`/`write_zeros` per
//! channel, then `commit`, which wakes the consumer once. Nothing on that
//! path allocates or locks.
//!
//! [`LaneFrameSink`](super::LaneFrameSink) (IOProc to processing thread) and
//! [`FrameRelay`](super::FrameRelay) (processing thread to writer thread)
//! are this type with lane names and a fixed frame size respectively; the
//! reservation logic lives here once.

use super::ring::LaneRingBuffer;
use super::wake::Wake;

/// `count` rings advanced together: a reservation on all or none, one
/// commit, one wake.
#[derive(Debug)]
pub struct LaneRings {
    rings: Vec<LaneRingBuffer>,
    /// Signalled once per committed block; the consumer waits on it.
    wake: Wake,
}

impl LaneRings {
    /// `count` rings of `capacity` samples each (rounded up to a power of two).
    #[must_use]
    pub fn new(count: usize, capacity: usize) -> Self {
        Self {
            rings: (0..count).map(|_| LaneRingBuffer::new(capacity)).collect(),
            wake: Wake::new(),
        }
    }

    /// Rings.
    #[must_use]
    pub fn count(&self) -> usize {
        self.rings.len()
    }

    /// Ring `channel`.
    #[must_use]
    pub fn ring(&self, channel: usize) -> &LaneRingBuffer {
        &self.rings[channel]
    }

    /// The consumer's wake-up: one signal per committed block.
    #[must_use]
    pub fn wake(&self) -> &Wake {
        &self.wake
    }

    // Producer (real-time)

    /// Whether every ring can take `count` more samples right now.
    #[inline(always)]
    pub fn has_room(&self, count: usize) -> bool {
        self.rings.iter().all(|ring| ring.has_room(count))
    }

    /// All or nothing: `true` when every ring has room for `count` samples;
    /// otherwise the block is counted as dropped on every ring and nothing
    /// is written.
    #[inline(always)]
    pub fn reserve(&self, count: usize) -> bool {
        if self.has_room(count) {
            return true;
        }
        for ring in &self.rings {
            ring.record_drop(count);
        }
        false
    }

    /// Wakes the consumer once for the block just written.
    #[inline(always)]
    pub fn commit(&self) {
        self.wake.signal();
    }

    // Consumer

    /// Samples every channel has queued right now: the minimum over rings.
    #[must_use]
    pub fn available_to_read(&self) -> usize {
        self.rings
            .iter()
            .map(LaneRingBuffer::available_to_read)
            .min()
            .unwrap_or(0)
    }

    /// Refused samples per channel.
    #[must_use]
    pub fn dropped_samples(&self) -> Vec<usize> {
        self.rings
            .iter()
            .map(LaneRingBuffer::dropped_samples)
            .collect()
    }

    /// Zeroes every ring so a restart never replays stale frames. Only
    /// while no producer runs.
    pub fn clear(&self) {
        for ring in &self.rings {
            ring.clear();
        }
    }
}
