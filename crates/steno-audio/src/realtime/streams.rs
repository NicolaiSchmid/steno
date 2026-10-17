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
//! than one period plus the master's buffer (the immediate slip), plus the
//! fill limit behind a master that fills what it missed
//! ([`FollowerLane::for_streams_filling`]). The plan's parity list tracks
//! both the missing resampling and this error.
//!
//! Audio a capture thread missed (A18; the design is in
//! [`realtime::missed`](super::missed)) goes through
//! [`StreamBody::write_missed`] before the packet that showed it. The
//! master fills it as a silent packet that still pulls the follower's
//! frames for that time, and its kept packets do not slip that audio first
//! ([`FollowerLane::for_streams_filling`]); the follower writes nothing,
//! since the master's pulls already pad its lane with zeros while it is
//! out, and those zeros count as its fill ([`FollowerLane::take_missed`]);
//! a follower still out at the stop, or whose flagged packet comes after
//! the master's last pull, counts at the backend's stop
//! ([`count_at_stop`]).
//! A master's jump too long to fill drops
//! the follower's audio for that time with it
//! ([`FollowerLane::skip_unfilled`]), and the lanes keep their lag. Three
//! shapes stay, each counted, none lost silently:
//!
//! - Both threads out (a stopped process) for less than the fill limit
//!   count the time once on each lane. Out longer, the master's unfilled
//!   jump and the follower's count the same time, and the session takes
//!   the lane that lost most, so it counts once too; a follower out for
//!   less than the fill limit inside a master's longer outage counts its
//!   part on top (an overcount, never a loss). The follower's padding
//!   counts up to the engine's buffer above what the engine dropped (the
//!   packets it kept land late and slip), an overcount of up to 100 ms per
//!   outage; an outage into the stop or overlapping the master's own
//!   reads up to two targets under, and one within two slip windows of an
//!   earlier outage whose late packets raised the queue also under by
//!   that excess ([`FollowerLane::take_missed`]).
//! - When the master's thread resumes first after such an outage, the
//!   follower's kept packets land late, as after the follower's own
//!   outage, until the queue slips back to the target within two slip
//!   windows, at most 3 840 frames (80 ms) slipped per outage.
//! - The master's thread alone out for 1.2 to 2.5 s (a suspension of the
//!   whole process does not make this): the follower's queue
//!   then passes the raised ceiling at the first pull of the master's kept
//!   packets and slips at once, so about 100 ms of microphone audio pairs
//!   with system audio from up to 1.3 s later, and the system audio
//!   slipped is counted as dropped. The Windows gate step of the stable
//!   plan checks it on hardware.
//!
//! Real-time: [`StreamBody::handle`] and [`StreamBody::write_missed`]
//! allocate nothing and take no lock; the scratch buffers are allocated
//! when the body is built, before the stream starts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::io_proc::{SliceView, deliver_slices};
use super::missed::{MISSED_FILL_LIMIT, Missed};
use super::ring::{LaneRingBuffer, saturating_add};
use super::sink::LaneFrameSink;
use crate::SAMPLE_RATE;
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
    /// Frames the follower's thread missed, filled and unfilled apart, not
    /// yet handed to the sink's counts ([`Self::take_missed`]).
    missed_fill: AtomicUsize,
    missed_unfilled: AtomicUsize,
    /// Zeros padded since the lane last primed (those before it first
    /// primed included) while the follower pushed nothing, and those of the
    /// runs before the last re-prime that the hold still keeps. A pull
    /// that finds a push since the last one pads the re-prime's wait, which
    /// is delay, not loss, and stays out of the run.
    run: AtomicUsize,
    last_run: AtomicUsize,
    /// Producer: frames the follower pushed (wrapping); consumer: their
    /// count at the last re-prime, at the last pull, and at the last pull
    /// that padded the run while the lane was out (a run that ends within
    /// one pull's shortfall needs no mark: no engine loss is that short).
    /// While fewer than `reprime_hold` came since the re-prime, `last_run`
    /// may still be a discontinuity's padding ([`Self::with_reprime_hold`]);
    /// a run the follower pushed more than that after, before the lane
    /// primed (the master was out), is forgotten at the re-prime.
    pushed: AtomicUsize,
    pushed_at_prime: AtomicUsize,
    pushed_at_pull: AtomicUsize,
    pushed_at_pad: AtomicUsize,
    reprime_hold: usize,
    /// The padding of a discontinuity that came while the lane was out: the
    /// held run (`held_run`) the master's first pull after it takes out of
    /// the runs, and what each later flag's first pull takes too;
    /// `usize::MAX` before that pull or with none pending.
    flag_run: AtomicUsize,
    /// Producer: the follower's queue at a flagged packet of the pending
    /// discontinuity was within the target and the hold, so it came back
    /// right before that flag.
    flag_fresh: AtomicBool,
    /// Producer: flagged packets recorded (wrapping); consumer: their count
    /// when `flag_run` was last added to, and when they were last handed on.
    /// A flag since the snapshot makes it stale: the next pull while the
    /// lane is out adds what padded since, and the stop takes it all. A
    /// flag since the hand-on is a discontinuity still pending: one
    /// recorded while the master hands the last one on stays pending.
    flags_recorded: AtomicUsize,
    flags_snapped: AtomicUsize,
    flags_handed: AtomicUsize,
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
            .with_reprime_hold(master_buffer.saturating_add(period))
    }

    /// [`Self::for_streams`] behind a master that writes what its thread
    /// missed as silence ([`StreamBody::write_missed`]), as the WASAPI
    /// backend's does: it can run late by its buffer and by up to
    /// [`MISSED_FILL_LIMIT`] more, whose fill pulls the follower's audio
    /// queued meanwhile, so that queue must not slip at once. The limit is
    /// counted at 48 kHz, the rate both WASAPI streams deliver at whatever
    /// the engine's.
    #[must_use]
    pub fn for_streams_filling(period: usize, master_buffer: usize) -> Self {
        let fill_limit = (MISSED_FILL_LIMIT.as_secs_f64() * SAMPLE_RATE) as usize;
        Self::for_streams(period, master_buffer)
            .with_max_lateness(master_buffer.saturating_add(fill_limit))
    }

    /// The most the master can run late, in frames; a queue more than this
    /// above the high-water mark after a pull slips to the target at once
    /// (see the module doc).
    #[must_use]
    pub fn with_max_lateness(mut self, frames: usize) -> Self {
        self.ceiling = self.high_water.saturating_add(frames);
        self
    }

    /// How many frames the follower may push after a re-prime while a
    /// discontinuity still counts the padding before it as its loss
    /// ([`Self::take_missed`]): the follower's buffer and one period, since
    /// an engine that refused the newest packets during an outage hands
    /// over the ones it kept unflagged and flags only the packet after
    /// them. The hold is counted in the follower's own frames, not the
    /// master's pulls, so a second stall, an idle or the master's catch-up
    /// burst between the kept packets and the flagged one does not use it
    /// up. Past it, that padding was a drift underrun or an idle no packet
    /// flagged, and is forgotten. [`Self::for_streams`] takes the
    /// master's buffer and period for it (both streams ask for the same
    /// buffer); [`Self::new`] the capacity.
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
    /// discontinuity: what the follower's clocks read was missed before it
    /// (`missed`, nothing when they read nothing), for
    /// [`Self::take_missed`]. It stages nothing: while the follower is out
    /// the master's pulls pad its lane with zeros, which keeps the lanes
    /// aligned, so that padding is the fill. A master out too (a stopped
    /// process) counts its own unfilled jump for the same time, and the
    /// session takes the lane that lost most, so that time counts once.
    #[inline(always)]
    pub fn record_missed(&self, missed: Missed) {
        saturating_add(&self.missed_fill, missed.fill);
        saturating_add(&self.missed_unfilled, missed.unfilled);
        // A later flag while one is pending can only make it fresh: a
        // second flag while the master is out must not drop the first's
        // padding, and a stale flag still pending (the lane never primed
        // since) must not drop a new outage's.
        let recorded = self.flags_recorded.load(Ordering::Relaxed);
        let fresh = self.ring.available_to_read() <= self.target.saturating_add(self.reprime_hold);
        if fresh || self.flags_handed.load(Ordering::Acquire) == recorded {
            self.flag_fresh.store(fresh, Ordering::Relaxed);
        }
        self.flags_recorded
            .store(recorded.wrapping_add(1), Ordering::Release);
    }

    /// Consumer (master thread). What the follower lost since the last
    /// call, for the sink's counts of the follower's lane. After a
    /// discontinuity, once the lane primed again, the zeros padded before
    /// that re-prime while the follower delivered nothing (not the
    /// re-prime's wait once it came back, which is delay, not loss) count
    /// as filled: the loss is the larger of that and what
    /// [`Self::record_missed`] counted, so it counts once, and whatever
    /// the follower's clocks read, positions that never move included. A
    /// drift underrun or an idle no packet flagged (no discontinuity)
    /// stays in [`Self::underrun_frames`]; one whose re-prime came less
    /// than the hold ([`Self::with_reprime_hold`], counted in the
    /// follower's frames) before a discontinuity counts with it, an
    /// overcount of that underrun or idle: two idles with less than the
    /// hold of audio between them count together when a flag follows the
    /// second, however long either was. A discontinuity that comes while
    /// the lane is out takes the padding up to the master's first pull
    /// after it (each later flag's first pull adds what padded since),
    /// with the run before the last re-prime while the hold keeps it (the
    /// engine's kept packets may run out before its flagged one comes),
    /// and not the re-prime's wait after it, so an idle right after the
    /// flag is not counted; what padded after the snapshot stays held for
    /// a later flag. A discontinuity whose follower had queued more than the
    /// target and the hold at each of its flags (it came back long before
    /// them, while the master was out) takes no padding at all; one flag
    /// that finds the queue within them is enough. The padding reads the
    /// loss to within the lane's lag: up to two targets under when the
    /// master's last pulls take the lag (an outage that ends within the lag
    /// before the stop, its flag at the final drain, or one overlapping the
    /// master's own); and any follower outage within two slip windows of
    /// an earlier one whose late packets raised the queue above the target
    /// reads also under by that excess, up to the engine's buffer less the
    /// target.
    #[inline(always)]
    pub fn take_missed(&self) -> Missed {
        let recorded = self.pending_flags();
        if recorded.is_some() && !self.primed.load(Ordering::Relaxed) {
            // The follower is back but the lane has not primed on its
            // audio yet: its padding is still running.
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
    /// a re-prime. It takes both runs, the current one and the one before
    /// the last re-prime, primed or not: a follower still out at the stop
    /// counts the zeros padded since it went out, and one that re-primed on
    /// the engine's kept packets and ran out again counts the outage before
    /// them too. After it, nothing is held.
    #[must_use]
    pub fn take_missed_at_stop(&self) -> Missed {
        let recorded = self.pending_flags();
        // Both runs, primed or not: an outage's padding sits in the last
        // run when the lane re-primed on the engine's kept packets and
        // then ran out again.
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
    /// pending (recorded since the last hand-on), `None` otherwise.
    #[inline(always)]
    fn pending_flags(&self) -> Option<usize> {
        let recorded = self.flags_recorded.load(Ordering::Acquire);
        // Pending is the count recorded against the count handed on, not a
        // flag the hand-on clears. This closes a race with the follower's
        // thread, which may record a flag anywhere between this read and
        // `hand_on`'s store of what it handed on:
        // - its count: `hand_on` stores only the count read here, so that
        //   flag stays pending for the next take (a cleared flag would drop
        //   it, and its outage's padding with it, silently);
        // - the freshness and the snapshot: `hand_on` reads both before
        //   that store. A flag recorded up to the store sees one pending,
        //   so it can only make it fresh, an overcount at most; one after
        //   it sees none and sets its own. With the store first, a flag in
        //   between would see none and overwrite the freshness of the flag
        //   being handed on, whose padding could then be dropped.
        (recorded != self.flags_handed.load(Ordering::Relaxed)).then_some(recorded)
    }

    /// What [`Self::record_missed`] counted, and after the discontinuities
    /// up to the `recorded`th flag the larger of that and the zeros in
    /// `run`, filled; the zeros capped by the run at the flag (see
    /// [`Self::take_missed`]). A flag recorded after `recorded` was read
    /// stays pending for the next call.
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
            // A queue past the target and the hold at the flag: the
            // follower came back long before it, so no padding is its loss
            // (and the hold has forgotten the runs: the follower pushed
            // more than it since the last re-prime).
            0
        } else if at_flag != usize::MAX {
            // The flag came while the lane was out: the padding up to it,
            // before the re-prime's wait. What padded after the snapshot
            // stays in `run` for a later flag within the hold: a new
            // outage, or the kept packets running out.
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
    /// frames that follows a stretch its thread missed and did not fill.
    /// Drops what is queued beyond the target and that packet, as a pull
    /// finds it in steady state: the follower's audio for that stretch, so
    /// the follower's lane loses the same span as the master's, while a
    /// packet the follower already queued for the time of `next` stays,
    /// and the lane keeps its lag. Returns the frames dropped.
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
    /// frames under the jitter-buffer policy (see the module doc) and
    /// returns the follower frames lost since the last pull (slips and
    /// staging overflows), for the sink's drop count.
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
            if let Some(recorded) = self.pending_flags()
                && self.flags_snapped.load(Ordering::Relaxed) != recorded
            {
                self.flags_snapped.store(recorded, Ordering::Relaxed);
                // The first pull since the follower flagged a loss while
                // its lane was out (or flagged again since the last
                // snapshot): the padding so far is that outage's, and what
                // pads after it is the re-prime's wait or a new stretch
                // without audio (an idle). The snapshot takes it out of the
                // runs, which hold only what pads after it.
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
                // a follower out from its first packets counts its padding;
                // with no discontinuity after it they are forgotten as any
                // re-prime's.
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
            // A re-prime within the hold of the last one (the lane primed on
            // part of the kept packets, ran out while the follower's thread
            // drained the rest, and primed again) keeps the run before it:
            // it may still be the outage's padding.
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
    /// re-prime, which [`Self::take_missed`] clears once the hold passed:
    /// an underrun between an outage's re-prime and its flag (the engine's
    /// kept packets ran out before the flagged one came) must not hide the
    /// outage's padding.
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
        let follower =
            follower.and_then(|follower| Some((follower, Self::follower_lane(&sources)?)));
        Self {
            sources,
            follower,
            scratch: vec![0.0; max_frames.max(1)].into_boxed_slice(),
        }
    }

    /// The lane of `sources` the follower's staging feeds: the one reading
    /// buffer 1.
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
    /// fill goes in as a silent packet of `next`'s channels: zeros on the
    /// master's lanes, counted as filled, and the follower's frames for the
    /// same time on its lane (it kept delivering). The unfilled frames
    /// count on the master's lanes, and the follower's audio queued for
    /// that time is dropped and counted on its lane
    /// ([`FollowerLane::skip_unfilled`]).
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
            StreamBody::Master { router, sink } => {
                router.route(packet, sink);
            }
            StreamBody::Follower { lane, scratch } => lane.push(packet, scratch),
        }
    }

    /// What the thread missed before `next`, its next packet, called for
    /// every packet flagged as a discontinuity, `missed` none included:
    /// [`PacketRouter::write_missed`] on the master,
    /// [`FollowerLane::record_missed`] on the follower (see the module doc).
    /// Real-time safe.
    #[inline(always)]
    pub fn write_missed(&mut self, next: &SliceView<'_>, missed: Missed) {
        match self {
            StreamBody::Master { router, sink } => router.write_missed(next, missed, sink),
            StreamBody::Follower { lane, .. } => lane.record_missed(missed),
        }
    }
}

/// What the backend's stop hands to `sink` once both capture threads are
/// joined, so no loss at the end of a recording is silent: what each
/// stream's clocks read was missed after its last packet (`master_end` and
/// `follower_end`), unfilled, on the lanes that stream feeds, unless the
/// session stops for a rebuild, whose gap covers that time; then what the
/// follower's lane (`follower`, with its lane in the sink) still holds
/// ([`FollowerLane::take_missed_at_stop`]), the follower's end included.
/// `follower_end` is `None` when the follower's end was not read: endpoint
/// loopback, which idles, or a stream that failed. `Some` is a follower
/// that delivers continuously (process loopback), so zeros still padding
/// its lane at the stop are its loss whether or not a packet flagged it:
/// the lane counts them as after a discontinuity.
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
