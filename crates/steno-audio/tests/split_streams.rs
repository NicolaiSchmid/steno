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

use std::sync::Arc;

use steno_audio::capture::split_streams::{
    MAX_BUFFER_FRAMES, StreamSizes, far_end_latencies, frames_from_hundred_nanoseconds,
    stream_sizes,
};
use steno_audio::capture::{ChannelRef, SplitStreamPlan, StreamSource};
use steno_audio::realtime::{
    FollowerLane, LaneFrameSink, PacketRouter, SliceView, StreamBody, deliver_slices,
};
use steno_audio::{CaptureError, CaptureMode};
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
    follower.push(packet(&ramp(2_480, 480), 1), &mut scratch);
    follower.pull(&mut out);
    assert_eq!(out, ramp(2_000, 480), "primed again, nothing skipped");
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

#[test]
fn a_period_policy_keeps_two_periods_and_slips_one_period_above() {
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
    let mut scratch = vec![0.0f32; 512];
    follower.push(packet(&ramp(0, 1_000), 1), &mut scratch);
    follower.push(packet(&ramp(1_000, 100), 1), &mut scratch);
    assert_eq!(follower.queued(), 1_000, "the second packet did not fit");

    let lanes = [AudioLane::Mic, AudioLane::System];
    let plan = SplitStreamPlan::new(&lanes).unwrap();
    let sink = LaneFrameSink::new(&lanes);
    let mut router = PacketRouter::new(plan.layout.sources, Some(Arc::clone(&follower)), 480);
    router.route(packet(&ramp(0, 480), 1), &sink);
    let dropped = sink.dropped_samples();
    assert_eq!(dropped.get(&AudioLane::System), Some(&100));
    assert_eq!(dropped.get(&AudioLane::Mic), None);

    router.route(packet(&ramp(480, 480), 1), &sink);
    assert_eq!(
        sink.dropped_samples().get(&AudioLane::System),
        Some(&100),
        "an overflow is reported once"
    );
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
