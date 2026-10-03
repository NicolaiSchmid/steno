//! The two-stream capture path the WASAPI backend runs (WP10), without
//! WASAPI: the stream plan, the latency arithmetic, the follower's
//! jitter-buffer policy and the master's routing into the sink, over
//! synthetic packets. Runs on every OS; on the Windows runner these are the
//! backend's unit tests, since the runner has no audio device. No Swift
//! equivalent.

// Test arithmetic: sample counts cast freely, and samples compare exactly
// on purpose (the path copies, folds by halves or writes zeros).
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::float_cmp,
    clippy::doc_markdown
)]

use std::sync::Arc;

use steno_audio::capture::split_streams::{far_end_latencies, frames_from_hundred_nanoseconds};
use steno_audio::capture::{ChannelRef, SplitStreamPlan, StreamSource};
use steno_audio::realtime::{FollowerLane, LaneFrameSink, Packet, PacketRouter, StreamBody};
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

fn packet(samples: &[f32], channels: usize) -> Packet<'_> {
    Packet {
        frames: samples.len() / channels,
        channels,
        samples: Some(samples),
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
fn the_follower_writes_zeros_until_primed_then_keeps_the_target_queued() {
    let follower = FollowerLane::new(960, 2_880, 48_000);
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
    let follower = FollowerLane::new(480, 1_440, 48_000);
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
fn a_fast_follower_slips_back_to_the_target_and_reports_the_loss() {
    let follower = FollowerLane::new(480, 960, 48_000);
    let mut scratch = vec![0.0f32; 4_096];
    let mut out = vec![0.0f32; 480];
    // 2 400 queued: after one pull of 480, 1 920 remain, above 960.
    follower.push(packet(&ramp(0, 2_400), 1), &mut scratch);
    let lost = follower.pull(&mut out);
    assert_eq!(out, ramp(0, 480));
    assert_eq!(lost, 1_440, "1 920 queued slips to the 480 target");
    assert_eq!(follower.slipped_frames(), 1_440);
    assert_eq!(follower.queued(), 480);
    follower.pull(&mut out);
    assert_eq!(out, ramp(1_920, 480), "the newest frames survive the slip");
}

#[test]
fn a_period_policy_keeps_two_periods_and_slips_one_period_above() {
    let follower = FollowerLane::for_period(480);
    assert_eq!(follower.target(), 960, "two periods");
    let mut scratch = vec![0.0f32; 480];
    let mut out = vec![0.0f32; 480];
    // 1 920 queued: 1 440 after a pull is the high-water mark, no slip.
    follower.push(packet(&ramp(0, 1_920), 1), &mut scratch);
    assert_eq!(follower.pull(&mut out), 0);
    assert_eq!(follower.queued(), 1_440);
    // Two more periods: 1 920 after a pull passes it and slips to 960.
    follower.push(packet(&ramp(1_920, 960), 1), &mut scratch);
    assert_eq!(follower.pull(&mut out), 960);
    assert_eq!(out, ramp(480, 480));
    assert_eq!(follower.queued(), 960);
    assert_eq!(follower.slipped_frames(), 960);
}

#[test]
fn the_follower_folds_stereo_and_stages_silent_packets_as_zeros() {
    let follower = FollowerLane::new(0, 4_800, 48_000);
    // A fold buffer smaller than the packet folds it in pieces.
    let mut scratch = vec![0.0f32; 100];
    follower.push(packet(&stereo_ramp(0, 480), 2), &mut scratch);
    follower.push(
        Packet {
            frames: 480,
            channels: 2,
            samples: None,
        },
        &mut scratch,
    );
    // A packet shorter than it claims becomes zeros, not a short write.
    let short = stereo_ramp(0, 10);
    follower.push(
        Packet {
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
    let follower = Arc::new(FollowerLane::new(0, 1_024, 1_024));
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
}

#[test]
fn the_master_routes_its_packet_and_the_followers_frames_as_one_callback() {
    let lanes = [AudioLane::Mic, AudioLane::System];
    let plan = SplitStreamPlan::new(&lanes).unwrap();
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let follower = Arc::new(FollowerLane::new(480, 2_400, 48_000));
    let mut master = StreamBody::Master {
        router: PacketRouter::new(
            plan.layout.sources.clone(),
            Some(Arc::clone(&follower)),
            480,
        ),
        sink: Arc::clone(&sink),
    };
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
    router.route(
        Packet {
            frames: 100,
            channels: 1,
            samples: None,
        },
        &sink,
    );
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
    let follower = Arc::new(FollowerLane::new(0, 4_800, 48_000));
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
