//! The two-stream capture path the WASAPI backend runs, without WASAPI:
//! the stream plan, latency arithmetic, trusted engine answers, the
//! follower's jitter buffer and the master's routing, over synthetic
//! packets. On the Windows runner, which has no audio device, these are the
//! backend's unit tests.

// Test arithmetic: sample counts cast freely, and samples compare exactly
// on purpose (the path copies, folds by halves or writes zeros).
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::float_cmp,
    clippy::doc_markdown
)]

use std::collections::BTreeMap;
use std::sync::Arc;

use steno_audio::capture::split_streams::{
    DevicePositions, MAX_BUFFER_FRAMES, MOST_READ_FRAMES, Packet, QPC_CALIBRATION_FRAMES,
    StreamSizes, far_end_latencies, frames_from_hundred_nanoseconds, stream_sizes,
};
use steno_audio::capture::{ChannelRef, DeviceChangeReason, SplitStreamPlan, StreamSource};
use steno_audio::realtime::{
    FollowerLane, LaneFrameSink, LaneRingBuffer, MISSED_FILL_LIMIT, Missed, PacketRouter,
    SliceView, StreamBody, count_at_stop, deliver_slices,
};
use steno_audio::{CaptureError, CaptureMode, SAMPLE_RATE};
use steno_core::AudioLane;

/// `frames` mono frames counting up from `start`.
fn ramp(start: usize, frames: usize) -> Vec<f32> {
    (start..start + frames).map(|i| i as f32).collect()
}

/// `frames` stereo frames: left counts up from `start`, right is left + 2,
/// so the fold is `ramp(start + 1, frames)`.
fn stereo_ramp(start: usize, frames: usize) -> Vec<f32> {
    (start..start + frames)
        .flat_map(|i| [i as f32, i as f32 + 2.0])
        .collect()
}

fn packet(samples: &[f32], channels: usize) -> SliceView<'_> {
    SliceView {
        frames: samples.len() / channels,
        channels,
        samples: Some(samples),
    }
}

/// A packet flagged silent: its length, no samples.
fn silent(frames: usize, channels: usize) -> SliceView<'static> {
    SliceView {
        frames,
        channels,
        samples: None,
    }
}

fn master_body(
    plan: &SplitStreamPlan,
    follower: &Arc<FollowerLane>,
    max_frames: usize,
    sink: &Arc<LaneFrameSink>,
) -> StreamBody {
    StreamBody::Master {
        router: PacketRouter::new(
            plan.layout.sources.clone(),
            Some(Arc::clone(follower)),
            max_frames,
        ),
        sink: Arc::clone(sink),
    }
}

fn drain(sink: &LaneFrameSink, lane: usize) -> Vec<f32> {
    sink.ring(lane).drain_all()
}

/// A call masters on the microphone and follows the system stream, staged
/// as mono; in person opens the microphone alone; a system-only capture
/// masters on the stereo system stream; no lanes is refused.
#[test]
fn the_stream_plan_masters_on_the_microphone_when_there_is_one() {
    let mono = |stream| ChannelRef::new(stream, 0, 1);
    for (lanes, master, follower, sources) in [
        (
            CaptureMode::Call.lanes(),
            StreamSource::Microphone,
            Some(StreamSource::System),
            vec![
                (AudioLane::Mic, mono(0), None),
                (AudioLane::System, mono(1), None),
            ],
        ),
        (
            CaptureMode::InPerson.lanes(),
            StreamSource::Microphone,
            None,
            vec![(AudioLane::Mixed, mono(0), None)],
        ),
        (
            vec![AudioLane::System],
            StreamSource::System,
            None,
            vec![(
                AudioLane::System,
                ChannelRef::new(0, 0, 2),
                Some(ChannelRef::new(0, 1, 2)),
            )],
        ),
    ] {
        let plan = SplitStreamPlan::new(&lanes).unwrap();
        assert_eq!(
            (plan.master, plan.follower),
            (master, follower),
            "{lanes:?}"
        );
        let got: Vec<_> = plan
            .layout
            .sources
            .iter()
            .map(|source| (source.lane, source.left, source.right))
            .collect();
        assert_eq!(got, sources, "{lanes:?}");
    }
    assert!(matches!(
        SplitStreamPlan::new(&[]),
        Err(CaptureError::UnexpectedStreamLayout(_))
    ));
}

/// The packets that measure `QPC_CALIBRATION_FRAMES`: one more than fit,
/// since each advance is read at the next packet.
const CALIBRATION_PACKETS: u64 = QPC_CALIBRATION_FRAMES / 480 + 1;

/// The QPC's 100 ns units in one 10 ms packet.
const QPC_PACKET: u64 = 100_000;

/// One WASAPI stream's clocks, 10 ms packets: the device position advances
/// `per_packet` a packet, the QPC `qpc_per_packet` (0: none).
struct Clocks {
    positions: DevicePositions,
    next: Packet,
    per_packet: u64,
    qpc_per_packet: u64,
}

impl Clocks {
    fn new(engine_rate: Option<u32>, per_packet: u64, qpc_per_packet: u64) -> Self {
        Self {
            positions: DevicePositions::new(engine_rate),
            next: Packet {
                frames: 480,
                position: 1_000_000,
                qpc: if qpc_per_packet > 0 { 1 << 40 } else { 0 },
                ..Packet::default()
            },
            per_packet,
            qpc_per_packet,
        }
    }

    /// Moves the clocks on by `packets` packets that are not delivered.
    fn skip(&mut self, packets: u64) -> &mut Self {
        self.next.position += packets * self.per_packet;
        if self.next.qpc > 0 {
            self.next.qpc += packets * self.qpc_per_packet;
        }
        self
    }

    /// One packet, flagged or not: what it reports.
    fn deliver(&mut self, discontinuity: bool, timestamp_error: bool) -> Missed {
        let packet = Packet {
            discontinuity,
            timestamp_error,
            ..self.next
        };
        let missed = self.positions.packet(packet);
        self.skip(1);
        missed
    }

    fn clean(&mut self, packets: u64) -> &mut Self {
        for _ in 0..packets {
            assert!(self.deliver(false, false).is_none(), "a clean packet");
        }
        self
    }

    /// `packets` packets lost, then the flagged packet: what it reports.
    fn lose(&mut self, packets: u64) -> Missed {
        self.skip(packets).deliver(true, false)
    }
}

fn filled(fill: usize) -> Missed {
    Missed { fill, unfilled: 0 }
}

fn unfilled(unfilled: usize) -> Missed {
    Missed { fill: 0, unfilled }
}

/// That `missed` counts `lost` unfilled, or up to 1 % more.
#[track_caller]
fn assert_counted(missed: Missed, lost: usize, context: &str) {
    assert!(
        missed.fill == 0 && (lost..=lost + lost / 100 + 1).contains(&missed.unfilled),
        "{context}: {missed:?} for {lost}"
    );
}

/// A sane QPC fills a discontinuity's loss exactly, up to the fill limit,
/// whatever unit the positions count in; before two seconds it only counts.
#[test]
fn a_sane_qpc_fills_exactly_what_was_lost_whatever_the_positions() {
    for (engine_rate, per_packet) in [
        (Some(48_000), 480),
        (Some(16_000), 160),
        (Some(8_000), 80),
        (Some(96_000), 960),
        (Some(192_000), 1_920),
        (None, 160),
        (Some(48_000), 0),
    ] {
        let name = format!("{per_packet} a packet on {engine_rate:?}");
        let mut clocks = Clocks::new(engine_rate, per_packet, QPC_PACKET);
        let early = clocks.clean(10).lose(50);
        assert_eq!(early.fill, 0, "{name}: still measured, so counted");
        assert!(early.unfilled >= 24_000, "{name}: {early:?}");
        clocks.clean(CALIBRATION_PACKETS);
        assert!(clocks.positions.fills(), "{name}");
        assert_eq!(clocks.lose(50), filled(24_000), "{name}");
        assert_eq!(clocks.clean(1).lose(100), filled(48_000), "{name}");
        assert_eq!(clocks.clean(1).lose(150), unfilled(72_000), "{name}");
        assert!(clocks.clean(1).deliver(true, false).is_none(), "{name}");
    }
}

/// Without a QPC a discontinuity counts unfilled at the largest position
/// reading, so the right unit reads the loss and others overcount; frozen
/// positions read nothing.
#[test]
fn without_a_sane_qpc_the_largest_reading_is_counted_unfilled() {
    for (engine_rate, per_packet, lost) in [
        (Some(48_000), 480, 24_000),
        (Some(16_000), 160, 24_000),
        (None, 160, 24_000),
        (Some(44_100), 480, 26_165),
        (Some(44_100), 441, 24_000),
        (Some(48_000), 960, 51 * 960 - 480),
        (Some(48_000), 0, 0),
    ] {
        let name = format!("{per_packet} a packet on {engine_rate:?}");
        for warm in [2, CALIBRATION_PACKETS] {
            let mut clocks = Clocks::new(engine_rate, per_packet, 0);
            assert_counted(
                clocks.clean(warm).lose(50),
                lost,
                &format!("{name}, {warm}"),
            );
            assert!(!clocks.positions.fills(), "{name}");
        }
    }
}

/// A ratio measured over one clean pair errs over, never under.
#[test]
fn a_ratio_measured_over_one_pair_errs_over() {
    for start in 0..48 {
        let mut positions = DevicePositions::new(Some(48_000));
        let at = |frames: u64| (start * 1_000 + frames) * 10_668 / 48_000;
        let packet = |frames, discontinuity| Packet {
            frames: 480,
            position: at(frames),
            discontinuity,
            ..Packet::default()
        };
        assert!(positions.packet(packet(0, false)).is_none());
        assert!(positions.packet(packet(480, false)).is_none());
        let missed = positions.packet(packet(960 + 8_640, true));
        assert!(missed.unfilled >= 8_640, "start {start}: {missed:?}");
    }
}

/// A QPC off by 2.5 % or more, or frozen, never fills; one 1.5 % fast is
/// sane.
#[test]
fn a_qpc_off_by_more_than_2_percent_never_fills() {
    for (qpc_per_packet, fills) in [
        (200_000, false),
        (102_500, false),
        (98_500, true),
        (101_500, true),
    ] {
        let mut clocks = Clocks::new(Some(48_000), 480, qpc_per_packet);
        clocks.clean(2 * CALIBRATION_PACKETS);
        assert_eq!(clocks.positions.fills(), fills, "{qpc_per_packet}");
        let missed = clocks.lose(50);
        if fills {
            assert_eq!(missed.unfilled, 0, "{qpc_per_packet}");
            assert!(missed.fill.abs_diff(24_000) <= 24_500 / 50, "{missed:?}");
        } else {
            assert_counted(missed, 24_000, &format!("{qpc_per_packet}: the positions"));
        }
    }
    let mut stuck = Clocks::new(Some(48_000), 0, QPC_PACKET);
    stuck.next.qpc = 1 << 40;
    stuck.qpc_per_packet = 0;
    stuck.clean(2 * CALIBRATION_PACKETS);
    assert!(!stuck.positions.fills());
    assert!(stuck.lose(50).is_none(), "neither clock moves");
}

/// A timestamp error rules out the QPC only: flagged packets near it count
/// from the device position.
#[test]
fn a_timestamp_error_rules_out_only_the_qpc() {
    for (qpc_per_packet, warm) in [
        (QPC_PACKET, CALIBRATION_PACKETS),
        (QPC_PACKET, 5),
        (0, CALIBRATION_PACKETS),
    ] {
        let name = format!("QPC {qpc_per_packet}, {warm} packets");
        let mut clocks = Clocks::new(Some(48_000), 480, qpc_per_packet);
        clocks.clean(warm);
        assert_counted(
            clocks.skip(50).deliver(true, true),
            24_000,
            &format!("{name}: the flag on the flagged packet"),
        );
        assert_counted(
            clocks.skip(50).deliver(true, true),
            24_000,
            &format!("{name}: and on the one after it"),
        );
        assert!(clocks.deliver(false, true).is_none());
        assert_counted(
            clocks.lose(50),
            24_000,
            &format!("{name}: the flag just before"),
        );
        clocks.clean(1);
        assert_eq!(
            clocks.lose(50).fill,
            if qpc_per_packet > 0 && warm > 5 {
                24_000
            } else {
                0
            },
            "{name}: the QPC reads again"
        );
    }
    let mut stuck = Clocks::new(Some(48_000), 0, QPC_PACKET);
    stuck.clean(CALIBRATION_PACKETS);
    assert!(stuck.skip(50).deliver(true, true).is_none());
}

/// Packets with a timestamp error stay out of both measurements.
#[test]
fn timestamp_errors_are_left_out_of_the_measurement() {
    let mut clocks = Clocks::new(Some(48_000), 480, QPC_PACKET);
    for _ in 0..100 {
        assert!(clocks.deliver(false, false).is_none());
        clocks.next.frames = 240;
        assert!(clocks.deliver(false, true).is_none());
        clocks.next.frames = 480;
    }
    assert_eq!(
        clocks.positions.ratio(),
        None,
        "no two clean packets in a row"
    );
    assert_eq!(clocks.positions.qpc_ratio(), None);
    clocks.clean(CALIBRATION_PACKETS);
    assert_eq!(clocks.positions.ratio(), Some(1.0));
    assert_eq!(clocks.positions.qpc_ratio(), Some(1.0));
    assert!(clocks.positions.fills());
}

/// An unflagged idle and a step back stay out of both measurements, so a
/// later loss fills exactly.
#[test]
fn an_idle_gap_or_a_step_back_does_not_skew_the_measurement() {
    for (engine_rate, per_packet, idle) in [
        (Some(48_000), 480, 2),
        (Some(48_000), 480, 1_000),
        (Some(96_000), 960, 2),
    ] {
        let name = format!("{idle} packets idle at {per_packet} a packet");
        let mut clocks = Clocks::new(engine_rate, per_packet, QPC_PACKET);
        clocks.clean(10).skip(idle).clean(CALIBRATION_PACKETS);
        assert_eq!(
            clocks.positions.ratio(),
            Some(per_packet as f64 / 480.0),
            "{name}"
        );
        assert_eq!(clocks.positions.qpc_ratio(), Some(1.0), "{name}");
        assert_eq!(clocks.lose(50), filled(24_000), "{name}");
    }
    let mut clocks = Clocks::new(Some(48_000), 480, QPC_PACKET);
    clocks.clean(10);
    clocks.next.position -= 480_000;
    clocks.next.qpc -= 100_000_000;
    clocks.clean(CALIBRATION_PACKETS);
    assert_eq!(clocks.positions.ratio(), Some(1.0), "10 s back");
    assert_eq!(clocks.lose(50), filled(24_000));
}

/// Once positions moved, a clean packet past 1.5 times the measured ratio
/// stays out of the sums.
#[test]
fn an_idle_inside_the_rate_bound_stays_out_once_the_ratio_is_measured() {
    let mut clocks = Clocks::new(Some(96_000), 160, 0);
    clocks.clean(10).skip(5).clean(200);
    assert_eq!(clocks.positions.ratio(), Some(160.0 / 480.0));
    assert_counted(clocks.lose(50), 24_000, "after the idle");
}

/// A QPC off by more than 2 % over two seconds is not read, even when its
/// last pair matched.
#[test]
fn a_qpc_measured_off_is_not_read_even_after_a_matching_pair() {
    let mut clocks = Clocks::new(Some(48_000), 480, QPC_PACKET);
    for packet in 0..2 * CALIBRATION_PACKETS {
        assert!(clocks.deliver(false, false).is_none());
        if packet % 2 == 0 {
            clocks.next.qpc += 40_000;
        }
    }
    clocks.clean(1);
    assert!(!clocks.positions.fills());
    clocks.skip(50);
    clocks.next.qpc += 2 * 50 * QPC_PACKET;
    assert_counted(clocks.deliver(true, false), 24_000, "the positions");
}

/// Positions that jitter by a few frames stay in the measurement.
#[test]
fn positions_that_jitter_stay_in_the_measurement() {
    let mut clocks = Clocks::new(None, 480, 0);
    for packet in 0..CALIBRATION_PACKETS {
        assert!(clocks.deliver(false, false).is_none());
        clocks.next.position -= if packet % 2 == 0 { 10 } else { 0 };
        clocks.next.position += if packet % 2 == 0 { 0 } else { 10 };
    }
    assert_eq!(clocks.positions.ratio(), Some(1.0));
}

/// A flagged packet counts nothing for clocks behind the last packet or
/// less than half a packet past its end; more counts in full.
#[test]
fn a_flagged_packet_counts_only_a_jump_of_more_than_half_a_packet() {
    for qpc_per_packet in [0, QPC_PACKET] {
        let mut clocks = Clocks::new(Some(48_000), 480, qpc_per_packet);
        clocks.clean(CALIBRATION_PACKETS);
        let next = clocks.next;
        clocks.next.position -= 5_280;
        clocks.next.qpc = clocks.next.qpc.saturating_sub(1_100_000);
        assert!(
            clocks.deliver(true, false).is_none(),
            "{qpc_per_packet}: behind"
        );
        clocks.next = next;
        clocks.clean(1);
        clocks.next.position += 240;
        clocks.next.qpc += qpc_per_packet / 2;
        assert!(clocks.deliver(true, false).is_none(), "half a packet");
        clocks.next.position += 241;
        clocks.next.qpc += qpc_per_packet * 241 / 480;
        let missed = clocks.deliver(true, false);
        assert_eq!(
            missed.fill + missed.unfilled,
            241,
            "{qpc_per_packet}: {missed:?}"
        );
    }
}

/// A garbage reading counts at most an hour, and the sink's counts
/// saturate.
#[test]
fn a_garbage_reading_is_capped_and_the_counters_saturate() {
    let mut clocks = Clocks::new(Some(48_000), 480, 0);
    clocks.clean(10);
    clocks.next.position += u64::MAX / 4;
    let missed = clocks.deliver(true, false);
    assert_eq!(missed, unfilled(MOST_READ_FRAMES as usize));
    let mut qpc = Clocks::new(Some(48_000), 0, QPC_PACKET);
    qpc.clean(CALIBRATION_PACKETS);
    qpc.next.qpc += u64::MAX / 4;
    assert_eq!(
        qpc.deliver(true, false),
        unfilled(MOST_READ_FRAMES as usize)
    );
    let ring = LaneRingBuffer::new(16);
    ring.record_unfilled(usize::MAX - 1);
    ring.record_unfilled(usize::MAX - 1);
    ring.record_filled(usize::MAX);
    ring.record_filled(1);
    assert_eq!(
        (ring.unfilled_samples(), ring.filled_samples()),
        (usize::MAX, usize::MAX)
    );
}

/// Frozen positions never panic, whatever the mix format says.
#[test]
fn positions_that_never_move_divide_nothing_by_zero() {
    for engine_rate in [None, Some(0), Some(16_000), Some(48_000)] {
        let mut clocks = Clocks::new(engine_rate, 0, 0);
        clocks.clean(2 * CALIBRATION_PACKETS);
        assert_eq!(
            clocks.positions.engine_rate(),
            engine_rate.filter(|r| *r > 0)
        );
        assert_eq!(clocks.positions.ratio(), Some(0.0));
        assert!(!clocks.positions.fills());
        assert!(clocks.lose(50).is_none(), "{engine_rate:?}");
        clocks.next.frames = 0;
        assert!(clocks.deliver(true, false).is_none());
        clocks.next.frames = 480;
        assert!(clocks.lose(50).is_none(), "after a packet of no frames");
    }
}

/// The stop's line names both clocks; frozen positions say "0.0000 a
/// frame".
#[test]
fn the_stop_line_names_both_clocks() {
    let mut clocks = Clocks::new(Some(48_000), 0, QPC_PACKET);
    clocks.clean(CALIBRATION_PACKETS);
    assert_eq!(
        clocks.positions.to_string(),
        "device positions advanced 0 over 96000 frames delivered (0.0000 a frame, engine at \
         48000 Hz), QPC 96000 frames over 96000 (1.0000 a frame), jumps filled"
    );
}

/// What [`over_positions`] saw, and the seconds the recorder's note would
/// say are missing.
#[derive(Debug)]
struct OverPositions {
    reported: Missed,
    filled: BTreeMap<AudioLane, usize>,
    unfilled: BTreeMap<AudioLane, usize>,
    underrun: usize,
    mic_frames: usize,
    system_frames: usize,
    note: f64,
}

/// The two stream bodies of a call over 6 s of 10 ms ticks, each read by
/// its own [`DevicePositions`]: the follower out for `follower_out` ticks
/// from `from`, or the master for `master_out`, then a flagged packet.
fn over_positions(
    engine_rate: Option<u32>,
    (per_packet, qpc_per_packet): (u64, u64),
    from: usize,
    follower_out: usize,
    master_out: usize,
) -> OverPositions {
    let lanes = [AudioLane::Mic, AudioLane::System];
    let plan = SplitStreamPlan::new(&lanes).unwrap();
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let follower = Arc::new(FollowerLane::for_streams_filling(PERIOD, 4_800));
    let mut master = master_body(&plan, &follower, 4_800, &sink);
    let mut staging = StreamBody::follower(Arc::clone(&follower), 4_800);
    let (mut mic_positions, mut system_positions) = (
        DevicePositions::new(engine_rate),
        DevicePositions::new(engine_rate),
    );
    let mic = vec![0.5f32; PERIOD];
    let tap = vec![0.25f32; 2 * PERIOD];
    let mut reported = Missed::default();
    let mut run = |body: &mut StreamBody,
                   positions: &mut DevicePositions,
                   next: SliceView<'_>,
                   tick: usize,
                   out: usize| {
        if (from..from + out).contains(&tick) {
            return;
        }
        let flagged = out > 0 && tick == from + out;
        let tick = tick as u64;
        let missed = positions.packet(Packet {
            frames: PERIOD,
            position: tick * per_packet,
            qpc: if qpc_per_packet > 0 {
                (1 << 40) + tick * qpc_per_packet
            } else {
                0
            },
            discontinuity: flagged,
            timestamp_error: false,
        });
        if flagged {
            reported = missed;
            body.write_missed(&next, missed);
        }
        body.handle(next);
    };
    let ticks = 600;
    let (mut mic_frames, mut system_frames) = (0, 0);
    for tick in 0..ticks {
        run(
            &mut staging,
            &mut system_positions,
            packet(&tap, 2),
            tick,
            follower_out,
        );
        run(
            &mut master,
            &mut mic_positions,
            packet(&mic, 1),
            tick,
            master_out,
        );
        mic_frames += drain(&sink, 0).len();
        system_frames += drain(&sink, 1).len();
    }
    let (filled, unfilled) = (sink.filled_samples(), sink.unfilled_samples());
    let most = |counts: &BTreeMap<AudioLane, usize>| {
        counts.values().max().copied().unwrap_or(0) as f64 / SAMPLE_RATE
    };
    let wall = (ticks * PERIOD) as f64 / SAMPLE_RATE;
    let short = wall - mic_frames as f64 / SAMPLE_RATE - wall * 0.001;
    OverPositions {
        reported,
        note: most(&filled) + most(&unfilled).max(short),
        filled,
        unfilled,
        underrun: follower.underrun_frames(),
        mic_frames,
        system_frames,
    }
}

fn on_the_system(frames: usize) -> BTreeMap<AudioLane, usize> {
    (frames > 0)
        .then_some((AudioLane::System, frames))
        .into_iter()
        .collect()
}

/// The system stream's thread out for 0.5 to 1.5 s: the master's padding
/// counts as filled whatever the follower's clocks read, the note counts
/// the loss, and the lanes keep one length.
#[test]
fn a_follower_loss_is_counted_whatever_its_clocks_read() {
    for (name, rate, clocks, from) in [
        ("a sane QPC", Some(48_000), (480, QPC_PACKET), 300),
        ("16 kHz positions", None, (160, 0), 300),
        ("positions that never move", Some(48_000), (0, 0), 300),
        ("early", Some(48_000), (480, QPC_PACKET), 50),
    ] {
        for out in [50, 90, 150] {
            let seen = over_positions(rate, clocks, from, out, 0);
            let lost = out * PERIOD;
            let padded = lost - 960;
            let (read, qpc_fill) = match clocks {
                (0, 0) => (0, 0),
                (_, 0) => (lost, 0),
                _ if from < 200 => (lost, 0),
                _ => (lost, if lost <= 48_000 { lost } else { 0 }),
            };
            let fill = padded.max(qpc_fill);
            assert_eq!(
                seen.filled,
                on_the_system(fill),
                "{name}, out {out}: {seen:?}"
            );
            let rest = read.max(padded) - fill;
            let unfilled = seen.unfilled.get(&AudioLane::System).copied().unwrap_or(0);
            assert!(
                (rest..=rest + read / 100 + 1).contains(&unfilled),
                "{name}, out {out}: {seen:?}"
            );
            let note = seen.note * SAMPLE_RATE;
            assert!(
                note >= padded as f64 - 1e-6 && note <= (lost + lost / 100 + 1) as f64,
                "{name}, out {out}: the note says {} s",
                seen.note
            );
            assert_eq!(
                (seen.mic_frames, seen.system_frames),
                (600 * PERIOD, 600 * PERIOD)
            );
        }
    }
}

/// A drift underrun stays an underrun, not missed audio.
#[test]
fn a_drift_underrun_is_not_counted_as_missed() {
    let lanes = [AudioLane::Mic, AudioLane::System];
    let plan = SplitStreamPlan::new(&lanes).unwrap();
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let follower = Arc::new(FollowerLane::for_streams_filling(PERIOD, 4_800));
    let mut master = master_body(&plan, &follower, 4_800, &sink);
    let mut staging = StreamBody::follower(Arc::clone(&follower), 4_800);
    let mic = vec![0.5f32; PERIOD];
    let tap = vec![0.25f32; 2 * PERIOD];
    let ticks =
        |range: std::ops::Range<usize>, staging: &mut StreamBody, master: &mut StreamBody| {
            for tick in range {
                // The follower's clock runs slow: it skips a packet every second.
                if tick % 100 != 99 {
                    staging.handle(packet(&tap, 2));
                }
                master.handle(packet(&mic, 1));
            }
        };
    ticks(0..300, &mut staging, &mut master);
    assert!(follower.underrun_frames() > 0);
    assert!(sink.filled_samples().is_empty() && sink.unfilled_samples().is_empty());
    // A discontinuity with a loss its clocks read, counted once.
    staging.write_missed(&packet(&tap, 2), filled(480));
    ticks(300..310, &mut staging, &mut master);
    assert_eq!(sink.filled_samples(), on_the_system(480));
    let underrun = follower.underrun_frames();
    ticks(310..600, &mut staging, &mut master);
    assert!(follower.underrun_frames() > underrun);
    assert_eq!(sink.filled_samples(), on_the_system(480), "nothing more");
}

/// The follower reports a discontinuity before it queues the packet; the
/// padding and the count are taken together, once.
#[test]
fn a_discontinuity_waits_for_the_lane_to_prime() {
    let follower = FollowerLane::for_streams(PERIOD, 4_800);
    let tap = vec![0.25f32; 2 * PERIOD];
    let mut scratch = vec![0.0f32; PERIOD];
    let mut out = vec![0.0f32; PERIOD];
    for _ in 0..4 {
        follower.push(packet(&tap, 2), &mut scratch);
    }
    for _ in 0..53 {
        follower.pull(&mut out);
    }
    assert_eq!(follower.underrun_frames(), 50 * PERIOD, "out for 50 pulls");
    follower.record_missed(unfilled(24_000));
    assert!(
        follower.take_missed().is_none(),
        "the lane is still padding"
    );
    for _ in 0..4 {
        follower.push(packet(&tap, 2), &mut scratch);
    }
    follower.pull(&mut out);
    let missed = follower.take_missed();
    assert_eq!(missed.fill + missed.unfilled, 24_000, "{missed:?}");
    assert!(missed.fill >= 23_000, "the padding is the fill: {missed:?}");
    assert!(follower.take_missed().is_none(), "once");
}

/// The microphone's thread out for 1.5 s with no QPC: counted unfilled, and
/// the system audio queued meanwhile dropped, so the lanes keep one length.
#[test]
fn a_master_loss_without_a_qpc_is_counted() {
    for from in [50, 300] {
        let seen = over_positions(Some(48_000), (480, 0), from, 0, 150);
        let lost = 150 * PERIOD;
        assert_counted(seen.reported, lost, "the flagged packet");
        assert!(seen.filled.is_empty(), "{seen:?}");
        assert_eq!(
            seen.unfilled.get(&AudioLane::Mic),
            Some(&seen.reported.unfilled)
        );
        assert_eq!(
            seen.mic_frames,
            450 * PERIOD,
            "the master falls short by it"
        );
        assert_eq!(
            seen.system_frames, seen.mic_frames,
            "the lanes keep one length"
        );
        assert!(
            (seen.note - seen.reported.unfilled as f64 / SAMPLE_RATE).abs() < 1e-9,
            "the note says {} s",
            seen.note
        );
        assert_eq!(seen.underrun, 0);
    }
}

#[test]
fn reference_times_convert_to_frames_at_48_khz() {
    assert_eq!(frames_from_hundred_nanoseconds(100_000, 48_000.0), 480);
    assert_eq!(frames_from_hundred_nanoseconds(1, 48_000.0), 0);
    assert_eq!(frames_from_hundred_nanoseconds(105, 48_000.0), 1);
    assert_eq!(frames_from_hundred_nanoseconds(0, 48_000.0), 0);
    assert_eq!(frames_from_hundred_nanoseconds(-5, 48_000.0), 0);
}

#[test]
fn the_follower_delay_comes_off_the_output_latency_first() {
    assert_eq!(far_end_latencies(500, 2_000, 960), (500, 1_040));
    assert_eq!(far_end_latencies(500, 400, 960), (0, 0));
    assert_eq!(far_end_latencies(1_000, 400, 960), (440, 0));
    assert_eq!(far_end_latencies(500, 2_000, 0), (500, 2_000));
}

#[test]
fn plausible_engine_answers_are_taken_as_they_are() {
    let sizes = |buffer, period, latency| stream_sizes(buffer, Some(period), Some(latency));
    assert_eq!(
        sizes(4_800, 100_000, 300_000),
        StreamSizes {
            buffer_frames: 4_800,
            period_frames: 480,
            latency_frames: 1_440,
        }
    );
    // The bounds themselves: a 1 ms and a 100 ms period, 200 ms of latency.
    assert_eq!(sizes(4_800, 10_000, 2_000_000).period_frames, 48);
    assert_eq!(sizes(4_800, 10_000, 2_000_000).latency_frames, 9_600);
    assert_eq!(sizes(4_800, 1_000_000, 0).period_frames, 4_800);
    // A polled stream's 50 ms poll.
    assert_eq!(sizes(4_800, 500_000, 0).period_frames, 2_400);
}

#[test]
fn implausible_engine_answers_fall_back() {
    let period = |period| stream_sizes(4_800, period, None).period_frames;
    for untrusted in [
        None,
        Some(0),
        Some(-1),
        Some(1),
        Some(9_999),
        Some(1_000_001),
    ] {
        assert_eq!(period(untrusted), 480, "{untrusted:?} is taken for 10 ms");
    }
    let latency = |latency| stream_sizes(4_800, Some(100_000), latency).latency_frames;
    for untrusted in [None, Some(-1), Some(2_000_001), Some(i64::MAX)] {
        assert_eq!(
            latency(untrusted),
            0,
            "{untrusted:?} counts as no latency, not as the most"
        );
    }
    // A buffer of 0 or a huge one: between one period and one second.
    assert_eq!(stream_sizes(0, Some(100_000), None).buffer_frames, 480);
    assert_eq!(
        stream_sizes(usize::MAX, Some(100_000), None).buffer_frames,
        MAX_BUFFER_FRAMES
    );
}

#[test]
fn the_follower_writes_zeros_until_primed_then_keeps_the_target_queued() {
    let follower = FollowerLane::new(960, 2_880, 4_800, 48_000);
    let mut scratch = vec![0.0f32; 480];
    let mut out = vec![1.0f32; 480];

    follower.push(packet(&ramp(0, 480), 1), &mut scratch);
    assert_eq!(follower.pull(&mut out), 0);
    assert!(out.iter().all(|s| *s == 0.0), "not primed: zeros");
    assert_eq!(follower.queued(), 480, "nothing consumed while priming");

    follower.push(packet(&ramp(480, 480), 1), &mut scratch);
    follower.push(packet(&ramp(960, 480), 1), &mut scratch);
    follower.pull(&mut out);
    assert_eq!(out, ramp(0, 480), "primed: the oldest frames");
    assert_eq!(follower.queued(), 960, "the target stays queued");
    assert_eq!(follower.underrun_frames(), 0);
    assert_eq!(follower.target(), 960);
}

#[test]
fn a_follower_that_falls_short_pads_counts_and_reprimes() {
    let follower = FollowerLane::new(480, 1_440, 4_800, 48_000);
    let mut scratch = vec![0.0f32; 480];
    let mut out = vec![0.0f32; 480];
    follower.push(packet(&ramp(0, 960), 1), &mut scratch);
    follower.pull(&mut out);
    assert_eq!(out, ramp(0, 480));

    // 480 queued; ask for 600: 480 real, 120 zeros, then re-prime.
    let mut longer = vec![0.0f32; 600];
    follower.pull(&mut longer);
    assert_eq!(&longer[..480], ramp(480, 480).as_slice());
    assert!(longer[480..].iter().all(|s| *s == 0.0));
    assert_eq!(follower.underrun_frames(), 120);

    follower.push(packet(&ramp(2_000, 480), 1), &mut scratch);
    follower.pull(&mut out);
    assert!(
        out.iter().all(|s| *s == 0.0),
        "re-priming: one packet is not target + packet"
    );
    assert_eq!(
        follower.underrun_frames(),
        600,
        "the re-prime's zeros count as underrun"
    );
    follower.push(packet(&ramp(2_480, 480), 1), &mut scratch);
    follower.pull(&mut out);
    assert_eq!(out, ramp(2_000, 480), "primed again, nothing skipped");
    assert_eq!(follower.underrun_frames(), 600);
}

#[test]
fn a_fast_follower_slips_back_to_the_target_once_a_window_stays_above_high_water() {
    // Target 480, high water 960, a window of four pulls of 480.
    let follower = FollowerLane::new(480, 960, 1_920, 48_000);
    let mut scratch = vec![0.0f32; 4_096];
    let mut out = vec![0.0f32; 480];
    // Primed at 960; then 960 extra frames at once and one packet per pull:
    // the queue stays at 1 440, above high water.
    follower.push(packet(&ramp(0, 960), 1), &mut scratch);
    assert_eq!(follower.pull(&mut out), 0);
    follower.push(packet(&ramp(960, 1_440), 1), &mut scratch);
    let mut lost = 0;
    let mut pushed = 2_400;
    for _ in 0..6 {
        lost += follower.pull(&mut out);
        follower.push(packet(&ramp(pushed, 480), 1), &mut scratch);
        pushed += 480;
    }
    assert_eq!(lost, 0, "no window has stayed above high water yet");
    assert_eq!(follower.queued(), 1_920);
    // Its fourth pull closes the window: the lowest queue, 1 440, slips to
    // the target.
    lost += follower.pull(&mut out);
    assert_eq!(
        lost, 960,
        "the lowest queue of the window slips to the target"
    );
    assert_eq!(follower.slipped_frames(), 960);
    assert_eq!(follower.queued(), 480);
    follower.pull(&mut out);
    assert_eq!(
        out,
        ramp(pushed - 480, 480),
        "the newest frames survive the slip"
    );
    assert_eq!(follower.trimmed_frames(), 0);
}

/// The slip takes the window's lowest queue, not the last pull's.
#[test]
fn a_burst_before_the_window_closes_survives_the_slip() {
    let follower = FollowerLane::new(480, 960, 1_920, 48_000);
    let mut scratch = vec![0.0f32; 4_096];
    let mut out = vec![0.0f32; 480];
    follower.push(packet(&ramp(0, 960), 1), &mut scratch);
    assert_eq!(follower.pull(&mut out), 0);
    follower.push(packet(&ramp(960, 1_440), 1), &mut scratch);
    let mut pushed = 2_400;
    for _ in 0..6 {
        follower.pull(&mut out);
        follower.push(packet(&ramp(pushed, 480), 1), &mut scratch);
        pushed += 480;
    }
    follower.push(packet(&ramp(pushed, 960), 1), &mut scratch);
    assert_eq!(
        follower.pull(&mut out),
        960,
        "the window's lowest queue, 1 440, slips to 480"
    );
    assert_eq!(follower.queued(), 1_440, "the burst stays queued");
}

/// A queue more than the master's buffer above high water slips at once.
#[test]
fn a_queue_beyond_the_masters_buffer_slips_at_once() {
    // Ceiling: high water 960 plus a 960-frame master buffer.
    let follower = FollowerLane::new(480, 960, 24_000, 48_000).with_max_lateness(960);
    let mut scratch = vec![0.0f32; 4_096];
    let mut out = vec![0.0f32; 480];
    follower.push(packet(&ramp(0, 960), 1), &mut scratch);
    assert_eq!(follower.pull(&mut out), 0);
    follower.push(packet(&ramp(960, 1_920), 1), &mut scratch);
    assert_eq!(
        follower.pull(&mut out),
        0,
        "at the ceiling: the window judges"
    );
    assert_eq!(follower.queued(), 1_920);
    follower.push(packet(&ramp(2_880, 960), 1), &mut scratch);
    assert_eq!(
        follower.pull(&mut out),
        1_920,
        "above the ceiling: back to the target at once"
    );
    assert_eq!(follower.queued(), 480);
    follower.pull(&mut out);
    assert_eq!(out, ramp(3_360, 480), "the newest frames survive the slip");
}

#[test]
fn the_stream_policy_keeps_two_periods_and_slips_one_period_above() {
    let follower = FollowerLane::for_streams(480, 4_800);
    assert_eq!(follower.target(), 960, "two periods");
    let mut scratch = vec![0.0f32; 4_096];
    let mut out = vec![0.0f32; 480];
    let pulls_per_window = FollowerLane::SLIP_WINDOW / 480;
    follower.push(packet(&ramp(0, 1_440), 1), &mut scratch);
    assert_eq!(follower.pull(&mut out), 0, "primed at the target");
    // `extra` frames at once, then a period per period for two windows.
    let mut pushed = 1_440;
    let mut sustain = |extra: usize| {
        follower.push(packet(&ramp(pushed, extra), 1), &mut scratch);
        pushed += extra;
        let mut lost = 0;
        for _ in 0..2 * pulls_per_window {
            follower.push(packet(&ramp(pushed, 480), 1), &mut scratch);
            pushed += 480;
            lost += follower.pull(&mut out);
        }
        lost
    };
    assert_eq!(sustain(480), 0, "at the high-water mark: no slip");
    assert_eq!(sustain(480), 960);
    assert_eq!(follower.slipped_frames(), 960);
    assert_eq!(follower.underrun_frames(), 0);
    assert_eq!(follower.queued(), 960, "back at the target");
}

/// The master misses two periods and drains three packets back to back:
/// nothing slips or underruns, and the system lane keeps its lag.
#[test]
fn a_late_master_with_equal_clocks_slips_nothing() {
    let period = 480;
    let follower = FollowerLane::for_streams(period, 4_800);
    let mut scratch = vec![0.0f32; period];
    let mut out = vec![0.0f32; period];
    let mut pushed = 0;
    let mut delivered: Vec<f32> = Vec::new();
    let mut lost = 0;
    for tick in 0..200 {
        // The fold is the frame's number, counting from 1.
        follower.push(packet(&ramp(pushed + 1, period), 1), &mut scratch);
        pushed += period;
        // The master misses 50 and 51 and drains three at 52; at 120 it runs
        // twice and skips 121.
        let pulls = match tick {
            50 | 51 | 121 => 0,
            52 => 3,
            120 => 2,
            _ => 1,
        };
        for _ in 0..pulls {
            lost += follower.pull(&mut out);
            delivered.extend_from_slice(&out);
        }
    }
    assert_eq!(
        follower.slipped_frames(),
        0,
        "a late master is not a fast follower"
    );
    assert_eq!(follower.underrun_frames(), 0);
    assert_eq!(lost, 0);
    // Zeros only while priming, then every frame in order.
    let first = delivered.iter().position(|s| *s != 0.0).unwrap();
    assert_eq!(first, 2 * period, "two pulls of zeros while priming");
    let lane = &delivered[first..];
    assert_eq!(lane, ramp(1, lane.len()).as_slice());
}

/// A follower clock 0.5 % fast for ten seconds: the queue slips back each
/// window and never runs away or underruns.
#[test]
fn a_drifting_follower_slips_repeatedly_and_stays_bounded() {
    let period = 480;
    let follower = FollowerLane::for_streams(period, 4_800);
    let mut scratch = vec![0.0f32; 1_024];
    let mut out = vec![0.0f32; period];
    let mut pushed = 0usize;
    let mut lost = 0;
    let mut highest = 0;
    for tick in 0..1_000usize {
        // 482 or 483 frames per period: 2.4 extra on average.
        let frames = period + (tick + 1) * 12 / 5 - tick * 12 / 5;
        follower.push(packet(&ramp(pushed + 1, frames), 1), &mut scratch);
        pushed += frames;
        lost += follower.pull(&mut out);
        highest = highest.max(follower.queued());
    }
    assert!(follower.slipped_frames() > 0, "the drift slipped");
    assert_eq!(lost, follower.slipped_frames(), "every slip is reported");
    assert_eq!(follower.underrun_frames(), 0);
    assert!(
        highest < 4 * period,
        "the queue stays near the high-water mark: highest {highest}"
    );
    let extra = pushed - 1_000 * period;
    assert!(
        follower.slipped_frames() + 3 * period >= extra,
        "the slips absorb the drift: {} slipped of {extra} extra",
        follower.slipped_frames()
    );
}

#[test]
fn audio_queued_before_the_first_pull_is_trimmed_uncounted() {
    let follower = FollowerLane::new(480, 960, 4_800, 48_000);
    let mut scratch = vec![0.0f32; 4_096];
    let mut out = vec![0.0f32; 480];
    // The follower started 100 ms before the master.
    follower.push(packet(&ramp(0, 4_800), 1), &mut scratch);
    assert_eq!(follower.pull(&mut out), 0, "a trim is not a loss");
    assert_eq!(out, ramp(3_840, 480), "the newest frames beyond the target");
    assert_eq!(follower.trimmed_frames(), 3_840);
    assert_eq!(follower.queued(), 480);
    assert_eq!(follower.slipped_frames(), 0);

    // A later re-prime trims nothing: that excess is recorded audio.
    follower.pull(&mut out);
    follower.pull(&mut out);
    assert!(follower.underrun_frames() > 0);
    follower.push(packet(&ramp(10_000, 1_920), 1), &mut scratch);
    follower.pull(&mut out);
    assert_eq!(out, ramp(10_000, 480));
    assert_eq!(follower.trimmed_frames(), 3_840);
}

/// A follower that starts after the master's first pull is never trimmed.
#[test]
fn audio_queued_after_the_masters_first_pull_is_not_trimmed() {
    let follower = FollowerLane::new(480, 960, 4_800, 48_000);
    let mut scratch = vec![0.0f32; 4_096];
    let mut out = vec![1.0f32; 480];
    assert_eq!(follower.pull(&mut out), 0);
    assert!(out.iter().all(|s| *s == 0.0), "nothing queued yet: zeros");
    follower.push(packet(&ramp(0, 2_400), 1), &mut scratch);
    follower.pull(&mut out);
    assert_eq!(out, ramp(0, 480), "the oldest frame first");
    assert_eq!(follower.trimmed_frames(), 0);
    assert_eq!(follower.queued(), 1_920);
}

#[test]
fn the_follower_folds_stereo_and_stages_silent_packets_as_zeros() {
    let follower = FollowerLane::new(0, 4_800, 4_800, 48_000);
    // A fold buffer smaller than the packet folds it in pieces.
    let mut scratch = vec![0.0f32; 100];
    follower.push(packet(&stereo_ramp(0, 480), 2), &mut scratch);
    follower.push(silent(480, 2), &mut scratch);
    // A packet shorter than it claims becomes zeros, not a short write.
    let short = stereo_ramp(0, 10);
    follower.push(
        SliceView {
            frames: 480,
            channels: 2,
            samples: Some(&short),
        },
        &mut scratch,
    );
    let mut out = vec![0.0f32; 1_440];
    follower.pull(&mut out);
    assert_eq!(&out[..480], ramp(1, 480).as_slice(), "folded");
    assert!(out[480..].iter().all(|s| *s == 0.0));
}

#[test]
fn a_full_staging_ring_refuses_whole_packets_and_the_master_counts_them() {
    let follower = Arc::new(FollowerLane::new(0, 1_024, 4_800, 1_024));
    let lanes = [AudioLane::Mic, AudioLane::System];
    let plan = SplitStreamPlan::new(&lanes).unwrap();
    let sink = LaneFrameSink::new(&lanes);
    let mut router = PacketRouter::new(plan.layout.sources, Some(Arc::clone(&follower)), 480);
    // The master's first pull, before the follower delivers.
    router.route(packet(&ramp(0, 480), 1), &sink);

    let mut scratch = vec![0.0f32; 512];
    follower.push(packet(&ramp(0, 1_000), 1), &mut scratch);
    follower.push(packet(&ramp(1_000, 100), 1), &mut scratch);
    assert_eq!(follower.queued(), 1_000, "the second packet did not fit");
    router.route(packet(&ramp(480, 480), 1), &sink);
    let dropped = sink.dropped_samples();
    assert_eq!(dropped.get(&AudioLane::System), Some(&100));
    assert_eq!(dropped.get(&AudioLane::Mic), None);

    router.route(packet(&ramp(960, 480), 1), &sink);
    assert_eq!(
        sink.dropped_samples().get(&AudioLane::System),
        Some(&100),
        "an overflow is reported once"
    );
}

/// A follower that fills its staging before the master's first pull drops
/// that backlog uncounted; an overflow after the first pull is counted.
#[test]
fn an_overflow_before_the_first_pull_drops_the_stale_queue_uncounted() {
    let follower = FollowerLane::new(0, 1_024, 4_800, 1_024);
    let mut scratch = vec![0.0f32; 512];
    let mut out = vec![1.0f32; 480];
    follower.push(packet(&ramp(0, 1_000), 1), &mut scratch);
    follower.push(packet(&ramp(1_000, 100), 1), &mut scratch);
    assert_eq!(follower.queued(), 1_000, "the second packet did not fit");
    assert_eq!(
        follower.pull(&mut out),
        0,
        "not counted before the first pull"
    );
    assert!(out.iter().all(|s| *s == 0.0), "nothing stale is delivered");
    assert_eq!(follower.queued(), 0);
    assert_eq!(
        follower.trimmed_frames(),
        1_100,
        "the queue and the refused"
    );
    assert_eq!(follower.underrun_frames(), 0, "never primed: no underrun");

    follower.push(packet(&ramp(1_100, 1_000), 1), &mut scratch);
    follower.push(packet(&ramp(2_100, 100), 1), &mut scratch);
    assert_eq!(follower.pull(&mut out), 100, "refused after the first pull");
    assert_eq!(out, ramp(1_100, 480), "primed on the audio after the gap");
    assert_eq!(follower.pull(&mut out), 0, "an overflow is reported once");
    assert_eq!(follower.trimmed_frames(), 1_100);
}

#[test]
fn the_master_routes_its_packet_and_the_followers_frames_as_one_callback() {
    let lanes = [AudioLane::Mic, AudioLane::System];
    let plan = SplitStreamPlan::new(&lanes).unwrap();
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let follower = Arc::new(FollowerLane::new(480, 2_400, 4_800, 48_000));
    let mut master = master_body(&plan, &follower, 480, &sink);
    let mut staging = StreamBody::follower(Arc::clone(&follower), 480);

    // The follower is one packet ahead: priming needs 480 + 480.
    staging.handle(packet(&stereo_ramp(0, 480), 2));
    staging.handle(packet(&stereo_ramp(480, 480), 2));
    master.handle(packet(&ramp(10_000, 480), 1));
    staging.handle(packet(&stereo_ramp(960, 480), 2));
    master.handle(packet(&ramp(10_480, 480), 1));

    assert_eq!(sink.available_to_read(), 960);
    assert_eq!(drain(&sink, 0), ramp(10_000, 960));
    assert_eq!(
        drain(&sink, 1),
        ramp(1, 960),
        "folded, in order, primed at once"
    );
    assert!(sink.dropped_samples().is_empty());
}

#[test]
fn a_silent_master_packet_writes_zeros_and_a_large_one_is_split() {
    let lanes = [AudioLane::Mixed];
    let plan = SplitStreamPlan::new(&lanes).unwrap();
    let sink = LaneFrameSink::new(&lanes);
    let mut router = PacketRouter::new(plan.layout.sources, None, 256);
    router.route(silent(100, 1), &sink);
    router.route(packet(&ramp(1, 1_000), 1), &sink);
    let mixed = drain(&sink, 0);
    assert_eq!(mixed.len(), 1_100);
    assert!(mixed[..100].iter().all(|s| *s == 0.0));
    assert_eq!(&mixed[100..], ramp(1, 1_000).as_slice());
    let mut callbacks = 0;
    while sink.wake().try_take() {
        callbacks += 1;
    }
    assert_eq!(
        callbacks, 5,
        "one callback for the silent packet, four for 1 000 frames in 256s"
    );
}

#[test]
fn a_full_sink_drops_the_callback_on_every_lane() {
    let lanes = [AudioLane::Mic, AudioLane::System];
    let plan = SplitStreamPlan::new(&lanes).unwrap();
    let sink = LaneFrameSink::with_handler(&lanes, 1_000.0, 1.0, Box::new(|_| {}));
    let follower = Arc::new(FollowerLane::new(0, 4_800, 4_800, 48_000));
    let mut scratch = vec![0.0f32; 2_048];
    follower.push(packet(&ramp(0, 2_048), 1), &mut scratch);
    let mut router = PacketRouter::new(plan.layout.sources, Some(follower), 2_048);
    // The rings hold 1 024 samples; 2 048 frames do not fit.
    router.route(packet(&ramp(0, 2_048), 1), &sink);
    let dropped = sink.dropped_samples();
    assert_eq!(dropped.get(&AudioLane::Mic), Some(&2_048));
    assert_eq!(dropped.get(&AudioLane::System), Some(&2_048));
    assert_eq!(sink.available_to_read(), 0);
}

#[test]
fn a_system_only_master_folds_its_stereo_packet() {
    let lanes = [AudioLane::System];
    let plan = SplitStreamPlan::new(&lanes).unwrap();
    let sink = LaneFrameSink::new(&lanes);
    let mut router = PacketRouter::new(plan.layout.sources, None, 480);
    router.route(packet(&stereo_ramp(0, 480), 2), &sink);
    assert_eq!(drain(&sink, 0), ramp(1, 480), "folded");
}

#[test]
fn a_slice_shorter_than_it_claims_is_delivered_as_silence_of_its_claimed_length() {
    let lanes = [AudioLane::Mic, AudioLane::System];
    let plan = SplitStreamPlan::new(&lanes).unwrap();
    let sink = LaneFrameSink::new(&lanes);
    let full = ramp(1, 480);
    let short = vec![7.0f32; 10];
    let slice = |samples| SliceView {
        channels: 1,
        frames: 480,
        samples: Some(samples),
    };

    // A short follower slice: the system lane is zeros, the mic intact.
    deliver_slices(&[slice(&full), slice(&short)], &plan.layout.sources, &sink);
    assert_eq!(drain(&sink, 0), full);
    assert_eq!(drain(&sink, 1), vec![0.0; 480]);

    // A short master slice: the callback keeps its 480 frames.
    deliver_slices(&[slice(&short), slice(&full)], &plan.layout.sources, &sink);
    assert_eq!(drain(&sink, 0), vec![0.0; 480]);
    assert_eq!(drain(&sink, 1), full);
}

/// The follower and the master on two threads at once: lanes stay whole,
/// equal and ordered, and every follower frame is accounted for. The `tsan`
/// job runs it under ThreadSanitizer.
#[test]
fn a_follower_and_a_master_on_two_threads_account_for_every_frame() {
    const PERIOD: usize = 480;
    const PACKETS: usize = 150;
    let lanes = [AudioLane::Mic, AudioLane::System];
    let plan = SplitStreamPlan::new(&lanes).unwrap();
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let follower = Arc::new(FollowerLane::for_streams(PERIOD, 4_800));
    let mut master = master_body(&plan, &follower, PERIOD, &sink);
    let mut staging = StreamBody::follower(Arc::clone(&follower), PERIOD);

    let follower_thread = std::thread::spawn(move || {
        for index in 0..PACKETS {
            // The fold is the frame's number, counting from 1.
            staging.handle(packet(&stereo_ramp(index * PERIOD, PERIOD), 2));
            if index % 3 == 0 {
                std::thread::yield_now();
            }
        }
        PACKETS * PERIOD
    });
    let mic = ramp(1, PACKETS * PERIOD);
    for (index, period) in mic.as_chunks::<PERIOD>().0.iter().enumerate() {
        master.handle(packet(period, 1));
        if index % 2 == 0 {
            std::thread::yield_now();
        }
    }
    let pushed = follower_thread.join().unwrap();
    let late_lost = follower.pull(&mut []);

    let mic_lane = drain(&sink, 0);
    let system_lane = drain(&sink, 1);
    assert_eq!(mic_lane, mic, "the mic lane arrives whole");
    assert_eq!(system_lane.len(), mic_lane.len(), "the lanes stay aligned");
    let mut previous = 0.0f32;
    let mut delivered = 0;
    for (index, sample) in system_lane.iter().copied().enumerate() {
        if sample == 0.0 {
            continue;
        }
        assert!(
            sample > previous,
            "order broken at {index}: {sample} after {previous}"
        );
        assert_eq!(sample.fract(), 0.0, "torn sample at {index}: {sample}");
        previous = sample;
        delivered += 1;
    }
    let system_lost = sink
        .dropped_samples()
        .get(&AudioLane::System)
        .copied()
        .unwrap_or(0)
        + late_lost;
    assert_eq!(
        pushed,
        delivered + system_lost + follower.queued() + follower.trimmed_frames(),
        "pushed = delivered + lost + queued + trimmed before the first pull"
    );
    assert_eq!(sink.dropped_samples().get(&AudioLane::Mic), None);
}

// Missed audio

/// 10 ms periods at 48 kHz.
const PERIOD: usize = 480;
/// The engine buffer: 100 ms, ten periods.
const ENGINE_PERIODS: usize = 10;

/// What [`run_outage`] saw in the sink.
#[derive(Debug)]
struct Outage {
    /// The microphone's frame less the system's where the sink holds both,
    /// before the outage and at every change after, with the period.
    lags: Vec<(usize, i64)>,
    mic_filled: usize,
    system_filled: usize,
    mic_unfilled: usize,
    system_unfilled: usize,
    system_dropped: usize,
    slipped: usize,
    /// The seconds the recorder's note would say are missing.
    note: f64,
    /// The seconds truly missing.
    truth: f64,
}

const OUTAGE_START: usize = 50;

/// Period `t` of a stream's thread out for `out` periods from
/// [`OUTAGE_START`]: nothing while out, then the ten kept packets, then
/// `lost` and its packet.
fn outage_period<'a>(
    body: &mut StreamBody,
    t: usize,
    out: usize,
    packet: impl Fn(usize) -> SliceView<'a>,
    lost: Missed,
) {
    let back = OUTAGE_START + out;
    if out == 0 || t < OUTAGE_START || t > back {
        body.handle(packet(t));
    } else if t == back {
        for kept in OUTAGE_START..OUTAGE_START + ENGINE_PERIODS.min(out) {
            body.handle(packet(kept));
        }
        body.write_missed(&packet(t), lost);
        body.handle(packet(t));
    }
}

/// [`run_outage_in_order`] with the follower's thread first.
fn run_outage(follower_out: usize, master_out: usize) -> Outage {
    run_outage_in_order(follower_out, master_out, false)
}

/// The two stream bodies as the WASAPI backend builds them over 400
/// periods (source frame `f` is `f + 1`), the follower out for
/// `follower_out` periods and the master for `master_out`; the master's
/// thread runs first in each period when `master_first`.
#[allow(clippy::too_many_lines)]
fn run_outage_in_order(follower_out: usize, master_out: usize, master_first: bool) -> Outage {
    let lanes = [AudioLane::Mic, AudioLane::System];
    let plan = SplitStreamPlan::new(&lanes).unwrap();
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let follower = Arc::new(FollowerLane::for_streams_filling(PERIOD, 4_800));
    let mut master_body = StreamBody::Master {
        router: PacketRouter::new(
            plan.layout.sources.clone(),
            Some(Arc::clone(&follower)),
            4_800,
        ),
        sink: Arc::clone(&sink),
    };
    let mut staging = StreamBody::follower(Arc::clone(&follower), 4_800);
    let periods = 400;
    let mic: Vec<f32> = (0..periods * PERIOD).map(|i| (i + 1) as f32).collect();
    let tap: Vec<f32> = (0..periods * PERIOD)
        .flat_map(|i| [(i + 1) as f32, (i + 1) as f32])
        .collect();
    let tap_packet = |t: usize| SliceView {
        frames: PERIOD,
        channels: 2,
        samples: Some(&tap[2 * t * PERIOD..2 * (t + 1) * PERIOD]),
    };
    let mic_packet = |t: usize| SliceView {
        frames: PERIOD,
        channels: 1,
        samples: Some(&mic[t * PERIOD..(t + 1) * PERIOD]),
    };
    let limit = MISSED_FILL_LIMIT.as_millis() as usize * PERIOD / 10;
    let lost = |out: usize| {
        let frames = out.saturating_sub(ENGINE_PERIODS) * PERIOD;
        if frames <= limit {
            Missed {
                fill: frames,
                unfilled: 0,
            }
        } else {
            Missed {
                fill: 0,
                unfilled: frames,
            }
        }
    };
    let start = OUTAGE_START;
    let (mut mic_out, mut system_out) = (Vec::new(), Vec::new());
    for t in 0..periods {
        let run_follower = |body: &mut StreamBody| {
            outage_period(body, t, follower_out, tap_packet, lost(follower_out));
        };
        let run_master = |body: &mut StreamBody| {
            outage_period(body, t, master_out, mic_packet, lost(master_out));
        };
        if master_first {
            run_master(&mut master_body);
            run_follower(&mut staging);
        } else {
            run_follower(&mut staging);
            run_master(&mut master_body);
        }
        let queued = sink.available_to_read();
        let (mut a, mut b) = (vec![0.0f32; queued], vec![0.0f32; queued]);
        assert!(sink.ring(0).read(&mut a) && sink.ring(1).read(&mut b));
        mic_out.extend(a);
        system_out.extend(b);
    }
    let lag = |p: usize| {
        let (m, s) = (mic_out[p], system_out[p]);
        (m != 0.0 && s != 0.0).then(|| m as i64 - s as i64)
    };
    let mut lags = vec![(40, lag(40 * PERIOD).unwrap())];
    for position in start * PERIOD..mic_out.len() {
        if let Some(now) = lag(position)
            && now != lags.last().unwrap().1
        {
            lags.push((position / PERIOD, now));
        }
    }
    let of = |counts: &BTreeMap<AudioLane, usize>, lane| counts.get(&lane).copied().unwrap_or(0);
    let (missed, unfilled) = (sink.filled_samples(), sink.unfilled_samples());
    let (mic_filled, system_filled) = (of(&missed, AudioLane::Mic), of(&missed, AudioLane::System));
    let (mic_unfilled, system_unfilled) = (
        of(&unfilled, AudioLane::Mic),
        of(&unfilled, AudioLane::System),
    );
    let seconds = |frames: usize| frames as f64 / SAMPLE_RATE;
    let wall = seconds(periods * PERIOD);
    let short = wall - seconds(mic_out.len());
    Outage {
        lags,
        mic_filled,
        system_filled,
        mic_unfilled,
        system_unfilled,
        system_dropped: of(&sink.dropped_samples(), AudioLane::System),
        slipped: follower.slipped_frames(),
        note: seconds(mic_filled.max(system_filled))
            + seconds(mic_unfilled.max(system_unfilled)).max(short - wall * 0.001),
        truth: short + seconds(mic_filled),
    }
}

/// That `outage`'s system lane counts the `lost` frames up to an engine
/// buffer more, and the note what is truly missing up to that buffer more.
fn assert_counted_once(outage: &Outage, lost: usize) {
    let system = outage.system_filled + outage.system_unfilled;
    let buffer = ENGINE_PERIODS * PERIOD;
    assert!(
        (lost..=lost + buffer).contains(&system),
        "the system lane counts {system} for {lost}: {outage:?}"
    );
    let truth = outage.truth.max(lost as f64 / SAMPLE_RATE);
    assert!(
        outage.note >= truth - 1e-9 && outage.note <= truth + buffer as f64 / SAMPLE_RATE + 1e-9,
        "the note says {} s for {truth} s: {outage:?}",
        outage.note
    );
}

/// Both threads out for 200 and 300 ms: nothing counts twice, and the lag
/// ends where it started in either order.
#[test]
fn a_stall_of_both_threads_keeps_the_lanes_aligned() {
    for out in [20, 30] {
        let lost = (out - ENGINE_PERIODS) * PERIOD;
        let outage = run_outage(out, out);
        assert_eq!(outage.lags, [(40, 960)], "{out} periods: {outage:?}");
        assert_eq!(
            (outage.slipped, outage.system_dropped),
            (0, 0),
            "{outage:?}"
        );
        assert_eq!(outage.mic_filled, lost);
        assert_counted_once(&outage, lost);
        let outage = run_outage_in_order(out, out, true);
        let (base, (period, lag)) = (outage.lags[0], *outage.lags.last().unwrap());
        assert_eq!(base, (40, 1_440), "master first, {out} periods: {outage:?}");
        assert_eq!(lag, base.1, "{outage:?}");
        assert!(period <= OUTAGE_START + out + 100, "{outage:?}");
        assert_eq!((outage.slipped, outage.system_dropped), (3_840, 3_840));
        assert_eq!(outage.mic_filled, lost);
        assert_counted_once(&outage, lost);
    }
}

/// Both threads out past the fill limit, in either order: each lane counts
/// its own jump, the note counts the time once, and the lag returns.
#[test]
fn an_outage_of_both_threads_past_the_fill_limit_counts_once() {
    for out in [150, 250] {
        let lost = (out - ENGINE_PERIODS) * PERIOD;
        for master_first in [false, true] {
            let outage = run_outage_in_order(out, out, master_first);
            let context = format!("{out} periods, master first {master_first}: {outage:?}");
            assert!(
                (outage.truth - lost as f64 / SAMPLE_RATE).abs() < 1e-9,
                "{context}"
            );
            assert_eq!(
                (outage.mic_filled, outage.mic_unfilled),
                (0, lost),
                "{context}"
            );
            assert_counted_once(&outage, lost);
            assert_eq!(outage.lags.last().unwrap().1, outage.lags[0].1, "{context}");
        }
        let outage = run_outage(out, out);
        let resumed = OUTAGE_START + ENGINE_PERIODS;
        assert_eq!(outage.lags.len(), 3, "{outage:?}");
        assert_eq!(outage.lags[1].0, resumed, "{outage:?}");
        assert_eq!(outage.lags[2], (resumed + 2, 960), "{outage:?}");
        // One thread alone: the note says its loss once.
        for (follower_out, master_out) in [(out, 0), (0, out)] {
            let outage = run_outage(follower_out, master_out);
            let context = format!("{follower_out} and {master_out} periods: {outage:?}");
            if follower_out > 0 {
                assert_counted_once(&outage, lost);
            } else {
                assert!(
                    (outage.note - lost as f64 / SAMPLE_RATE).abs() < 0.01,
                    "{context}"
                );
            }
            assert_eq!(outage.lags.last().unwrap().1, 960, "{context}");
        }
    }
}

/// The master's thread alone out for 200 and 300 ms: nothing slips and the
/// lanes stay aligned.
#[test]
fn a_stall_of_the_master_alone_keeps_the_lanes_aligned() {
    for out in [20, 30] {
        let outage = run_outage(0, out);
        assert_eq!(outage.lags, [(40, 960)], "{out} periods: {outage:?}");
        assert_eq!(outage.slipped, 0, "{outage:?}");
        assert_eq!(outage.system_dropped, 0, "{outage:?}");
        assert_eq!(outage.mic_filled, (out - ENGINE_PERIODS) * PERIOD);
        assert_eq!(outage.system_filled, 0);
    }
}

/// A master jump too long to fill drops the system audio queued for that
/// time, so both lanes lose the same span.
#[test]
fn an_unfilled_jump_of_the_master_drops_the_follower_audio_of_that_time() {
    let lanes = [AudioLane::Mic, AudioLane::System];
    let plan = SplitStreamPlan::new(&lanes).unwrap();
    let sink = LaneFrameSink::new(&lanes);
    let follower = Arc::new(FollowerLane::for_streams_filling(PERIOD, 4_800));
    let mut router = PacketRouter::new(
        plan.layout.sources.clone(),
        Some(Arc::clone(&follower)),
        4_800,
    );
    let tap = vec![0.5f32; 2 * PERIOD];
    let mic = vec![0.25f32; PERIOD];
    let stereo = SliceView {
        frames: PERIOD,
        channels: 2,
        samples: Some(&tap),
    };
    let mono = SliceView {
        frames: PERIOD,
        channels: 1,
        samples: Some(&mic),
    };
    let mut scratch = vec![0.0f32; PERIOD];
    for _ in 0..3 {
        follower.push(stereo, &mut scratch);
    }
    router.route(mono, &sink);
    for _ in 0..100 {
        follower.push(stereo, &mut scratch);
    }
    assert_eq!(follower.queued(), 960 + 100 * PERIOD);
    router.write_missed(
        &mono,
        Missed {
            fill: 0,
            unfilled: 100 * PERIOD,
        },
        &sink,
    );
    assert_eq!(
        follower.queued(),
        960 + PERIOD,
        "back to the target and the next packet's pull"
    );
    assert_eq!(
        sink.unfilled_samples(),
        BTreeMap::from([
            (AudioLane::Mic, 100 * PERIOD),
            (AudioLane::System, 99 * PERIOD)
        ])
    );
    assert_eq!(sink.available_to_read(), PERIOD, "nothing written for it");
    assert_eq!(follower.slipped_frames(), 0, "not a slip");
}

/// The follower's thread alone out for 150 to 300 ms: it counts only what
/// it lost, and the slip stays the same however long the outage.
#[test]
fn a_stall_of_the_follower_alone_stages_no_zeros() {
    for out in [15, 20, 30] {
        let outage = run_outage(out, 0);
        assert_eq!(outage.mic_filled, 0);
        assert_counted_once(&outage, (out - ENGINE_PERIODS) * PERIOD);
        assert_eq!(
            (outage.slipped, outage.system_dropped),
            (3_840, 3_840),
            "{out} periods: only the kept packets' lateness slips: {outage:?}"
        );
        let (period, lag) = *outage.lags.last().unwrap();
        assert_eq!(lag, 960, "{outage:?}");
        assert!(period <= 50 + out + 100, "{outage:?}");
    }
}

// --- the stop, and padding that is no loss --------------------------------

/// A call's two bodies over one sink, 10 ms packets each.
struct Call {
    sink: Arc<LaneFrameSink>,
    follower: Arc<FollowerLane>,
    master: StreamBody,
    staging: StreamBody,
    mic: Vec<f32>,
    tap: Vec<f32>,
}

impl Call {
    fn new() -> Self {
        let plan = SplitStreamPlan::new(&[AudioLane::Mic, AudioLane::System]).unwrap();
        let sink = Arc::new(LaneFrameSink::new(&[AudioLane::Mic, AudioLane::System]));
        let follower = Arc::new(FollowerLane::for_streams_filling(PERIOD, 4_800));
        let master = master_body(&plan, &follower, 4_800, &sink);
        let staging = StreamBody::follower(Arc::clone(&follower), 4_800);
        Self {
            sink,
            follower,
            master,
            staging,
            mic: vec![0.5; PERIOD],
            tap: vec![0.25; 2 * PERIOD],
        }
    }

    fn master_packet(&mut self) {
        self.master.handle(packet(&self.mic, 1));
        for lane in 0..2 {
            self.sink.ring(lane).drain_all();
        }
    }

    fn follower_packet(&mut self) {
        self.staging.handle(packet(&self.tap, 2));
    }

    /// The follower's flagged packet, after what its clocks read.
    fn follower_flagged(&mut self, missed: Missed) {
        self.staging.write_missed(&packet(&self.tap, 2), missed);
        self.follower_packet();
    }

    fn tick(&mut self) {
        self.follower_packet();
        self.master_packet();
    }

    /// The backend's stop, with the streams' end readings (`None`: not
    /// read).
    fn stop(&self, master_end: Missed, follower_end: Option<Missed>) {
        count_at_stop(
            &self.sink,
            Some((&self.follower, 1)),
            master_end,
            follower_end,
        );
    }

    /// The system lane's (filled, unfilled).
    fn system(&self) -> (usize, usize) {
        let get = |counts: BTreeMap<AudioLane, usize>| {
            counts.get(&AudioLane::System).copied().unwrap_or(0)
        };
        (
            get(self.sink.filled_samples()),
            get(self.sink.unfilled_samples()),
        )
    }
}

impl Call {
    /// Runs `script`, words split by spaces: `<n>t` ticks, `<n>m` master
    /// packets, `<n>f` follower packets; `F` the follower's flagged packet
    /// with nothing its clocks read, `F<n>` with `n` filled; `S` the stop
    /// with no follower reading, `C` with a continuous follower's empty
    /// reading, `U<n>` with `n` unfilled.
    fn run(&mut self, script: &str) -> &mut Self {
        for word in script.split_whitespace() {
            let (letter, number) = if word.as_bytes()[0].is_ascii_digit() {
                let (count, op) = word.split_at(word.len() - 1);
                (op, count.parse::<usize>().unwrap())
            } else {
                let (op, value) = word.split_at(1);
                (op, value.parse::<usize>().unwrap_or(0))
            };
            match letter {
                "t" => (0..number).for_each(|_| self.tick()),
                "m" => (0..number).for_each(|_| self.master_packet()),
                "f" => (0..number).for_each(|_| self.follower_packet()),
                "F" => self.follower_flagged(filled(number)),
                "S" => self.stop(Missed::default(), None),
                "C" => self.stop(Missed::default(), Some(Missed::default())),
                "U" => self.stop(Missed::default(), Some(unfilled(number))),
                _ => panic!("{word}"),
            }
        }
        self
    }
}

/// What the system lane counts (filled and unfilled) after each script of
/// the follower's outages, idles, flags and the stop. Padding the master
/// wrote while the system stream was out counts once, at its flag or at
/// the stop; padding of an idle no flag follows within the hold, and a
/// trim or re-prime that is only delay, does not.
#[test]
fn the_follower_padding_counts_once_for_every_outage_and_never_for_an_idle() {
    let p = PERIOD;
    for (name, script, counted) in [
        // Out for the last 0.5 s before the stop, however the drains fall.
        (
            "out at the stop",
            "200t 50m 10f F19200 S S",
            19_200..=24_000,
        ),
        (
            "out at the stop, 1 after",
            "200t 50m 10f F19200 1m S S",
            19_200..=24_000,
        ),
        (
            "out at the stop, 2 after",
            "200t 50m 10f F19200 2m S S",
            19_200..=24_000,
        ),
        // A loss into the stop with no flag: the stop's counter reading.
        ("no packet flags", "200t 50m 10f U19200", 19_200..=24_000),
        // After an unflagged 10 s idle a flag counts only its own loss.
        ("older than the hold", "100t 1000m 100t F 10t", 0..=0),
        ("older, its own fill", "100t 1000m 100t F480 10t", 480..=480),
        // Kept packets unflagged and the flag a period later.
        (
            "flag after, follower first",
            "200t 50m 10f 1m F 1m 20t",
            40 * p - 960..=40 * p + 4_800,
        ),
        (
            "flag after, master first",
            "200t 50m 1m 10f F 1m 20t",
            40 * p - 960..=40 * p + 4_800,
        ),
        ("flag long after an idle", "100t 50m 60f F 10t S", 0..=0),
        // A flag before a 6 s idle takes the outage's padding only.
        (
            "pending flag, idle after",
            "100t 27m F 601m 20t",
            27 * p - 960..=39 * p,
        ),
        // An underrun after the resume keeps the outage's padding.
        (
            "underrun, follower first",
            "200t 63m 10f 10m 1m F 20t",
            61 * p - 960..=usize::MAX,
        ),
        (
            "underrun, master first",
            "200t 63m 10m 10f 1m F 20t",
            61 * p - 960..=usize::MAX,
        ),
        (
            "out before the first prime",
            "2t 96m 10f F 20t",
            86 * p..=usize::MAX,
        ),
        // The stop takes the outage's padding, primed again or still out.
        ("stop, primed again", "200t 50m 10f 3m F S", 48 * p..=50 * p),
        ("stop, still out", "200t 50m 2f F S", 48 * p..=50 * p),
        (
            "two re-primes in the hold",
            "200t 50m 3f 5m 4f 4m F 1m",
            48 * p..=52 * p,
        ),
        // Two idles: both count within the hold apart, only the second past.
        (
            "two idles within the hold",
            "200t 100m 5t 50m F 20t",
            0..=152 * p,
        ),
        (
            "two idles past the hold",
            "200t 100m 20t 50m F 20t",
            0..=52 * p,
        ),
        (
            "idle ended while the master was out",
            "200t 600m 30f 30m F S",
            0..=4 * p,
        ),
        (
            "a spurious second flag",
            "200t 100m 8f F 1m 3t F 20t S",
            100 * p - 1_920..=102 * p,
        ),
        // A process loopback follower out at the stop, unflagged.
        ("continuous, out at the stop", "200t 50m C", 48 * p..=50 * p),
        ("not continuous, out at the stop", "200t 50m S", 0..=0),
    ] {
        let mut call = Call::new();
        let (filled, unfilled) = call.run(script).system();
        assert!(
            counted.contains(&(filled + unfilled)),
            "{name} ({script}): filled {filled}, unfilled {unfilled}"
        );
    }
}

/// A rebuild's stop leaves out the stop's readings but not the follower's
/// flagged padding.
#[test]
fn the_stops_readings_count_on_their_own_lanes_and_not_at_a_rebuild() {
    let mut call = Call::new();
    call.run("200t").stop(unfilled(9_600), None);
    assert_eq!(
        call.sink.unfilled_samples(),
        BTreeMap::from([(AudioLane::Mic, 9_600)])
    );

    let mut call = Call::new();
    call.run("200t").follower_flagged(unfilled(4_800));
    call.sink
        .report_device_change(DeviceChangeReason::OutputDeviceGone);
    call.stop(unfilled(9_600), Some(unfilled(48_000)));
    assert_eq!(
        call.sink.unfilled_samples(),
        BTreeMap::from([(AudioLane::System, 4_800)]),
        "only the flagged packet's count"
    );
}

/// A second flag keeps the first's padding, whenever the stop comes.
#[test]
fn a_second_flag_while_the_master_is_out_keeps_the_first_flags_padding() {
    for rest in ["F S", "F 50t S", "1f 50t S"] {
        let mut call = Call::new();
        let before = call.run("200t").follower.underrun_frames();
        let padded = call.run("100m").follower.underrun_frames() - before;
        let (filled, unfilled) = call.run("F 1m 30f").run(rest).system();
        assert!(
            filled + unfilled + 2 * 960 >= padded,
            "{rest}: {padded} padded, filled {filled}, unfilled {unfilled}"
        );
    }
}

/// Positions frozen at first measure from the pair after the move, so a
/// loss counts what was lost, not a hundred times it.
#[test]
fn positions_that_stood_still_measure_from_after_their_first_move() {
    for engine_rate in [None, Some(48_000), Some(16_000)] {
        for still in [0, 1, 5, 100, 300] {
            let mut clocks = Clocks::new(engine_rate, 480, 0);
            clocks.per_packet = 0;
            clocks.clean(still);
            clocks.per_packet = 480;
            clocks.clean(100);
            let missed = clocks.lose(50);
            let lost = 24_000;
            let most = if engine_rate == Some(16_000) { 3 } else { 1 } * (lost + 480);
            assert!(
                (lost..=most).contains(&missed.unfilled),
                "{engine_rate:?}, still {still}: {missed:?}"
            );
        }
    }
}

/// An unexplained unit whose first pair spans an unflagged idle: the second
/// pair replaces it, so a later 10 ms loss is counted.
#[test]
fn a_first_pair_over_an_idle_is_replaced_by_the_next() {
    // 14 350 position units a second against the stream's 48 kHz.
    let mut positions = DevicePositions::new(None);
    let packet = |positions: &mut DevicePositions, frames: u64, flagged: bool| {
        positions.packet(Packet {
            frames: 480,
            position: 1_000 + frames * 14_350 / 48_000,
            discontinuity: flagged,
            ..Packet::default()
        })
    };
    assert!(packet(&mut positions, 0, false).is_none());
    assert!(
        packet(&mut positions, 960, false).is_none(),
        "after the idle"
    );
    assert!(packet(&mut positions, 1_440, false).is_none());
    let missed = packet(&mut positions, 2_400, true);
    assert!(missed.unfilled >= 480, "{missed:?}");
}

/// A jump is filled only while the QPC's last clean pair matched within
/// half a packet.
#[test]
fn a_qpc_whose_last_pair_did_not_match_fills_nothing() {
    let mut clocks = Clocks::new(Some(48_000), 480, QPC_PACKET);
    clocks.clean(CALIBRATION_PACKETS);
    assert!(clocks.positions.fills());
    clocks.next.qpc += 30_000;
    clocks.clean(1);
    assert!(clocks.positions.fills(), "a jittery pair");
    clocks.clean(1);
    clocks.next.qpc -= 60_000;
    clocks.clean(1);
    assert!(!clocks.positions.fills());
    assert_counted(clocks.lose(50), 24_000, "counted from the positions");
    clocks.clean(1);
    assert!(clocks.positions.fills(), "a matching pair again");
    assert_eq!(clocks.lose(50), filled(24_000));
}

/// The stop's counter reading: past the in-flight packet, unfilled, only on
/// a sane QPC.
#[test]
fn the_stop_reads_the_counter_only_on_a_sane_qpc() {
    let mut clocks = Clocks::new(Some(48_000), 480, QPC_PACKET);
    clocks.clean(10);
    let now = |clocks: &Clocks, packets: u64| clocks.next.qpc + packets * QPC_PACKET;
    assert!(
        clocks.positions.missed_until(now(&clocks, 50)).is_none(),
        "still measuring"
    );
    clocks.clean(CALIBRATION_PACKETS);
    assert!(clocks.positions.missed_until(now(&clocks, 0)).is_none());
    assert!(
        clocks.positions.missed_until(now(&clocks, 1)).is_none(),
        "the packet in flight"
    );
    assert_eq!(
        clocks.positions.missed_until(now(&clocks, 51)),
        unfilled(24_000)
    );
    assert!(
        clocks
            .positions
            .missed_until(now(&clocks, 0) - 10 * QPC_PACKET)
            .is_none(),
        "a counter behind the last packet"
    );
    assert_eq!(
        clocks
            .positions
            .missed_until(now(&clocks, 0) + 7_200 * 10_000_000)
            .unfilled,
        MOST_READ_FRAMES as usize,
        "capped at an hour"
    );
    clocks.deliver(false, true);
    assert!(
        clocks.positions.missed_until(now(&clocks, 51)).is_none(),
        "the last packet's QPC not to be trusted"
    );
    let mut stuck = Clocks::new(None, 0, 0);
    stuck.clean(CALIBRATION_PACKETS);
    assert!(stuck.positions.missed_until(1 << 50).is_none(), "no QPC");
}

/// The stop reads the counter only while the last clean pair matched.
#[test]
fn the_stop_reads_nothing_after_a_last_pair_that_did_not_match() {
    let mut clocks = Clocks::new(Some(48_000), 480, QPC_PACKET);
    clocks.clean(CALIBRATION_PACKETS);
    let now = |clocks: &Clocks, packets: u64| clocks.next.qpc + packets * QPC_PACKET;
    clocks.next.qpc -= 60_000;
    clocks.clean(1);
    assert!(
        clocks.positions.missed_until(now(&clocks, 51)).is_none(),
        "a last pair 60 % short"
    );
    clocks.clean(1);
    assert_eq!(
        clocks.positions.missed_until(now(&clocks, 51)),
        unfilled(24_000)
    );
}

/// The stop reads at a jump's resolution, half the last packet.
#[test]
fn the_stop_reads_a_stall_of_a_packet_and_a_half() {
    let mut clocks = Clocks::new(Some(48_000), 480, QPC_PACKET);
    clocks.clean(CALIBRATION_PACKETS);
    let last = clocks.next.qpc - QPC_PACKET;
    let stop = |packets_tenths: u64| last + (20 + packets_tenths) * QPC_PACKET / 10;
    assert_eq!(clocks.positions.missed_until(stop(15)), unfilled(720));
    assert!(clocks.positions.missed_until(stop(4)).is_none());
}

/// A continuous stream's sizes: a first packet up to 30 ms after the start
/// is on time.
const START_SIZES: StreamSizes = StreamSizes {
    buffer_frames: 4_800,
    period_frames: 480,
    latency_frames: 960,
};

/// A stream whose first packet comes `late_packets` after `start`'s QPC,
/// then two seconds of clean packets.
fn started_late(start: bool, late_packets: u64) -> Clocks {
    let mut clocks = Clocks::new(Some(48_000), 480, QPC_PACKET);
    if start {
        clocks.positions.start(clocks.next.qpc);
    }
    clocks.skip(late_packets).clean(CALIBRATION_PACKETS);
    clocks
}

/// A first packet half a second after the start counts that half second
/// unfilled at the stop, less the start latency.
#[test]
fn a_first_packet_half_a_second_late_counts_what_came_before_it() {
    let clocks = started_late(true, 50);
    assert_eq!(
        clocks.positions.missed_before_first(START_SIZES),
        24_000 - 960 - 480
    );
    let hour = started_late(true, 360_000 * 2);
    assert_eq!(
        hour.positions.missed_before_first(START_SIZES),
        MOST_READ_FRAMES as usize,
        "capped at an hour"
    );
}

/// A first packet within the start latency counts nothing.
#[test]
fn a_first_packet_inside_the_start_latency_counts_nothing() {
    for late_packets in [0, 1, 3] {
        assert_eq!(
            started_late(true, late_packets)
                .positions
                .missed_before_first(START_SIZES),
            0,
            "{late_packets} packets late"
        );
    }
    let mut clocks = Clocks::new(Some(48_000), 480, QPC_PACKET);
    clocks.positions.start(clocks.next.qpc);
    clocks.next.qpc += 3 * QPC_PACKET + QPC_PACKET / 2;
    clocks.clean(CALIBRATION_PACKETS);
    assert_eq!(
        clocks.positions.missed_before_first(START_SIZES),
        0,
        "half a period past the latency"
    );
}

/// No start given (endpoint loopback) or no sane QPC: a late first packet
/// counts nothing.
#[test]
fn a_stream_given_no_start_or_no_sane_qpc_counts_nothing_before_its_first_packet() {
    assert_eq!(
        started_late(false, 50)
            .positions
            .missed_before_first(START_SIZES),
        0,
        "endpoint loopback"
    );
    let mut measuring = Clocks::new(Some(48_000), 480, QPC_PACKET);
    measuring.positions.start(measuring.next.qpc);
    measuring.skip(50).clean(10);
    assert_eq!(
        measuring.positions.missed_before_first(START_SIZES),
        0,
        "still measuring"
    );
    let mut absent = Clocks::new(Some(48_000), 480, 0);
    absent.positions.start(1 << 40);
    absent.skip(50).clean(CALIBRATION_PACKETS);
    assert_eq!(
        absent.positions.missed_before_first(START_SIZES),
        0,
        "no QPC"
    );
}

/// A QPC with no clean pairs is never sane, and the stop's verdict calls it
/// not sane once 2 s came; under 2 s it stays not judged.
#[test]
fn a_stream_of_2_s_or_more_without_clean_qpc_pairs_is_judged_not_sane() {
    let run = |packets: u64, qpc: &dyn Fn(u64) -> u64, timestamp_error: bool| {
        let mut positions = DevicePositions::new(Some(48_000));
        for i in 0..packets {
            positions.packet(Packet {
                frames: 480,
                position: i * 480,
                qpc: qpc(i),
                discontinuity: false,
                timestamp_error,
            });
        }
        positions
    };
    let hour = 360_000;
    let ticking = |i: u64| (1 << 40) + i * QPC_PACKET;
    let absent = run(hour, &|_| 0, false);
    let errors = run(hour, &ticking, true);
    let twice = run(hour, &|i| (1 << 40) + i * 2 * QPC_PACKET, false);
    for (name, positions) in [("absent", absent), ("errors", errors), ("twice", twice)] {
        assert_eq!(positions.qpc_sane(), None, "{name}: the counting's verdict");
        assert_eq!(positions.qpc_judged(), Some(false), "{name}");
    }
    let calibration = QPC_CALIBRATION_FRAMES / 480;
    assert_eq!(
        run(calibration - 1, &|_| 0, false).qpc_judged(),
        None,
        "under 2 s"
    );
    assert_eq!(
        run(calibration, &|_| 0, false).qpc_judged(),
        Some(false),
        "2 s"
    );
    assert_eq!(
        run(hour, &ticking, false).qpc_judged(),
        Some(true),
        "a good QPC"
    );
}

/// A first packet with a timestamp error does not read the start; the next
/// does.
#[test]
fn the_first_packet_with_a_qpc_reads_the_start_past_the_frames_before_it() {
    let mut clocks = Clocks::new(Some(48_000), 480, QPC_PACKET);
    clocks.positions.start(clocks.next.qpc);
    clocks.skip(50);
    clocks.deliver(false, true);
    clocks.deliver(false, true);
    clocks.clean(CALIBRATION_PACKETS);
    assert_eq!(
        clocks.positions.missed_before_first(START_SIZES),
        24_000 - 960 - 480
    );
}

/// The same when that first packet's QPC is before the start.
#[test]
fn a_first_packet_with_a_timestamp_error_never_reads_the_start() {
    let mut clocks = Clocks::new(Some(48_000), 480, QPC_PACKET);
    clocks.positions.start(clocks.next.qpc);
    clocks.skip(50);
    let on_time = clocks.next.qpc;
    clocks.next.qpc -= 10_000_000;
    clocks.deliver(false, true);
    clocks.next.qpc = on_time + QPC_PACKET;
    clocks.clean(CALIBRATION_PACKETS);
    assert_eq!(
        clocks.positions.missed_before_first(START_SIZES),
        24_000 - 960 - 480
    );
}

/// Two hours on both streams with wrapping clocks and a loss every ten
/// minutes: nothing panics, every loss counts.
#[test]
fn two_hours_of_wrapping_clocks_count_every_loss() {
    let mut call = Call::new();
    let mut mic_positions = DevicePositions::new(Some(48_000));
    let mut system_positions = DevicePositions::new(None);
    let ticks = 2 * 3_600 * 100;
    let start_position = u64::MAX - 1_000_000;
    let start_qpc = u64::MAX - 50_000_000;
    let (mut mic_read, mut system_read) = (0, 0);
    let (mut tick, mut losses) = (0usize, 0usize);
    while tick < ticks {
        let lose = tick > 0 && tick.is_multiple_of(60_000);
        if lose {
            tick += 50;
            losses += 1;
        }
        let at = (tick * PERIOD) as u64;
        let flagged = Packet {
            frames: PERIOD,
            position: start_position.wrapping_add(at),
            qpc: start_qpc.wrapping_add(at * 10_000_000 / 48_000),
            discontinuity: lose,
            timestamp_error: false,
        };
        let system = system_positions.packet(flagged);
        if lose {
            system_read += system.fill + system.unfilled;
            call.follower_flagged(system);
        } else {
            call.follower_packet();
        }
        let mic = mic_positions.packet(flagged);
        if lose {
            mic_read += mic.fill + mic.unfilled;
            call.master.write_missed(&packet(&call.mic, 1), mic);
        }
        call.master_packet();
        tick += 1;
    }
    let lost = losses * 50 * PERIOD;
    assert_eq!((mic_read, system_read), (lost, lost));
    assert_eq!(
        call.sink.filled_samples(),
        BTreeMap::from([(AudioLane::Mic, lost), (AudioLane::System, lost)])
    );
}

/// Frozen positions among left-out pairs still measure from the pair after
/// their first move.
#[test]
fn a_first_move_after_left_out_pairs_is_left_out_too() {
    let mut positions = DevicePositions::new(Some(48_000));
    let mut packet = |frames: usize, position: u64, timestamp_error: bool, flagged: bool| {
        positions.packet(Packet {
            frames,
            position,
            discontinuity: flagged,
            timestamp_error,
            ..Packet::default()
        })
    };
    packet(59, 1_000, false, false);
    packet(960, 1_000, true, false);
    packet(960, 1_000, false, false);
    packet(960, 1_059, false, false);
    let missed = packet(960, 1_059 + 1_920, false, true);
    assert!(
        (960..=6 * 1_920).contains(&missed.unfilled),
        "a 960-frame idle read as {missed:?}"
    );
}

/// Positions that pause mid-stream keep their measured ratio.
#[test]
fn a_pause_of_the_positions_mid_stream_keeps_the_ratio() {
    let mut clocks = Clocks::new(None, 143, 0);
    clocks.clean(100);
    clocks.per_packet = 0;
    clocks.clean(1);
    clocks.per_packet = 143;
    clocks.clean(1);
    let missed = clocks.lose(50);
    assert!(
        (24_000..=6 * 24_480).contains(&missed.unfilled),
        "{missed:?}"
    );
}

/// A short pair mid-stream cannot replace the first, so a later loss reads
/// within a few percent.
#[test]
fn a_short_pair_mid_stream_replaces_nothing() {
    let mut clocks = Clocks::new(None, 143, 0);
    clocks.clean(100);
    clocks.next.position -= 72;
    clocks.clean(1);
    let missed = clocks.lose(50);
    assert!(
        (24_000..=24_000 + 24_000 / 20).contains(&missed.unfilled),
        "{missed:?}"
    );
}
