//! Two independent capture streams into one [`LaneFrameSink`]: the bodies
//! of the WASAPI capture threads, kept apart from COM so they run on every
//! OS. The master stream's thread is the sink's only producer; the follower
//! stages its packets, folded to mono, in a jitter buffer ([`FollowerLane`])
//! the master pulls from. Real-time: nothing here allocates or locks.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::io_proc::{SliceView, deliver_slices};
use super::missed::{MISSED_FILL_LIMIT, Missed};
use super::ring::{LaneRingBuffer, saturating_add};
use super::sink::LaneFrameSink;
use crate::SAMPLE_RATE;
use crate::capture::layout::LaneSource;

/// The follower stream's staging ring and the jitter-buffer policy the
/// master applies when it pulls: it keeps `target` frames queued, pads
/// with zeros and re-primes when the follower falls short, and slips back
/// to `target` when the lowest queue of a slip window stays above
/// `high_water` (a fast clock), or at once above the ceiling. Producer: the
/// follower's capture thread ([`Self::push`]); consumer: the master's
/// ([`Self::pull`]). The clocks are not resampled; slips count as dropped
/// system frames, shortfalls as underruns.
#[derive(Debug)]
pub struct FollowerLane {
    ring: LaneRingBuffer,
    target: usize,
    high_water: usize,
    window: usize,
    /// A queue above this after a pull slips at once.
    ceiling: usize,
    /// Consumer-only state from here on; atomics only so the lane is
    /// `Sync`.
    primed: AtomicBool,
    /// The master's first pull is behind us: nothing is trimmed after it.
    started: AtomicBool,
    underrun: AtomicUsize,
    slipped: AtomicUsize,
    trimmed: AtomicUsize,
    window_pulled: AtomicUsize,
    window_low: AtomicUsize,
    reported_overflow: AtomicUsize,
    /// Frames the follower's thread missed, not yet handed to the sink's
    /// counts.
    missed_fill: AtomicUsize,
    missed_unfilled: AtomicUsize,
    /// Zeros padded since the lane last primed while the follower pushed
    /// nothing, plus the runs before the last re-prime the hold still
    /// keeps. A re-prime's wait is delay, not loss, and stays out.
    run: AtomicUsize,
    last_run: AtomicUsize,
    /// Producer: frames the follower pushed (wrapping); consumer: their
    /// count at the last re-prime, the last pull, and the last pull that
    /// padded the run while the lane was out. While fewer than
    /// `reprime_hold` came since the re-prime, `last_run` may still be a
    /// discontinuity's padding.
    pushed: AtomicUsize,
    pushed_at_prime: AtomicUsize,
    pushed_at_pull: AtomicUsize,
    pushed_at_pad: AtomicUsize,
    reprime_hold: usize,
    /// The padding of a discontinuity that came while the lane was out,
    /// snapshotted at the master's first pull after it; `usize::MAX` with
    /// none pending.
    flag_run: AtomicUsize,
    /// Producer: the follower's queue at a flagged packet of the pending
    /// discontinuity was within the target and the hold.
    flag_fresh: AtomicBool,
    /// Producer: flagged packets recorded (wrapping); consumer: their count
    /// when `flag_run` was last added to, and when they were last handed
    /// on. A flag since the hand-on is a discontinuity still pending.
    flags_recorded: AtomicUsize,
    flags_snapped: AtomicUsize,
    flags_handed: AtomicUsize,
}

impl FollowerLane {
    /// One second of staging at 48 kHz (the ring rounds it up to 65 536
    /// frames, about 1.37 s).
    pub const CAPACITY: usize = 48_000;

    /// Half a second at 48 kHz: how long the queue must stay above the
    /// high-water mark before it slips.
    pub const SLIP_WINDOW: usize = 24_000;

    /// `target` frames are kept queued; a queue that stays above
    /// `high_water` for `window` frames of master pulls slips back to
    /// `target`. `high_water` is raised to `target` if lower.
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
            missed_fill: AtomicUsize::new(0),
            missed_unfilled: AtomicUsize::new(0),
            run: AtomicUsize::new(0),
            last_run: AtomicUsize::new(0),
            pushed: AtomicUsize::new(0),
            pushed_at_prime: AtomicUsize::new(0),
            pushed_at_pull: AtomicUsize::new(0),
            pushed_at_pad: AtomicUsize::new(0),
            reprime_hold: capacity,
            flag_run: AtomicUsize::new(usize::MAX),
            flag_fresh: AtomicBool::new(false),
            flags_recorded: AtomicUsize::new(0),
            flags_snapped: AtomicUsize::new(0),
            flags_handed: AtomicUsize::new(0),
        }
    }

    /// The policy for a follower with `period` frames per packet behind a
    /// master whose buffer holds `master_buffer` frames: a target of two
    /// periods, a high-water mark one period above it, [`Self::SLIP_WINDOW`],
    /// and a master that runs at most `master_buffer` late.
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
            .with_reprime_hold(master_buffer.saturating_add(period))
    }

    /// [`Self::for_streams`] behind a master that writes what its thread
    /// missed as silence ([`StreamBody::write_missed`]): it can run late by
    /// up to [`MISSED_FILL_LIMIT`] more, whose fill pulls the follower's
    /// audio queued meanwhile, so that queue must not slip at once.
    #[must_use]
    pub fn for_streams_filling(period: usize, master_buffer: usize) -> Self {
        let fill_limit = (MISSED_FILL_LIMIT.as_secs_f64() * SAMPLE_RATE) as usize;
        Self::for_streams(period, master_buffer)
            .with_max_lateness(master_buffer.saturating_add(fill_limit))
    }

    /// The most the master can run late, in frames; a queue more than this
    /// above the high-water mark after a pull slips at once.
    #[must_use]
    pub fn with_max_lateness(mut self, frames: usize) -> Self {
        self.ceiling = self.high_water.saturating_add(frames);
        self
    }

    /// How many follower frames may come after a re-prime while a
    /// discontinuity still counts the padding before it as its loss: an
    /// engine that refused packets during an outage hands over the ones it
    /// kept unflagged and flags only the packet after them. Counted in the
    /// follower's frames, so the master's catch-up does not use it up.
    #[must_use]
    pub fn with_reprime_hold(mut self, frames: usize) -> Self {
        self.reprime_hold = frames;
        self
    }

    /// Frames kept queued: how much later the follower lane sits than the
    /// master on the master's clock.
    #[must_use]
    pub fn target(&self) -> usize {
        self.target
    }

    /// Frames of zeros the master wrote because the follower fell short,
    /// re-primes included; not those before the lane first primes.
    #[must_use]
    pub fn underrun_frames(&self) -> usize {
        self.underrun.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn slipped_frames(&self) -> usize {
        self.slipped.load(Ordering::Relaxed)
    }

    /// Frames the follower delivered before the master's first pull that
    /// were dropped there, uncounted: the queue trimmed to the target, or
    /// all of it after a staging overflow.
    #[must_use]
    pub fn trimmed_frames(&self) -> usize {
        self.trimmed.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn queued(&self) -> usize {
        self.ring.available_to_read()
    }

    /// Producer (follower thread). Folds the packet to mono (the mean of
    /// its first two channels) into the staging ring, whole or not at all;
    /// a full ring counts it as an overflow for the next [`Self::pull`]. A
    /// packet shorter than it claims is written as zeros.
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
        self.pushed.fetch_add(frames, Ordering::Relaxed);
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

    /// Producer (follower thread), at each packet flagged as a
    /// discontinuity: what the follower's clocks read was missed before it,
    /// for [`Self::take_missed`]. It stages nothing: the master's pulls pad
    /// the lane with zeros meanwhile, and that padding is the fill.
    #[inline(always)]
    pub fn record_missed(&self, missed: Missed) {
        saturating_add(&self.missed_fill, missed.fill);
        saturating_add(&self.missed_unfilled, missed.unfilled);
        // A later flag while one is pending can only make it fresh, so no
        // outage's padding is dropped.
        let recorded = self.flags_recorded.load(Ordering::Relaxed);
        let fresh = self.ring.available_to_read() <= self.target.saturating_add(self.reprime_hold);
        if fresh || self.flags_handed.load(Ordering::Acquire) == recorded {
            self.flag_fresh.store(fresh, Ordering::Relaxed);
        }
        self.flags_recorded
            .store(recorded.wrapping_add(1), Ordering::Release);
    }

    /// Consumer (master thread). What the follower lost since the last
    /// call. After a discontinuity, once the lane primed again, the zeros
    /// padded while the follower delivered nothing count as filled: the
    /// loss is the larger of that and what [`Self::record_missed`] counted.
    /// An underrun no packet flagged stays in [`Self::underrun_frames`],
    /// unless its re-prime came within the hold before a discontinuity (an
    /// overcount). A follower that queued more than the target and the hold
    /// at each of its flags came back long before them and takes no
    /// padding. The padding reads the loss to within the lane's lag.
    #[inline(always)]
    pub fn take_missed(&self) -> Missed {
        let recorded = self.pending_flags();
        if recorded.is_some() && !self.primed.load(Ordering::Relaxed) {
            return Missed::default();
        }
        if recorded.is_none()
            && self
                .pushed
                .load(Ordering::Relaxed)
                .wrapping_sub(self.pushed_at_prime.load(Ordering::Relaxed))
                > self.reprime_hold
        {
            self.last_run.store(0, Ordering::Relaxed);
        }
        self.hand_on(recorded, &self.last_run)
    }

    /// The backend's stop, once both capture threads are joined: what the
    /// lane still holds, as [`Self::take_missed`] but without waiting for
    /// a re-prime, both runs taken. After it, nothing is held.
    #[must_use]
    pub fn take_missed_at_stop(&self) -> Missed {
        let recorded = self.pending_flags();
        let held = self.held_run();
        self.run.store(0, Ordering::Relaxed);
        self.last_run.store(0, Ordering::Relaxed);
        if self.flags_snapped.load(Ordering::Relaxed) != self.flags_recorded.load(Ordering::Relaxed)
        {
            // A flag with no pull after it: what the snapshots took and
            // everything padded since are its padding.
            let snapped = self.flag_run.swap(usize::MAX, Ordering::Relaxed);
            let snapped = if snapped == usize::MAX { 0 } else { snapped };
            self.last_run
                .store(snapped.saturating_add(held), Ordering::Relaxed);
        }
        self.hand_on(recorded, &self.last_run)
    }

    /// Consumer: the count of flagged packets recorded when one is still
    /// pending, `None` otherwise.
    #[inline(always)]
    fn pending_flags(&self) -> Option<usize> {
        let recorded = self.flags_recorded.load(Ordering::Acquire);
        // Pending is the count recorded against the count handed on, not a
        // flag the hand-on clears: a flag the follower records between this
        // read and `hand_on`'s store stays pending. `hand_on` reads the
        // freshness and the snapshot before that store, so a racing flag
        // can only make the one handed on fresh, never overwrite it.
        (recorded != self.flags_handed.load(Ordering::Relaxed)).then_some(recorded)
    }

    /// What [`Self::record_missed`] counted, and after the discontinuities
    /// up to the `recorded`th flag the larger of that and the zeros in
    /// `run`, capped by the run at the flag. A later flag stays pending.
    #[inline(always)]
    fn hand_on(&self, recorded: Option<usize>, run: &AtomicUsize) -> Missed {
        let counted = Missed {
            fill: self.missed_fill.swap(0, Ordering::Relaxed),
            unfilled: self.missed_unfilled.swap(0, Ordering::Relaxed),
        };
        let Some(recorded) = recorded else {
            return counted;
        };
        // Both read before the store (see `pending_flags`).
        let at_flag = self.flag_run.swap(usize::MAX, Ordering::Relaxed);
        let fresh = self.flag_fresh.load(Ordering::Relaxed);
        self.flags_handed.store(recorded, Ordering::Release);
        let padded = if !fresh {
            // The follower came back long before the flag: no padding is
            // its loss.
            0
        } else if at_flag != usize::MAX {
            // The flag came while the lane was out: the padding up to it.
            // What padded after stays in `run` for a later flag.
            at_flag
        } else {
            run.swap(0, Ordering::Relaxed)
        };
        let lost = counted.fill.saturating_add(counted.unfilled).max(padded);
        let fill = counted.fill.max(padded).min(lost);
        Missed {
            fill,
            unfilled: lost - fill,
        }
    }

    /// Consumer (master thread), before the master's packet of `next`
    /// frames that follows a stretch its thread missed and did not fill:
    /// drops the follower's audio for that stretch, so both lanes lose the
    /// same span and keep their lag. Returns the frames dropped.
    #[inline(always)]
    pub fn skip_unfilled(&self, next: usize) -> usize {
        let queued = self.ring.available_to_read();
        let keep = self.target.saturating_add(next);
        if queued <= keep {
            return 0;
        }
        self.ring.discard(queued - keep)
    }

    /// Consumer (master thread). Fills `out` with the follower's next
    /// frames under the jitter-buffer policy and returns the follower
    /// frames lost since the last pull, for the sink's drop count.
    #[inline(always)]
    pub fn pull(&self, out: &mut [f32]) -> usize {
        let wanted = out.len();
        let mut available = self.ring.available_to_read();
        let mut lost = 0;
        let first_pull = !self.started.swap(true, Ordering::Relaxed);
        // The follower pushed since the last pull: zeros padded now are a
        // re-prime's wait, not an outage.
        let pushed = self.pushed.load(Ordering::Relaxed);
        let waiting = self.pushed_at_pull.swap(pushed, Ordering::Relaxed) != pushed;
        let overflow = self.ring.dropped_samples();
        let reported = self.reported_overflow.load(Ordering::Relaxed);
        if overflow > reported {
            self.reported_overflow.store(overflow, Ordering::Relaxed);
            if first_pull {
                // The full ring refused the newest audio, so what is
                // queued is older than the gap it left: all of it goes.
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
            if let Some(recorded) = self.pending_flags()
                && self.flags_snapped.load(Ordering::Relaxed) != recorded
            {
                self.flags_snapped.store(recorded, Ordering::Relaxed);
                // The first pull since the follower flagged a loss while
                // its lane was out: the padding so far is that outage's,
                // and the snapshot takes it out of the runs.
                let held = self.held_run();
                self.run.store(0, Ordering::Relaxed);
                self.last_run.store(0, Ordering::Relaxed);
                let snapped = self.flag_run.load(Ordering::Relaxed);
                self.flag_run.store(
                    if snapped == usize::MAX {
                        held
                    } else {
                        snapped.saturating_add(held)
                    },
                    Ordering::Relaxed,
                );
            }
            if available < self.target + wanted {
                out.fill(0.0);
                // Only an underrun unprimes a lane that primed, and it
                // counts at least one frame: these zeros are its re-prime.
                if self.underrun.load(Ordering::Relaxed) > 0 {
                    self.underrun.fetch_add(wanted, Ordering::Relaxed);
                }
                // The run counts the zeros before the first prime too, so
                // a follower out from its first packets counts its padding.
                if !waiting {
                    self.run.fetch_add(wanted, Ordering::Relaxed);
                    self.pushed_at_pad.store(pushed, Ordering::Relaxed);
                }
                return lost;
            }
            self.primed.store(true, Ordering::Relaxed);
            let run = self.run.swap(0, Ordering::Relaxed);
            // A run the follower delivered past the hold after (an idle
            // that ended while the master was out) is no discontinuity's.
            let since_pad = pushed.wrapping_sub(self.pushed_at_pad.load(Ordering::Relaxed));
            let run = if since_pad > self.reprime_hold {
                0
            } else {
                run
            };
            // A re-prime within the hold of the last one keeps the run
            // before it: it may still be the outage's padding.
            let since_prime = pushed.wrapping_sub(self.pushed_at_prime.load(Ordering::Relaxed));
            let kept = if since_prime > self.reprime_hold {
                0
            } else {
                self.last_run.load(Ordering::Relaxed)
            };
            self.last_run
                .store(kept.saturating_add(run), Ordering::Relaxed);
            self.pushed_at_prime.store(pushed, Ordering::Relaxed);
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
            self.run.fetch_add(wanted - available, Ordering::Relaxed);
            self.primed.store(false, Ordering::Relaxed);
        }
        lost
    }

    /// The zeros of the current run and of the run before the last
    /// re-prime, which [`Self::take_missed`] clears once the hold passed.
    #[inline(always)]
    fn held_run(&self) -> usize {
        self.run
            .load(Ordering::Relaxed)
            .saturating_add(self.last_run.load(Ordering::Relaxed))
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
/// [`deliver_slices`]. Buffer 0 is the master packet, buffer 1 the
/// follower's mono copy.
#[derive(Debug)]
pub struct PacketRouter {
    sources: Vec<LaneSource>,
    follower: Option<(Arc<FollowerLane>, usize)>,
    scratch: Box<[f32]>,
}

impl PacketRouter {
    /// `max_frames` is the largest packet expected; a larger one is
    /// delivered in pieces. The follower's lane is the source reading
    /// buffer 1.
    #[must_use]
    pub fn new(
        sources: Vec<LaneSource>,
        follower: Option<Arc<FollowerLane>>,
        max_frames: usize,
    ) -> Self {
        let follower =
            follower.and_then(|follower| Some((follower, Self::follower_lane(&sources)?)));
        Self {
            sources,
            follower,
            scratch: vec![0.0; max_frames.max(1)].into_boxed_slice(),
        }
    }

    #[must_use]
    pub fn follower_lane(sources: &[LaneSource]) -> Option<usize> {
        sources.iter().position(|s| s.left.buffer == 1)
    }

    /// One master packet into the sink; the frames the sink took (none
    /// refused as a drop).
    #[inline(always)]
    pub fn route(&mut self, packet: SliceView<'_>, sink: &LaneFrameSink) -> usize {
        let channels = packet.channels;
        if channels == 0 {
            return 0;
        }
        let Self {
            sources,
            follower,
            scratch,
        } = self;
        let mut written = 0;
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
            let delivered = match follower {
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
                    let delivered = deliver_slices(&views, sources, sink);
                    if lost > 0 {
                        sink.ring(*index).record_drop(lost);
                    }
                    let missed = lane.take_missed();
                    if missed.fill > 0 {
                        sink.ring(*index).record_filled(missed.fill);
                    }
                    if missed.unfilled > 0 {
                        sink.ring(*index).record_unfilled(missed.unfilled);
                    }
                    delivered
                }
                None => deliver_slices(&[master], sources, sink),
            };
            if delivered {
                written += frames;
            }
            offset += frames;
        }
        written
    }

    /// What the master's thread missed before `next`, its next packet. The
    /// fill goes in as a silent packet that still pulls the follower's
    /// frames for that time; the unfilled frames count on the master's
    /// lanes and drop the follower's audio for that time.
    #[inline(always)]
    pub fn write_missed(&mut self, next: &SliceView<'_>, missed: Missed, sink: &LaneFrameSink) {
        let follower = self.follower.as_ref().map(|(_, index)| *index);
        let masters = (0..self.sources.len()).filter(move |lane| Some(*lane) != follower);
        if missed.fill > 0 {
            let silent = SliceView {
                channels: next.channels,
                frames: missed.fill,
                samples: None,
            };
            let written = self.route(silent, sink);
            if written > 0 {
                for lane in masters.clone() {
                    sink.ring(lane).record_filled(written);
                }
            }
        }
        if missed.unfilled > 0 {
            for lane in masters {
                sink.ring(lane).record_unfilled(missed.unfilled);
            }
            if let Some((lane, index)) = &self.follower {
                let skipped = lane.skip_unfilled(next.frames);
                if skipped > 0 {
                    sink.ring(*index).record_unfilled(skipped);
                }
            }
        }
    }
}

#[derive(Debug)]
pub enum StreamBody {
    Master {
        router: PacketRouter,
        sink: Arc<LaneFrameSink>,
    },
    Follower {
        lane: Arc<FollowerLane>,
        scratch: Box<[f32]>,
    },
}

impl StreamBody {
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
            StreamBody::Master { router, sink } => {
                router.route(packet, sink);
            }
            StreamBody::Follower { lane, scratch } => lane.push(packet, scratch),
        }
    }

    /// What the thread missed before `next`, its next packet, called for
    /// every packet flagged as a discontinuity. Real-time safe.
    #[inline(always)]
    pub fn write_missed(&mut self, next: &SliceView<'_>, missed: Missed) {
        match self {
            StreamBody::Master { router, sink } => router.write_missed(next, missed, sink),
            StreamBody::Follower { lane, .. } => lane.record_missed(missed),
        }
    }
}

/// What the backend's stop hands to `sink` once both capture threads are
/// joined: what each stream's clocks read was missed after its last packet
/// (unless the stop is for a rebuild), then what the follower's lane still
/// holds. `follower_end` is `None` when it was not read (endpoint loopback,
/// which idles, or a stream that failed); `Some` is a follower that
/// delivers continuously, so its padding at the stop counts as loss.
pub fn count_at_stop(
    sink: &LaneFrameSink,
    follower: Option<(&FollowerLane, usize)>,
    master_end: Missed,
    follower_end: Option<Missed>,
) {
    let follower_index = follower.map(|(_, index)| index);
    if !sink.device_change_reported() {
        if master_end.unfilled > 0 {
            for lane in (0..sink.lanes().len()).filter(|lane| Some(*lane) != follower_index) {
                sink.ring(lane).record_unfilled(master_end.unfilled);
            }
        }
        if let Some((lane, _)) = follower
            && let Some(end) = follower_end
        {
            lane.record_missed(end);
        }
    }
    if let Some((lane, index)) = follower {
        let held = lane.take_missed_at_stop();
        if held.fill > 0 {
            sink.ring(index).record_filled(held.fill);
        }
        if held.unfilled > 0 {
            sink.ring(index).record_unfilled(held.unfilled);
        }
    }
}
