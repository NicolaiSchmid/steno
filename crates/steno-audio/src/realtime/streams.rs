//! Two independent capture streams into one [`LaneFrameSink`]: the bodies
//! of the WASAPI capture threads (WP10a), kept apart from COM so they run on
//! every OS under the counting allocator (`tests/realtime.rs`). No Swift
//! counterpart: Core Audio hands the microphone and the tap to one IOProc
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
//! the follower falls short (the follower stopped, or its clock runs
//! slow), and slips back to `target` when the queue stays above
//! `high_water` for a whole slip window (its clock runs fast). The window
//! matters because the queue also rises when the master's thread runs
//! late and then drains its packets back to back: that is a moment of
//! jitter, not drift, and the queue falls back on its own, so only the
//! lowest queue left over a window is judged. A queue more than the
//! master's own buffer above `high_water` cannot be such lateness (the
//! master would have lost data first), so it slips at once.
//!
//! What the follower queued before the master's first pull is trimmed to
//! the target, uncounted: audio from before the recording, or, when the
//! master's first drain is late, the system audio recorded during that
//! lateness. A full staging ring refuses the newest packets, so one it
//! refused before that pull (it takes a master that starts more than the
//! ring's 1.37 s after the follower) leaves only older audio queued: the
//! first pull drops all of it and the refused packets, uncounted, and the
//! lane primes on the audio that follows.
//!
//! The two endpoints' clocks are not reconciled by resampling; the slips
//! are counted as dropped system frames in the sink's accounting, the
//! shortfalls and their re-prime zeros as underruns. The far-end delay assumes `target`, so between
//! slips the system lane sits later than the echo canceller expects by up
//! to `high_water - target` (one period with
//! [`FollowerLane::for_streams`]), plus the follower's worst lateness in a
//! window (it lowers the window's lowest queue and so hides drift), plus up
//! to two windows of drift (about 0.1 ms at 100 ppm), and never by more
//! than one period plus the master's buffer (the immediate slip). The
//! plan's parity list tracks both the missing resampling and this error.
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

/// The follower stream's staging ring and the jitter-buffer policy the
/// master applies when it pulls. Producer: the follower's capture thread
/// ([`Self::push`]); consumer: the master's ([`Self::pull`]).
#[derive(Debug)]
pub struct FollowerLane {
    ring: LaneRingBuffer,
    target: usize,
    high_water: usize,
    window: usize,
    /// A queue above this after a pull slips at once (see the module doc).
    ceiling: usize,
    /// Consumer-only state from here on; atomics only so the lane is
    /// `Sync`.
    primed: AtomicBool,
    /// The master's first pull is behind us: nothing is trimmed after it.
    started: AtomicBool,
    underrun: AtomicUsize,
    slipped: AtomicUsize,
    trimmed: AtomicUsize,
    /// Master frames pulled in the current slip window.
    window_pulled: AtomicUsize,
    /// The lowest queue a pull left in the current slip window.
    window_low: AtomicUsize,
    /// Staging overflows already handed to the sink's drop count.
    reported_overflow: AtomicUsize,
}

impl FollowerLane {
    /// One second of staging at 48 kHz (the ring rounds it up to 65 536
    /// frames, about 1.37 s).
    pub const CAPACITY: usize = 48_000;

    /// Half a second at 48 kHz: how long the queue must stay above the
    /// high-water mark before it slips. Windows are back to back, so a
    /// sustained excess slips within one to two windows.
    pub const SLIP_WINDOW: usize = 24_000;

    /// `target` frames are kept queued; a queue that stays above
    /// `high_water` for `window` frames of master pulls slips back to
    /// `target`. `high_water` is raised to `target` if lower. No queue
    /// slips at once without [`Self::with_max_lateness`].
    #[must_use]
    pub fn new(target: usize, high_water: usize, window: usize, capacity: usize) -> Self {
        Self {
            ring: LaneRingBuffer::new(capacity.max(high_water)),
            target,
            high_water: high_water.max(target),
            window,
            ceiling: usize::MAX,
            primed: AtomicBool::new(false),
            started: AtomicBool::new(false),
            underrun: AtomicUsize::new(0),
            slipped: AtomicUsize::new(0),
            trimmed: AtomicUsize::new(0),
            window_pulled: AtomicUsize::new(0),
            window_low: AtomicUsize::new(usize::MAX),
            reported_overflow: AtomicUsize::new(0),
        }
    }

    /// The policy for a follower with `period` frames per packet behind a
    /// master whose buffer holds `master_buffer` frames: a target of two
    /// periods (20 ms at WASAPI's usual 10 ms period), a high-water mark
    /// one period above it, [`Self::SLIP_WINDOW`], and a master that runs
    /// at most `master_buffer` late. A master that runs late and catches up
    /// costs nothing, and a drift slip drops a little over one period.
    ///
    /// ```
    /// use steno_audio::realtime::FollowerLane;
    ///
    /// // 10 ms periods at 48 kHz behind a 100 ms master buffer.
    /// let lane = FollowerLane::for_streams(480, 4_800);
    /// assert_eq!(lane.target(), 960);
    /// ```
    #[must_use]
    pub fn for_streams(period: usize, master_buffer: usize) -> Self {
        let period = period.max(1);
        Self::new(2 * period, 3 * period, Self::SLIP_WINDOW, Self::CAPACITY)
            .with_max_lateness(master_buffer)
    }

    /// The most the master can run late, in frames; a queue more than this
    /// above the high-water mark after a pull slips to the target at once
    /// (see the module doc).
    #[must_use]
    pub fn with_max_lateness(mut self, frames: usize) -> Self {
        self.ceiling = self.high_water.saturating_add(frames);
        self
    }

    /// Frames kept queued: how much later the follower lane sits than the
    /// master on the master's clock.
    #[must_use]
    pub fn target(&self) -> usize {
        self.target
    }

    /// Frames of zeros the master wrote because the follower fell short:
    /// the shortfall and the re-prime that follows it. The zeros before the
    /// lane first primes are not counted.
    #[must_use]
    pub fn underrun_frames(&self) -> usize {
        self.underrun.load(Ordering::Relaxed)
    }

    /// Frames skipped to bring a fast follower back to the target.
    #[must_use]
    pub fn slipped_frames(&self) -> usize {
        self.slipped.load(Ordering::Relaxed)
    }

    /// Frames the follower delivered before the master's first pull that
    /// were dropped there, not counted as lost: the queue trimmed to the
    /// target, or all of it and the refused packets after a staging
    /// overflow (see the module doc).
    #[must_use]
    pub fn trimmed_frames(&self) -> usize {
        self.trimmed.load(Ordering::Relaxed)
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
    /// count (none before the master's first pull, see the module doc). A
    /// packet shorter than it claims is written as zeros.
    /// `scratch` is the thread's fold buffer; any length above zero works.
    #[inline(always)]
    pub fn push(&self, packet: SliceView<'_>, scratch: &mut [f32]) {
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
        let mut available = self.ring.available_to_read();
        let mut lost = 0;
        let first_pull = !self.started.swap(true, Ordering::Relaxed);
        let overflow = self.ring.dropped_samples();
        let reported = self.reported_overflow.load(Ordering::Relaxed);
        if overflow > reported {
            self.reported_overflow.store(overflow, Ordering::Relaxed);
            if first_pull {
                // The full ring refused the newest audio, so what is
                // queued is older than the gap it left: all of it goes,
                // uncounted, as the trim below (see the module doc).
                let discarded = self.ring.discard(available);
                self.trimmed
                    .fetch_add(discarded + overflow - reported, Ordering::Relaxed);
                available -= discarded;
            } else {
                lost += overflow - reported;
            }
        }
        if first_pull && available > self.target + wanted {
            let trimmed = self.ring.discard(available - self.target - wanted);
            self.trimmed.fetch_add(trimmed, Ordering::Relaxed);
            available -= trimmed;
        }
        if !self.primed.load(Ordering::Relaxed) {
            if available < self.target + wanted {
                out.fill(0.0);
                // Only an underrun unprimes a lane that primed, and it
                // counts at least one frame: these zeros are its re-prime.
                if self.underrun.load(Ordering::Relaxed) > 0 {
                    self.underrun.fetch_add(wanted, Ordering::Relaxed);
                }
                return lost;
            }
            self.primed.store(true, Ordering::Relaxed);
            self.restart_window();
        }
        if available >= wanted {
            self.ring.read(out);
            lost += self.judge_window(available - wanted, wanted);
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

    /// Records the queue a pull of `pulled` frames left (`rest`); slips it
    /// back to the target at once when it is above the ceiling, or at the
    /// end of a slip window when the window's lowest queue stayed above the
    /// high-water mark. Returns the frames skipped.
    #[inline(always)]
    fn judge_window(&self, rest: usize, pulled: usize) -> usize {
        if rest > self.ceiling {
            self.restart_window();
            return self.slip(rest);
        }
        let low = self.window_low.load(Ordering::Relaxed).min(rest);
        let seen = self.window_pulled.load(Ordering::Relaxed) + pulled;
        if seen < self.window {
            self.window_low.store(low, Ordering::Relaxed);
            self.window_pulled.store(seen, Ordering::Relaxed);
            return 0;
        }
        self.restart_window();
        if low <= self.high_water {
            return 0;
        }
        // `rest` frames are still queued and `low <= rest`, so this skips
        // whole and leaves at least the target.
        self.slip(low)
    }

    /// Skips `queued - target` frames (`queued` is at most what is queued
    /// and above the target) and counts them as slipped.
    #[inline(always)]
    fn slip(&self, queued: usize) -> usize {
        let skipped = self.ring.discard(queued - self.target);
        self.slipped.fetch_add(skipped, Ordering::Relaxed);
        skipped
    }

    #[inline(always)]
    fn restart_window(&self) {
        self.window_low.store(usize::MAX, Ordering::Relaxed);
        self.window_pulled.store(0, Ordering::Relaxed);
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
    pub fn route(&mut self, packet: SliceView<'_>, sink: &LaneFrameSink) {
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
    pub fn handle(&mut self, packet: SliceView<'_>) {
        match self {
            StreamBody::Master { router, sink } => router.route(packet, sink),
            StreamBody::Follower { lane, scratch } => lane.push(packet, scratch),
        }
    }
}
