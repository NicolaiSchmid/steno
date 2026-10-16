//! The two-stream capture path the WASAPI backend runs (WP10a), without
//! WASAPI: the stream plan, the latency arithmetic, which engine answers
//! are trusted, the follower's
//! jitter-buffer policy and the master's routing into the sink, over
//! synthetic packets. Runs on every OS; on the Windows runner these are the
//! backend's unit tests, since the runner has no audio device. No Swift
//! counterpart.

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
    DevicePositions, MAX_BUFFER_FRAMES, POSITION_CALIBRATION_FRAMES, PositionVerdict, StreamSizes,
    far_end_latencies, frames_from_hundred_nanoseconds, stream_sizes,
};
use steno_audio::capture::{ChannelRef, SplitStreamPlan, StreamSource};
use steno_audio::realtime::{
    FollowerLane, LaneFrameSink, MISSED_FILL_LIMIT, Missed, PacketRouter, SliceView, StreamBody,
    deliver_slices,
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

/// The master body for `plan`, pulling from `follower` into `sink`.
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

#[test]
fn a_call_masters_on_the_microphone_and_follows_the_system_stream() {
    let plan = SplitStreamPlan::new(&CaptureMode::Call.lanes()).unwrap();
    assert_eq!(plan.master, StreamSource::Microphone);
    assert_eq!(plan.follower, Some(StreamSource::System));
    assert_eq!(
        plan.streams(),
        [StreamSource::Microphone, StreamSource::System]
    );
    let [mic, system] = plan.layout.sources.as_slice() else {
        panic!("two sources: {:?}", plan.layout.sources);
    };
    assert_eq!(mic.lane, AudioLane::Mic);
    assert_eq!(mic.left, ChannelRef::new(0, 0, 1));
    assert_eq!(system.lane, AudioLane::System);
    assert_eq!(system.left, ChannelRef::new(1, 0, 1));
    assert_eq!(system.right, None, "the follower is staged as mono");
}

#[test]
fn in_person_opens_the_microphone_alone() {
    let plan = SplitStreamPlan::new(&CaptureMode::InPerson.lanes()).unwrap();
    assert_eq!(plan.master, StreamSource::Microphone);
    assert_eq!(plan.follower, None);
    assert_eq!(plan.layout.sources.len(), 1);
    assert_eq!(plan.layout.sources[0].lane, AudioLane::Mixed);
    assert_eq!(plan.layout.sources[0].left, ChannelRef::new(0, 0, 1));
}

#[test]
fn a_system_only_capture_masters_on_the_stereo_system_stream() {
    let plan = SplitStreamPlan::new(&[AudioLane::System]).unwrap();
    assert_eq!(plan.master, StreamSource::System);
    assert_eq!(plan.follower, None);
    let source = plan.layout.sources[0];
    assert_eq!(source.left, ChannelRef::new(0, 0, 2));
    assert_eq!(source.right, Some(ChannelRef::new(0, 1, 2)));
    assert_eq!(StreamSource::System.channels(), 2);
    assert_eq!(StreamSource::Microphone.channels(), 1);
}

#[test]
fn no_lanes_is_refused() {
    assert!(matches!(
        SplitStreamPlan::new(&[]),
        Err(CaptureError::UnexpectedStreamLayout(_))
    ));
}

/// `positions` fed `packets` clean 10 ms packets of 48 kHz frames from
/// `position` on, advancing `per_packet` each: none reports a loss. Returns
/// where the next packet's position is.
fn measure(positions: &mut DevicePositions, position: u64, per_packet: u64, packets: usize) -> u64 {
    let mut position = position;
    for _ in 0..packets {
        assert!(
            positions.packet(Some(position), 480, false).is_none(),
            "a clean packet at {position}"
        );
        position += per_packet;
    }
    position
}

/// The packets that measure `POSITION_CALIBRATION_FRAMES`: one more than
/// the 10 ms packets in it, since each advance is read at the next packet.
const CALIBRATION_PACKETS: usize = POSITION_CALIBRATION_FRAMES as usize / 480 + 1;

/// WASAPI's device positions read in the unit they are measured in (A18 of
/// `.plans/2026-10-07-stable-promotion.md`): on a 44.1 kHz engine whose
/// positions count 441 a packet of 48 kHz frames, a jump is counted in
/// 48 kHz frames up to the fill limit and past it. A discontinuity while
/// they are measured (its advance left out of the sums) is counted, not
/// filled, read at one position a frame: 4 371 of the 4 800 frames lost.
#[test]
fn device_positions_count_at_the_engines_rate() {
    let mut positions = DevicePositions::new(Some(44_100));
    let mut position = measure(&mut positions, 0, 441, 100);
    assert_eq!(
        positions.packet(Some(position + 4_410), 480, true),
        Missed {
            fill: 0,
            unfilled: 4_410 + 441 - 480
        },
        "a discontinuity while measuring is counted, not filled"
    );
    position = measure(
        &mut positions,
        position + 4_410 + 441,
        441,
        CALIBRATION_PACKETS - 100,
    );
    assert_eq!(positions.verdict(), PositionVerdict::Fills);
    assert_eq!(
        positions.ratio(),
        Some(441.0 / 480.0),
        "the jump is not in the sums"
    );
    assert_eq!(
        positions.packet(Some(position + 4_410), 480, true).fill,
        4_800
    );
    position += 4_410 + 441;
    assert!(
        positions
            .packet(Some(position + 4_410), 480, false)
            .is_none(),
        "a jump on a packet not flagged is not read"
    );
    position += 4_410 + 441;
    assert_eq!(
        positions.packet(Some(position + 44_100), 480, true).fill,
        48_000,
        "a second at 44.1 kHz is the limit"
    );
    position += 44_100 + 441;
    assert_eq!(
        positions.packet(Some(position + 88_200), 480, true),
        Missed {
            fill: 0,
            unfilled: 96_000
        }
    );
}

/// Half a second lost before a flagged packet, read on an engine at
/// `engine_rate` whose positions advance `per_packet` a 10 ms packet: what
/// the packet reports, and the verdict.
fn half_a_second_lost(engine_rate: Option<u32>, per_packet: u64) -> (Missed, PositionVerdict) {
    let mut positions = DevicePositions::new(engine_rate);
    let position = measure(&mut positions, 1_000_000, per_packet, CALIBRATION_PACKETS);
    let missed = positions.packet(Some(position + 50 * per_packet), 480, true);
    (missed, positions.verdict())
}

/// Whichever unit the positions count in, the 48 kHz frames delivered or
/// the engine's, a jump is written for the half second it lost and no more:
/// a hands-free headset at 16 or 8 kHz writes no extra zeros, and a device
/// above 48 kHz misses no jump.
#[test]
fn device_positions_in_either_unit_fill_exactly_what_was_lost() {
    let filled = |fill| (Missed { fill, unfilled: 0 }, PositionVerdict::Fills);
    for rate in [8_000, 16_000, 44_100, 48_000, 96_000, 192_000] {
        assert_eq!(
            half_a_second_lost(Some(rate), 480),
            filled(24_000),
            "48 kHz frames on a {rate} Hz engine"
        );
        assert_eq!(
            half_a_second_lost(Some(rate), u64::from(rate) / 100),
            filled(24_000),
            "the engine's frames at {rate} Hz"
        );
    }
}

/// Positions whose advance neither unit explains fill nothing, and count
/// what was lost at the ratio measured, so the note says it: at 16 kHz
/// against a 48 kHz engine (one third of a position a frame), an engine
/// whose rate is unknown (process loopback with no mix format), or one off
/// by more than 1 %. Positions at 96 kHz against a 48 kHz engine (two a
/// frame) advance more than either unit explains on every packet, so they
/// never enter the sums: the stream stays measuring and counts at one
/// position a frame, about twice what was lost, which overstates the note
/// and writes nothing.
#[test]
fn device_positions_no_unit_explains_count_what_was_lost() {
    let unfilled = |unfilled| Missed { fill: 0, unfilled };
    let counts_only = (unfilled(24_000), PositionVerdict::CountsOnly);
    assert_eq!(half_a_second_lost(Some(48_000), 160), counts_only, "16 kHz");
    assert_eq!(half_a_second_lost(None, 480), counts_only, "no mix format");
    assert_eq!(
        half_a_second_lost(Some(44_100), 446),
        counts_only,
        "441 + 1.1 %"
    );
    assert_eq!(
        half_a_second_lost(Some(48_000), 960),
        (unfilled(51 * 960 - 480), PositionVerdict::Measuring),
        "96 kHz"
    );
    assert_eq!(
        half_a_second_lost(Some(44_100), 445).1,
        PositionVerdict::Fills,
        "441 + 0.9 %"
    );
    let mut positions = DevicePositions::new(Some(48_000));
    measure(&mut positions, 0, 160, CALIBRATION_PACKETS);
    assert_eq!(
        positions.to_string(),
        "device positions advanced 32000 over 96000 frames delivered (0.3333 a frame, engine at \
         48000 Hz), jumps counted, not filled"
    );
}

/// Positions that never move (as CI's process loopback reports them) never
/// panic on the audio thread, whatever the mix format says, a 0 Hz one
/// included: they measure no unit, fill nothing, and a flagged packet reads
/// no jump from them.
#[test]
fn positions_that_never_move_divide_nothing_by_zero() {
    for engine_rate in [None, Some(0), Some(16_000), Some(48_000)] {
        let mut positions = DevicePositions::new(engine_rate);
        measure(&mut positions, 7, 0, 2 * CALIBRATION_PACKETS);
        assert_eq!(
            positions.verdict(),
            PositionVerdict::CountsOnly,
            "{engine_rate:?}"
        );
        assert_eq!(positions.ratio(), Some(0.0));
        assert_eq!(positions.packet(Some(7), 480, true), Missed::default());
        assert_eq!(positions.packet(Some(7), 0, true), Missed::default());
    }
}

/// A position the engine flags as invalid starts the clock afresh: the
/// packet after it reports nothing, the one after that reads on.
#[test]
fn an_invalid_device_position_reports_nothing_and_starts_afresh() {
    let mut positions = DevicePositions::new(Some(48_000));
    let position = measure(&mut positions, 0, 480, CALIBRATION_PACKETS);
    assert!(positions.packet(None, 480, true).is_none());
    assert!(
        positions
            .packet(Some(position + 9_600), 480, true)
            .is_none()
    );
    assert_eq!(
        positions
            .packet(Some(position + 9_600 + 480 + 960), 480, true)
            .fill,
        960
    );
}

/// A flagged packet counts nothing while measuring or counting only when
/// its position sits behind the last packet's, or less than half a packet
/// past where that one ended (the engine's rounding, as on the other
/// backends); a jump of more counts in full.
#[test]
fn a_flagged_packet_counts_only_a_jump_of_more_than_half_a_packet() {
    for (engine_rate, packets) in [(Some(48_000), 10), (None, CALIBRATION_PACKETS)] {
        let mut positions = DevicePositions::new(engine_rate);
        let position = measure(&mut positions, 1_000_000, 480, packets);
        let last = position - 480;
        assert!(positions.packet(Some(last - 4_800), 480, true).is_none());
        assert!(positions.packet(Some(last), 480, false).is_none());
        assert!(
            positions
                .packet(Some(last + 480 + 240), 480, true)
                .is_none()
        );
        assert_eq!(
            positions.packet(Some(last + 2 * 480 + 240 + 241), 480, true),
            Missed {
                fill: 0,
                unfilled: 241
            },
            "{engine_rate:?}: {:?}",
            positions.verdict()
        );
    }
}

/// A clean packet whose position advanced more than either unit explains
/// is left out of the measurement: endpoint loopback delivers nothing
/// while nothing plays, and the packet after an idle of 25 ms or 10 s may
/// not be flagged. Measured with it, the 25 ms alone would put the ratio
/// 0.6 % off and the 10 s 3.5 times, and nothing would be filled; left
/// out, the positions measure exactly one a frame and a later loss fills.
/// A position 10 s behind the last one is left out the same way. On a
/// 96 kHz engine whose positions count its own frames, the bound is two
/// and a half positions a frame, so 25 ms idle is left out there too.
#[test]
fn an_idle_gap_or_a_step_back_does_not_skew_the_measurement() {
    for (rate, per_packet, step) in [
        (48_000, 480, 1_200),
        (48_000, 480, 480_000),
        (96_000, 960, 2_400),
        (48_000, 480, 0u64.wrapping_sub(480_000)),
    ] {
        let mut positions = DevicePositions::new(Some(rate));
        let position = measure(&mut positions, 1_000_000, per_packet, 50);
        let position = measure(
            &mut positions,
            position.wrapping_add(step),
            per_packet,
            CALIBRATION_PACKETS,
        );
        let at = |verdict| (verdict, Some(per_packet as f64 / 480.0));
        assert_eq!(
            (positions.verdict(), positions.ratio()),
            at(PositionVerdict::Fills),
            "{rate} Hz, a step of {step}"
        );
        assert_eq!(
            positions.packet(Some(position + 50 * per_packet), 480, true),
            Missed {
                fill: 24_000,
                unfilled: 0
            }
        );
    }
}

/// Positions that jitter by a few frames a packet around one a frame (480,
/// then 470 and 490 in turn) stay in the measurement: the bound allows half
/// a packet over either unit, so the ratio is exact and a loss fills.
#[test]
fn positions_that_jitter_stay_in_the_measurement() {
    let mut positions = DevicePositions::new(Some(48_000));
    let mut position = 0;
    for packet in 0..CALIBRATION_PACKETS {
        assert!(positions.packet(Some(position), 480, false).is_none());
        position += if packet % 2 == 0 { 470 } else { 490 };
    }
    assert_eq!(positions.verdict(), PositionVerdict::Fills);
    assert_eq!(positions.ratio(), Some(1.0));
}

/// Positions the engine flags as invalid leave the measurement alone: the
/// next valid packet's advance spans the invalid one, so it is not paired
/// with the packet before. Every other packet invalid (and half as long)
/// for a second, then clean packets: the positions measure exactly one a
/// frame. Paired across the invalid packets, they would measure 1.5 there.
#[test]
fn invalid_positions_are_left_out_of_the_measurement() {
    let mut positions = DevicePositions::new(Some(48_000));
    let mut position = 0;
    for _ in 0..100 {
        assert!(positions.packet(Some(position), 480, false).is_none());
        assert!(positions.packet(None, 240, false).is_none());
        position += 720;
    }
    assert_eq!(positions.ratio(), None, "no two valid packets in a row");
    measure(&mut positions, position, 480, CALIBRATION_PACKETS);
    assert_eq!(positions.verdict(), PositionVerdict::Fills);
    assert_eq!(positions.ratio(), Some(1.0));
}

/// What [`over_positions`] saw: what the flagged packet reported and the
/// verdict then, the sink's counts and the lanes' lengths, and the seconds the recorder's note
/// would say are missing (the lane that missed most's zeros, plus the
/// larger of the lane that left most unfilled and the master's shortfall
/// to wall time beyond the drift allowance).
#[derive(Debug)]
struct OverPositions {
    reported: Missed,
    verdict: PositionVerdict,
    filled: BTreeMap<AudioLane, usize>,
    unfilled: BTreeMap<AudioLane, usize>,
    mic_frames: usize,
    system_frames: usize,
    note: f64,
}

/// The two stream bodies of a call over 6 s of 10 ms ticks, each stream's
/// packets read by its own [`DevicePositions`] on an engine at
/// `engine_rate` whose positions advance `per_packet` a tick, as
/// `run_stream` does: the follower's (system) thread out for
/// `follower_out` ticks from tick `from`, or the master's (microphone) for
/// `master_out`, the packet after flagged as a discontinuity.
fn over_positions(
    engine_rate: Option<u32>,
    per_packet: u64,
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
    let mut reported = (Missed::default(), PositionVerdict::Measuring);
    let mut run = |body: &mut StreamBody,
                   positions: &mut DevicePositions,
                   next: SliceView<'_>,
                   tick: usize,
                   out: usize| {
        if (from..from + out).contains(&tick) {
            return;
        }
        let flagged = out > 0 && tick == from + out;
        let missed = positions.packet(Some(tick as u64 * per_packet), PERIOD, flagged);
        if flagged {
            reported = (missed, positions.verdict());
        }
        if !missed.is_none() {
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
        reported: reported.0,
        verdict: reported.1,
        note: most(&filled) + most(&unfilled).max(short),
        filled,
        unfilled,
        mic_frames,
        system_frames,
    }
}

/// The system stream's thread out for 0.9 s or 1.5 s, on the follower,
/// whose missed audio the master's pulls pad with zeros, so the master
/// stays on wall time and only the count can tell. Under `Fills` the loss
/// is filled up to the limit and unfilled past it; when no unit explains
/// the positions (no mix format, 16 kHz positions on a 48 kHz engine) and
/// while they are still measured (the thread out at 0.5 s), it is counted
/// unfilled. Every time the note counts it, and the lanes keep the
/// master's length.
#[test]
fn a_follower_loss_is_counted_whatever_the_positions_verdict() {
    for (name, rate, per_packet, from) in [
        ("fills", Some(48_000), 480, 300),
        ("no mix format", None, 480, 300),
        ("16 kHz positions", Some(48_000), 160, 300),
        ("measuring", Some(48_000), 480, 50),
    ] {
        for out in [90, 150] {
            let seen = over_positions(rate, per_packet, from, out, 0);
            let lost = out * PERIOD;
            let fills = name == "fills";
            let expected = if fills && lost as f64 <= SAMPLE_RATE * MISSED_FILL_LIMIT.as_secs_f64()
            {
                Missed {
                    fill: lost,
                    unfilled: 0,
                }
            } else {
                Missed {
                    fill: 0,
                    unfilled: lost,
                }
            };
            assert_eq!(seen.reported, expected, "{name}, out {out}: {seen:?}");
            assert_eq!(
                seen.verdict,
                match name {
                    "fills" => PositionVerdict::Fills,
                    "measuring" => PositionVerdict::Measuring,
                    _ => PositionVerdict::CountsOnly,
                },
                "{name}"
            );
            let on_the_system = |frames: usize| {
                (frames > 0)
                    .then_some((AudioLane::System, frames))
                    .into_iter()
                    .collect::<BTreeMap<_, _>>()
            };
            assert_eq!(
                seen.filled,
                on_the_system(expected.fill),
                "{name}, out {out}"
            );
            assert_eq!(
                seen.unfilled,
                on_the_system(expected.unfilled),
                "{name}, out {out}"
            );
            assert_eq!(
                (seen.mic_frames, seen.system_frames),
                (600 * PERIOD, 600 * PERIOD)
            );
            assert!(
                (seen.note - lost as f64 / SAMPLE_RATE).abs() < 1e-9,
                "{name}, out {out}: the note says {} s",
                seen.note
            );
        }
    }
}

/// The microphone's thread out for 1.5 s while its positions are still
/// measured (from 0.5 s): counted unfilled, so the note says it however
/// long the recording runs (the master's shortfall alone falls inside the
/// drift allowance from about 25 minutes on), and the system audio queued
/// for that time is dropped, so the lanes keep one length.
#[test]
fn a_master_loss_while_measuring_is_counted() {
    let seen = over_positions(Some(48_000), 480, 50, 0, 150);
    let lost = 150 * PERIOD;
    assert_eq!(
        seen.reported,
        Missed {
            fill: 0,
            unfilled: lost
        }
    );
    assert_eq!(seen.verdict, PositionVerdict::Measuring);
    assert!(seen.filled.is_empty(), "{seen:?}");
    assert_eq!(seen.unfilled.get(&AudioLane::Mic), Some(&lost));
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
        (seen.note - 1.5).abs() < 1e-9,
        "the note says {} s",
        seen.note
    );
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
    // Primed at 960; then the follower delivers 960 extra frames at once
    // and keeps one packet per pull: the queue stays at 1 440 after each
    // pull, above the high-water mark.
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
    // The first window held the priming pull's 480; the second has seen
    // three pulls at 1 440.
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

/// The slip takes the window's lowest queue, not the last pull's: a burst
/// that lands just before the window closes stays queued.
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

/// A re-prime after an underrun starts a fresh window, so a sustained
/// excess after it slips one window later.
#[test]
fn a_reprime_starts_a_fresh_slip_window() {
    let follower = FollowerLane::new(480, 960, 1_920, 48_000);
    let mut scratch = vec![0.0f32; 4_096];
    let mut out = vec![0.0f32; 480];
    follower.push(packet(&ramp(0, 960), 1), &mut scratch);
    follower.pull(&mut out);
    for start in [960, 1_440] {
        follower.push(packet(&ramp(start, 480), 1), &mut scratch);
        follower.pull(&mut out);
    }
    // Three pulls into the window, an underrun.
    follower.pull(&mut vec![0.0f32; 600]);
    assert!(follower.underrun_frames() > 0);
    follower.push(packet(&ramp(10_000, 2_400), 1), &mut scratch);
    let mut pushed = 12_400;
    let mut losses = Vec::new();
    for _ in 0..4 {
        losses.push(follower.pull(&mut out));
        follower.push(packet(&ramp(pushed, 480), 1), &mut scratch);
        pushed += 480;
    }
    assert_eq!(
        losses,
        vec![0, 0, 0, 1_440],
        "one window after the re-prime"
    );
}

/// A queue more than the master's buffer above the high-water mark cannot
/// be a late master, so it slips on the pull that sees it.
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

/// A slip at once starts a fresh window, so the excess queued before it
/// does not count toward the next window's end.
#[test]
fn a_slip_at_once_starts_a_fresh_window() {
    // Ten pulls a window; ceiling: high water 960 plus a 960-frame buffer.
    let follower = FollowerLane::new(480, 960, 4_800, 48_000).with_max_lateness(960);
    let mut scratch = vec![0.0f32; 4_096];
    let mut out = vec![0.0f32; 480];
    let mut pushed = 0;
    let mut feed = |frames: usize| {
        follower.push(packet(&ramp(pushed, frames), 1), &mut scratch);
        pushed += frames;
        follower.pull(&mut out)
    };
    // A first window at the target.
    assert_eq!(feed(960), 0);
    for _ in 0..9 {
        assert_eq!(feed(480), 0);
    }
    // Three pulls above the high-water mark, then a burst above the
    // ceiling.
    assert_eq!(feed(1_200), 0);
    assert_eq!(feed(480), 0);
    assert_eq!(feed(480), 0);
    assert_eq!(feed(1_440), 1_680, "2 160 left is above 1 920: at once");
    // Above the high-water mark again for nine pulls, one short of a
    // fresh window.
    assert_eq!(feed(1_200), 0);
    for pull in 0..8 {
        assert_eq!(feed(480), 0, "pull {pull} after the slip");
    }
    assert_eq!(follower.slipped_frames(), 1_680);
}

/// [`FollowerLane::for_streams`] sets the ceiling a master buffer above
/// the high-water mark: 1 440 + 960 for 10 ms periods behind 20 ms.
#[test]
fn the_stream_policy_slips_at_once_a_master_buffer_above_high_water() {
    let follower = FollowerLane::for_streams(480, 960);
    let mut scratch = vec![0.0f32; 4_096];
    let mut out = vec![0.0f32; 480];
    follower.push(packet(&ramp(0, 1_440), 1), &mut scratch);
    assert_eq!(follower.pull(&mut out), 0);
    assert_eq!(follower.queued(), 960);
    follower.push(packet(&ramp(1_440, 1_980), 1), &mut scratch);
    assert_eq!(
        follower.pull(&mut out),
        1_500,
        "2 460 left is above 2 400: at once"
    );
    assert_eq!(follower.queued(), 960);
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
    // `extra` frames at once, then a period pushed per period pulled for
    // two windows: the frames lost.
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
    // One period too many: 1 440 left after every pull is the high-water
    // mark, which a whole window at it does not pass.
    assert_eq!(sustain(480), 0, "at the high-water mark: no slip");
    // Two periods too many: one window later the lowest queue, 1 920,
    // slips to 960.
    assert_eq!(sustain(480), 960);
    assert_eq!(follower.slipped_frames(), 960);
    assert_eq!(follower.underrun_frames(), 0);
    assert_eq!(follower.queued(), 960, "back at the target");
}

/// Equal clocks, and the master's thread misses two periods and then
/// drains its three packets back to back. The queue rises for a moment
/// and falls back on its own: nothing slips and nothing underruns, and the
/// system lane keeps its lag. Judging each pull alone would discard 960
/// frames here and pad the next jitter with zeros.
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
        // The master misses ticks 50 and 51 and drains three packets at
        // 52; at 120 it runs twice, the second time before the follower's
        // next packet, and so skips 121.
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
    // Zeros only while priming, then every frame in order: the lag never
    // changed.
    let first = delivered.iter().position(|s| *s != 0.0).unwrap();
    assert_eq!(first, 2 * period, "two pulls of zeros while priming");
    let lane = &delivered[first..];
    assert_eq!(lane, ramp(1, lane.len()).as_slice());
}

/// A follower clock 0.5 % fast (2.4 extra frames per 480), sustained for
/// ten seconds: the queue creeps up, slips back each time a window stays
/// above the high-water mark, and never runs away or underruns.
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

    // A later re-prime trims nothing: that excess is recorded audio, which
    // only a sustained slip may drop.
    follower.pull(&mut out);
    follower.pull(&mut out);
    assert!(follower.underrun_frames() > 0);
    follower.push(packet(&ramp(10_000, 1_920), 1), &mut scratch);
    follower.pull(&mut out);
    assert_eq!(out, ramp(10_000, 480));
    assert_eq!(follower.trimmed_frames(), 3_840);
}

/// A pre-roll above the ceiling is trimmed at the first pull, not
/// slipped: nothing lost, nothing below the target skipped.
#[test]
fn a_pre_roll_above_the_ceiling_is_trimmed_not_slipped() {
    let follower = FollowerLane::for_streams(480, 960);
    let mut scratch = vec![0.0f32; 4_096];
    let mut out = vec![0.0f32; 480];
    follower.push(packet(&ramp(0, 8_000), 1), &mut scratch);
    assert_eq!(follower.pull(&mut out), 0, "a trim is not a loss");
    assert_eq!(out, ramp(6_560, 480));
    assert_eq!(follower.trimmed_frames(), 6_560);
    assert_eq!(follower.slipped_frames(), 0);
    assert_eq!(follower.queued(), 960);
}

/// The trim is decided at the master's first pull: a follower that starts
/// after the master delivers recorded audio, which is never trimmed.
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

/// A follower that fills its staging before the master's first pull (a
/// master slow to start) refuses its newest packet, so all it queued is
/// older than that gap: the first pull drops it and the refused packet,
/// uncounted, and the lane primes on what follows. An overflow after the
/// first pull is counted.
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

/// The follower pushing and the master pulling on two threads at once, as
/// the capture threads do: whatever the interleaving, the mic lane arrives
/// whole, both lanes stay the same length, the system lane keeps its order
/// with no torn sample, and every pushed follower frame is delivered,
/// counted as lost, trimmed before the master's first pull or still
/// queued. Small enough for every CI job; the
/// `tsan` job runs it under ThreadSanitizer.
#[test]
fn a_follower_and_a_master_on_two_threads_account_for_every_frame() {
    const PERIOD: usize = 480;
    const PACKETS: usize = 150;
    let lanes = [AudioLane::Mic, AudioLane::System];
    let plan = SplitStreamPlan::new(&lanes).unwrap();
    // Two seconds per lane hold all 150 periods, so nothing drains early.
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
    // A last empty pull hands over any staging overflow not yet reported.
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

// Missed audio (A18 of `.plans/2026-10-07-stable-promotion.md`)

/// 10 ms periods at 48 kHz.
const PERIOD: usize = 480;
/// The engine buffer: 100 ms, ten periods.
const ENGINE_PERIODS: usize = 10;

/// What [`run_outage`] saw in the sink.
#[derive(Debug)]
struct Outage {
    /// The microphone's frame less the system's at each sink position that
    /// holds both, once before the outage and at every change after it,
    /// with the period: the target (960) when the follower's thread runs
    /// first in each period, a period more when the master's does.
    lags: Vec<(usize, i64)>,
    mic_filled: usize,
    system_filled: usize,
    mic_unfilled: usize,
    system_unfilled: usize,
    system_dropped: usize,
    slipped: usize,
    /// The seconds the recorder's note would say are missing, as the
    /// session and the recorder count them: the lane that missed most's
    /// zeros, plus the larger of the lane that left most unfilled and the
    /// master's shortfall to wall time beyond the drift allowance.
    note: f64,
    /// The seconds truly missing: the master's shortfall to wall time, plus
    /// the microphone's zeros for what its thread missed.
    truth: f64,
}

/// The period [`run_outage`]'s outages start in.
const OUTAGE_START: usize = 50;

/// Period `t` of a stream's thread that is out for `out` periods from
/// [`OUTAGE_START`] (0: never): its packet; nothing while it is out; once
/// it is back, the ten oldest packets the engine kept, then `lost` (what
/// its device position says) and its packet.
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

/// [`run_outage_in_order`] with the follower's thread running first in
/// each period.
fn run_outage(follower_out: usize, master_out: usize) -> Outage {
    run_outage_in_order(follower_out, master_out, false)
}

/// The two stream bodies as the WASAPI backend builds them (the follower
/// behind a master with a 100 ms buffer that fills what it missed), over
/// 400 periods in which source frame `f` of either stream is `f + 1`: the
/// follower's thread out for `follower_out` periods and the master's for
/// `master_out` ([`outage_period`]), what each missed filled up to
/// [`MISSED_FILL_LIMIT`] and unfilled past it, as `run_stream` hands it
/// over. In each period the master's thread runs first when
/// `master_first`, the follower's otherwise.
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

/// Both threads out for 200 and 300 ms, as a stopped process is: the
/// master fills its lost part, which pulls the system audio queued for
/// that time; the follower only counts its own, which the master's pulls
/// pad with zeros, and nothing is counted twice. When the follower's
/// thread wakes first the system lane stays where it was behind the
/// microphone and nothing slips. When the master's does, its kept packets
/// find the follower's not yet queued, and the follower's land late, as
/// after an outage of the follower alone: the queue slips them back within
/// two slip windows, and the lag ends where it started.
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
        assert_eq!((outage.mic_filled, outage.system_filled), (lost, lost));
        let outage = run_outage_in_order(out, out, true);
        let (base, (period, lag)) = (outage.lags[0], *outage.lags.last().unwrap());
        assert_eq!(base, (40, 1_440), "master first, {out} periods: {outage:?}");
        assert_eq!(lag, base.1, "{outage:?}");
        assert!(period <= OUTAGE_START + out + 100, "{outage:?}");
        assert_eq!((outage.slipped, outage.system_dropped), (3_840, 3_840));
        assert_eq!((outage.mic_filled, outage.system_filled), (lost, lost));
        assert!((outage.note - outage.truth).abs() < 1e-9, "{outage:?}");
    }
}

/// Both threads out past the fill limit, 1.5 and 2.5 s, in either order:
/// the microphone's lane counts its jump unfilled and the system lane the
/// follower's, so the note counts the time once, as truly missing, and the
/// lag ends where it started. With the follower's thread first, the
/// packet it queued for the time the master resumes at stays queued: the
/// target it held from before the outage (20 ms) pairs with the master's
/// first two packets after it, and from the third the lag is back.
#[test]
fn an_outage_of_both_threads_past_the_fill_limit_counts_once() {
    for out in [150, 250] {
        let lost = (out - ENGINE_PERIODS) * PERIOD;
        for master_first in [false, true] {
            let outage = run_outage_in_order(out, out, master_first);
            let context = format!("{out} periods, master first {master_first}: {outage:?}");
            assert!((outage.note - outage.truth).abs() < 0.01, "{context}");
            assert!(
                (outage.truth - lost as f64 / SAMPLE_RATE).abs() < 1e-9,
                "{context}"
            );
            assert_eq!(
                (outage.mic_filled, outage.system_filled),
                (0, 0),
                "{context}"
            );
            assert_eq!((outage.mic_unfilled, outage.system_unfilled), (lost, lost));
            assert_eq!(outage.lags.last().unwrap().1, outage.lags[0].1, "{context}");
        }
        let outage = run_outage(out, out);
        let resumed = OUTAGE_START + ENGINE_PERIODS;
        assert_eq!(outage.lags.len(), 3, "{outage:?}");
        assert_eq!(outage.lags[1].0, resumed, "{outage:?}");
        assert_eq!(outage.lags[2], (resumed + 2, 960), "{outage:?}");
        // One thread alone: the master's loss is the microphone's, the
        // follower's the system audio's, and the note says either once.
        for (follower_out, master_out) in [(out, 0), (0, out)] {
            let outage = run_outage(follower_out, master_out);
            let context = format!("{follower_out} and {master_out} periods: {outage:?}");
            assert!(
                (outage.note - lost as f64 / SAMPLE_RATE).abs() < 0.01,
                "{context}"
            );
            assert_eq!(outage.lags.last().unwrap().1, 960, "{context}");
        }
    }
}

/// The master's thread alone out for 200 and 300 ms: the system audio
/// queued meanwhile rises past the old ceiling (its buffer above the
/// high-water mark), and the kept packets and the fill pull it all, so
/// nothing slips and the lanes stay aligned.
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

/// A jump the master's thread missed too long to fill counts on its lane
/// and drops the system audio queued for that time, counted on the system
/// lane, so the follower's lane loses the same span; the target and the
/// next packet's pull stay queued, the next packet's own time.
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

/// The follower's thread alone out for 150 to 300 ms: the master padded
/// its lane with zeros meanwhile, so the follower only counts what it lost.
/// Its kept packets still land late, as any late packets do, until the
/// queue slips back to the target within two slip windows, and that slip
/// stays the same however long the outage: zeros staged for the lost part
/// would queue up and slip as much real system audio again.
#[test]
fn a_stall_of_the_follower_alone_stages_no_zeros() {
    for out in [15, 20, 30] {
        let outage = run_outage(out, 0);
        assert_eq!(outage.system_filled, (out - ENGINE_PERIODS) * PERIOD);
        assert_eq!(outage.mic_filled, 0);
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
