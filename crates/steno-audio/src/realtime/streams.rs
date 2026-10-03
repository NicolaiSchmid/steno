//! Two independent capture streams into one [`LaneFrameSink`]: the bodies
//! of the WASAPI capture threads (WP10), kept apart from COM so they run on
//! every OS under the counting allocator (`tests/realtime.rs`). No Swift
//! equivalent: Core Audio hands the microphone and the tap to one IOProc
//! through the aggregate device, WASAPI delivers them as two streams on two
//! clocks.
//!
//! The sink takes one producer and reserves each callback on every lane at
//! once. With two streams, one of them is the **master**: its thread is the
//! sink's only producer, and every master packet becomes one callback over
//! all lanes. The other stream is the **follower**: its thread writes its
//! packets, folded to mono, into a private staging ring ([`FollowerLane`]),
//! and the master pulls exactly as many follower frames as its own packet
//! carries. The microphone is the master whenever a lane needs it (it
//! delivers continuously); the system stream is the follower, or the master
//! when it is the only stream.
//!
//! The staging ring is a jitter buffer with a fixed target: the master
//! starts pulling once `target` frames are queued beyond its packet (zeros
//! until then), leaves `target` queued, pads with zeros and re-primes when
//! the follower falls short (the follower stopped, or its clock runs slow),
//! and slips back to `target` when the queue passes `high_water` (its clock
//! runs fast). The two endpoints' clocks are not reconciled by resampling;
//! the slips are counted as dropped system frames in the sink's accounting,
//! the shortfalls as underruns. Both are a parity item in the plan.
//!
//! Real-time: [`StreamBody::handle`] allocates nothing and takes no lock;
//! the scratch buffers are allocated when the body is built, before the
//! stream starts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::io_proc::{SliceView, deliver_slices};
use super::ring::LaneRingBuffer;
use super::sink::LaneFrameSink;
use crate::capture::layout::LaneSource;

/// One packet as a capture client handed it out: valid only for the call
/// it is passed to (WASAPI's `GetBuffer` to `ReleaseBuffer`).
#[derive(Debug, Clone, Copy)]
pub struct Packet<'a> {
    /// Frames in the packet.
    pub frames: usize,
    /// Interleaved channels per frame.
    pub channels: usize,
    /// `channels * frames` interleaved samples; `None` for a packet flagged
    /// silent, which becomes zeros.
    pub samples: Option<&'a [f32]>,
}

/// The follower stream's staging ring and the jitter-buffer policy the
/// master applies when it pulls. Producer: the follower's capture thread
/// ([`Self::push`]); consumer: the master's ([`Self::pull`]).
#[derive(Debug)]
pub struct FollowerLane {
    ring: LaneRingBuffer,
    target: usize,
    high_water: usize,
    /// Consumer-only state; an atomic only so the lane is `Sync`.
    primed: AtomicBool,
    underrun: AtomicUsize,
    slipped: AtomicUsize,
    /// Staging overflows already handed to the sink's drop count;
    /// consumer-only.
    reported_overflow: AtomicUsize,
}

impl FollowerLane {
    /// One second of staging at 48 kHz.
    pub const CAPACITY: usize = 48_000;

    /// `target` frames are kept queued; a queue beyond `high_water` slips
    /// back to `target`. `high_water` is raised to `target` if lower.
    #[must_use]
    pub fn new(target: usize, high_water: usize, capacity: usize) -> Self {
        Self {
            ring: LaneRingBuffer::new(capacity.max(high_water)),
            target,
            high_water: high_water.max(target),
            primed: AtomicBool::new(false),
            underrun: AtomicUsize::new(0),
            slipped: AtomicUsize::new(0),
            reported_overflow: AtomicUsize::new(0),
        }
    }

    /// The policy for a stream with `period` frames per packet: a target of
    /// two periods (20 ms at WASAPI's usual 10 ms period), a high-water mark
    /// four periods above it.
    #[must_use]
    pub fn for_period(period: usize) -> Self {
        let period = period.max(1);
        Self::new(2 * period, 6 * period, Self::CAPACITY)
    }

    /// Frames kept queued: how much later the follower lane sits than the
    /// master on the master's clock.
    #[must_use]
    pub fn target(&self) -> usize {
        self.target
    }

    /// Frames of zeros the master wrote because the follower had fallen
    /// short while primed.
    #[must_use]
    pub fn underrun_frames(&self) -> usize {
        self.underrun.load(Ordering::Relaxed)
    }

    /// Frames skipped to bring a fast follower back to the target.
    #[must_use]
    pub fn slipped_frames(&self) -> usize {
        self.slipped.load(Ordering::Relaxed)
    }

    /// Frames queued in the staging ring right now.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.ring.available_to_read()
    }

    /// Producer (follower thread). Folds the packet to mono (the mean of
    /// its first two channels, as the macOS tap is folded) into the staging
    /// ring, whole or not at all; a full ring counts the packet as an
    /// overflow, which the next [`Self::pull`] hands to the sink's drop
    /// count. A packet shorter than it claims is written as zeros.
    /// `scratch` is the thread's fold buffer; any length above zero works.
    #[inline(always)]
    pub fn push(&self, packet: Packet<'_>, scratch: &mut [f32]) {
        let frames = packet.frames;
        if frames == 0 || packet.channels == 0 {
            return;
        }
        if !self.ring.has_room(frames) {
            self.ring.record_drop(frames);
            return;
        }
        let channels = packet.channels;
        let samples = packet
            .samples
            .and_then(|samples| samples.get(..frames * channels));
        match samples {
            None => {
                self.ring.write_zeros(frames);
            }
            Some(samples) if channels == 1 => {
                self.ring.write_slice(samples);
            }
            Some(samples) => {
                let chunk = scratch.len().max(1);
                for block in samples.chunks(chunk * channels) {
                    let count = block.len() / channels;
                    let Some(out) = scratch.get_mut(..count) else {
                        return;
                    };
                    for (sample, frame) in out.iter_mut().zip(block.chunks_exact(channels)) {
                        *sample = f32::midpoint(frame[0], frame[1]);
                    }
                    self.ring.write_slice(out);
                }
            }
        }
    }

    /// Consumer (master thread). Fills `out` with the follower's next
    /// frames under the jitter-buffer policy (see the module doc) and
    /// returns the follower frames lost since the last pull (slips and
    /// staging overflows), for the sink's drop count.
    #[inline(always)]
    pub fn pull(&self, out: &mut [f32]) -> usize {
        let wanted = out.len();
        let available = self.ring.available_to_read();
        let mut lost = 0;
        let overflow = self.ring.dropped_samples();
        let reported = self.reported_overflow.load(Ordering::Relaxed);
        if overflow > reported {
            lost += overflow - reported;
            self.reported_overflow.store(overflow, Ordering::Relaxed);
        }
        if !self.primed.load(Ordering::Relaxed) {
            if available < self.target + wanted {
                out.fill(0.0);
                return lost;
            }
            self.primed.store(true, Ordering::Relaxed);
        }
        if available >= wanted {
            self.ring.read(out);
            let rest = available - wanted;
            if rest > self.high_water {
                let skipped = self.ring.discard(rest - self.target);
                self.slipped.fetch_add(skipped, Ordering::Relaxed);
                lost += skipped;
            }
        } else {
            let (head, tail) = out.split_at_mut(available);
            self.ring.read(head);
            tail.fill(0.0);
            self.underrun
                .fetch_add(wanted - available, Ordering::Relaxed);
            self.primed.store(false, Ordering::Relaxed);
        }
        lost
    }
}

/// The master stream's body: each packet, plus as many follower frames,
/// into the sink as one callback over every lane through
/// [`deliver_slices`], so the all-or-nothing reservation and the drop
/// accounting are the IOProc's. The layout's buffer 0 is the master
/// packet, buffer 1 the follower's mono copy (see
/// [`SplitStreamPlan`](crate::capture::SplitStreamPlan)).
#[derive(Debug)]
pub struct PacketRouter {
    sources: Vec<LaneSource>,
    follower: Option<(Arc<FollowerLane>, usize)>,
    scratch: Box<[f32]>,
}

impl PacketRouter {
    /// `max_frames` is the largest packet expected (the capture client's
    /// buffer size); a larger one is delivered in pieces. The follower's
    /// lane is the source reading buffer 1.
    #[must_use]
    pub fn new(
        sources: Vec<LaneSource>,
        follower: Option<Arc<FollowerLane>>,
        max_frames: usize,
    ) -> Self {
        let follower = follower.and_then(|follower| {
            let lane = sources.iter().position(|s| s.left.buffer == 1)?;
            Some((follower, lane))
        });
        Self {
            sources,
            follower,
            scratch: vec![0.0; max_frames.max(1)].into_boxed_slice(),
        }
    }

    /// One master packet into the sink.
    #[inline(always)]
    pub fn route(&mut self, packet: Packet<'_>, sink: &LaneFrameSink) {
        let channels = packet.channels;
        if channels == 0 {
            return;
        }
        let Self {
            sources,
            follower,
            scratch,
        } = self;
        let mut offset = 0;
        while offset < packet.frames {
            let frames = (packet.frames - offset).min(scratch.len());
            // A sub-slice past the packet's end becomes silence, like any
            // short buffer.
            let master = SliceView {
                channels,
                frames,
                samples: packet
                    .samples
                    .and_then(|s| s.get(offset * channels..(offset + frames) * channels)),
            };
            match follower {
                Some((lane, index)) => {
                    let staged = &mut scratch[..frames];
                    let lost = lane.pull(staged);
                    let views = [
                        master,
                        SliceView {
                            channels: 1,
                            frames,
                            samples: Some(staged),
                        },
                    ];
                    deliver_slices(&views, sources, sink);
                    if lost > 0 {
                        sink.ring(*index).record_drop(lost);
                    }
                }
                None => deliver_slices(&[master], sources, sink),
            }
            offset += frames;
        }
    }
}

/// What a capture thread runs per packet.
#[derive(Debug)]
pub enum StreamBody {
    /// The sink's producer.
    Master {
        /// Routes each packet into the sink.
        router: PacketRouter,
        /// The sink every lane lands in.
        sink: Arc<LaneFrameSink>,
    },
    /// Stages packets for the master.
    Follower {
        /// The staging ring the master pulls from.
        lane: Arc<FollowerLane>,
        /// The fold buffer for stereo packets.
        scratch: Box<[f32]>,
    },
}

impl StreamBody {
    /// The follower body with a fold buffer for `max_frames`.
    #[must_use]
    pub fn follower(lane: Arc<FollowerLane>, max_frames: usize) -> Self {
        StreamBody::Follower {
            lane,
            scratch: vec![0.0; max_frames.max(1)].into_boxed_slice(),
        }
    }

    /// One packet; real-time safe.
    #[inline(always)]
    pub fn handle(&mut self, packet: Packet<'_>) {
        match self {
            StreamBody::Master { router, sink } => router.route(packet, sink),
            StreamBody::Follower { lane, scratch } => lane.push(packet, scratch),
        }
    }
}
