//! The ring and the two `LaneRings` shapes built on it.
//! Swift: `Tests/StenoAudioTests/LaneRingBufferTests.swift`,
//! `LaneFrameSinkTests.swift`.

// Test arithmetic: sample counts and dB values cast freely, and sample
// rates compare exactly on purpose.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::cast_lossless
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::time::Duration;

use steno_audio::capture::DeviceChangeReason;
use steno_audio::realtime::{FrameRelay, LaneFrameSink, LaneRingBuffer};
use steno_core::AudioLane;

#[test]
fn capacity_rounds_up_to_a_power_of_two() {
    assert_eq!(LaneRingBuffer::new(1000).capacity(), 1024);
    assert_eq!(LaneRingBuffer::new(1024).capacity(), 1024);
    assert_eq!(LaneRingBuffer::new(1).capacity(), 2);
}

#[test]
fn write_then_read_round_trips_across_the_wrap_point() {
    let ring = LaneRingBuffer::new(8);
    let mut out = [0.0f32; 5];
    for round in 0..5 {
        let input: Vec<f32> = (0..5).map(|i| (round * 10 + i) as f32).collect();
        assert!(ring.write_slice(&input));
        assert_eq!(ring.available_to_read(), 5);
        assert!(ring.read(&mut out));
        assert_eq!(out.to_vec(), input);
    }
    assert_eq!(ring.dropped_samples(), 0);
}

#[test]
fn strided_write_deinterleaves() {
    let ring = LaneRingBuffer::new(8);
    let interleaved: [f32; 6] = [1.0, 100.0, 2.0, 200.0, 3.0, 300.0];
    // SAFETY: six floats, read from index 1 every second float: 1, 3, 5.
    assert!(unsafe { ring.write(interleaved.as_ptr().add(1), 3, 2) });
    let mut out = [0.0f32; 3];
    assert!(ring.read(&mut out));
    assert_eq!(out, [100.0, 200.0, 300.0]);
}

#[test]
fn mixed_write_averages_two_channels() {
    let ring = LaneRingBuffer::new(8);
    let left = [1.0f32, 1.0, 1.0];
    let right = [0.0f32, 0.5, -1.0];
    // SAFETY: both slices hold three floats at stride 1.
    assert!(unsafe { ring.write_mixed(left.as_ptr(), right.as_ptr(), 3, 1, 1) });
    let mut out = [0.0f32; 3];
    assert!(ring.read(&mut out));
    assert_eq!(out, [0.5, 0.75, 0.0]);
}

#[test]
fn full_ring_refuses_the_whole_block_and_counts_it() {
    let ring = LaneRingBuffer::new(8);
    let block = [1.0f32; 6];
    assert!(ring.write_slice(&block));
    assert!(!ring.write_slice(&block));
    assert!(!ring.has_room(3));
    assert!(ring.has_room(2));
    assert_eq!(ring.dropped_samples(), 6);
    assert_eq!(
        ring.available_to_read(),
        6,
        "a refused write changes nothing"
    );
    let mut seven = [0.0f32; 7];
    assert!(!ring.read(&mut seven), "short reads are refused as a whole");
    let mut six = [0.0f32; 6];
    assert!(ring.read(&mut six));
}

/// The ring holds exactly `capacity` samples: full is full (one more zero
/// is refused and counted), empty is empty (a one-sample read is refused),
/// and the wrap point moves every round.
#[test]
fn a_ring_holds_exactly_its_capacity_and_empties_again() {
    let ring = LaneRingBuffer::new(8);
    let block: Vec<f32> = (0..8).map(|i| i as f32).collect();
    let partial = [-1.0f32, -2.0, -3.0];
    let mut out = [0.0f32; 8];
    for round in 0..4 {
        assert!(ring.write_slice(&partial));
        assert!(ring.read(&mut out[..3]));
        assert_eq!(&out[..3], &partial);
        assert!(ring.write_slice(&block), "round {round}");
        assert_eq!(ring.available_to_read(), 8);
        assert_eq!(ring.available_to_write(), 0);
        assert!(!ring.has_room(1));
        assert!(!ring.write_zeros(1), "full: one more sample is refused");
        assert!(ring.read(&mut out));
        assert_eq!(out.to_vec(), block, "round {round}");
        assert_eq!(ring.available_to_read(), 0);
        assert_eq!(ring.available_to_write(), 8);
        assert!(!ring.read(&mut out[..1]), "empty: a read is refused");
    }
    assert_eq!(ring.dropped_samples(), 4, "one refused zero per round");
}

#[test]
fn clear_zeroes_storage_and_resets_indices() {
    let ring = LaneRingBuffer::new(4);
    let block = [1.0f32, 2.0, 3.0];
    ring.write_slice(&block);
    ring.write_slice(&block);
    ring.clear();
    assert_eq!(ring.available_to_read(), 0);
    assert_eq!(ring.dropped_samples(), 0);
    ring.write_zeros(4);
    let mut out = [9.0f32; 4];
    ring.read(&mut out);
    assert_eq!(out, [0.0; 4]);
}

/// A producer thread writes 480-sample blocks as fast as it can while a
/// consumer thread drains; every sample is either read in order or counted
/// as dropped, and nothing is duplicated or lost.
#[test]
fn concurrent_producer_and_consumer_account_for_every_sample() {
    let ring = Arc::new(LaneRingBuffer::new(4096));
    let block = 480;
    let blocks = 2_000;
    let producer_done = Arc::new(AtomicBool::new(false));

    let producer = {
        let ring = Arc::clone(&ring);
        let done = Arc::clone(&producer_done);
        std::thread::spawn(move || {
            let mut buffer = vec![0.0f32; block];
            for index in 0..blocks {
                buffer.fill(index as f32);
                ring.write_slice(&buffer);
            }
            done.store(true, Ordering::Release);
        })
    };
    let consumer = {
        let ring = Arc::clone(&ring);
        let done = Arc::clone(&producer_done);
        std::thread::spawn(move || {
            let mut buffer = vec![0.0f32; block];
            let mut last_block: i64 = -1;
            let mut ordered = true;
            let mut count = 0usize;
            loop {
                if ring.read(&mut buffer) {
                    let value = buffer[0] as i64;
                    if buffer.iter().any(|s| *s as i64 != value) || value <= last_block {
                        ordered = false;
                    }
                    last_block = value;
                    count += 1;
                } else if done.load(Ordering::Acquire) {
                    // One more look: the producer may have written between
                    // the failed read and the flag.
                    if !ring.read(&mut buffer) {
                        break;
                    }
                    count += 1;
                } else {
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
            (count, ordered)
        })
    };
    producer.join().unwrap();
    let (read_blocks, ordered) = consumer.join().unwrap();
    let dropped = ring.dropped_samples() / block;
    assert_eq!(
        read_blocks + dropped,
        blocks,
        "read {read_blocks} + dropped {dropped}"
    );
    assert!(ordered);
}

#[test]
fn a_callback_that_fits_one_lane_but_not_the_other_is_refused_for_both() {
    // 1 000 samples of headroom round up to 1 024 per ring.
    let sink = LaneFrameSink::with_handler(
        &[AudioLane::Mic, AudioLane::System],
        1_000.0,
        1.0,
        Box::new(|_| {}),
    );
    let block = [0.5f32; 400];
    for _ in 0..2 {
        assert!(sink.begin_callback(400));
        sink.write_slice(0, &block);
        sink.write_slice(1, &block);
        sink.end_callback();
    }
    // Drain the mic ring alone; the system ring still holds 800 of 1 024.
    let mut scratch = vec![0.0f32; 800];
    assert!(sink.ring(0).read(&mut scratch));
    assert!(sink.ring(0).has_room(400));
    assert!(!sink.ring(1).has_room(400));
    assert!(!sink.begin_callback(400), "refused as a whole");
    assert_eq!(
        sink.dropped_samples().into_iter().collect::<Vec<_>>(),
        vec![(AudioLane::Mic, 400), (AudioLane::System, 400)],
        "counted on every lane"
    );
    assert_eq!(
        sink.ring(0).available_to_read(),
        0,
        "nothing landed on the lane that had room"
    );
    assert_eq!(sink.ring(1).available_to_read(), 800);
    assert_eq!(
        sink.available_to_read(),
        0,
        "the consumer sees the minimum over lanes"
    );
}

#[test]
fn silence_keeps_the_lane_aligned_and_a_device_change_fires_once_until_rearmed() {
    let count = Arc::new(AtomicUsize::new(0));
    let (sender, last) = channel();
    let sink = {
        let count = Arc::clone(&count);
        LaneFrameSink::with_handler(
            &[AudioLane::Mic, AudioLane::System],
            1_000.0,
            1.0,
            Box::new(move |reason| {
                count.fetch_add(1, Ordering::Relaxed);
                let _ = sender.send(reason);
            }),
        )
    };
    let block = [1.0f32, 2.0, 3.0];
    assert!(sink.begin_callback(3));
    sink.write_slice(0, &block);
    sink.write_silence(1);
    sink.end_callback();
    assert!(sink.wake().try_take(), "one wake per callback");
    assert!(!sink.wake().try_take());
    assert_eq!(
        sink.available_to_read(),
        3,
        "the silent lane advanced with the other"
    );
    let mut out = [9.0f32; 3];
    sink.ring(1).read(&mut out);
    assert_eq!(out, [0.0, 0.0, 0.0]);
    sink.ring(0).read(&mut out);
    assert_eq!(out, [1.0, 2.0, 3.0]);

    sink.report_device_change(DeviceChangeReason::DefaultInputChanged);
    sink.report_device_change(DeviceChangeReason::OutputDeviceGone);
    assert_eq!(
        count.load(Ordering::Relaxed),
        1,
        "later reports are ignored"
    );
    assert_eq!(
        last.try_recv().unwrap(),
        DeviceChangeReason::DefaultInputChanged
    );
    sink.rearm_device_change();
    sink.report_device_change(DeviceChangeReason::SampleRateChanged);
    sink.report_device_change(DeviceChangeReason::InputDeviceGone);
    assert_eq!(
        count.load(Ordering::Relaxed),
        2,
        "rearming lets exactly one more through"
    );
    assert_eq!(
        last.try_recv().unwrap(),
        DeviceChangeReason::SampleRateChanged
    );

    sink.begin_callback(3);
    sink.write_slice(0, &block);
    sink.write_slice(1, &block);
    sink.end_callback();
    sink.clear();
    assert_eq!(sink.available_to_read(), 0);
    assert!(sink.dropped_samples().is_empty());
}

#[test]
fn the_relay_refuses_and_counts_a_frame_on_every_channel() {
    // Two frames of four samples per channel: the ring is exactly eight.
    let relay = FrameRelay::new(3, 4, 2);
    let frame = [1.0f32, 2.0, 3.0, 4.0];
    for _ in 0..2 {
        assert!(relay.begin_frame());
        for channel in 0..3 {
            relay.write(channel, &frame);
        }
        relay.end_frame();
    }
    assert_eq!(relay.available_frames(), 2);
    assert!(!relay.begin_frame(), "full: refused for every channel");
    assert_eq!(relay.dropped_frames(), vec![1, 1, 1]);
    let mut out = [0.0f32; 4];
    assert!(relay.read(2, &mut out));
    assert_eq!(out, frame);
    assert_eq!(
        relay.available_frames(),
        1,
        "whole frames every channel has: the minimum"
    );
    assert!(relay.wake().try_take());
    assert!(relay.wake().try_take());
    assert!(!relay.wake().try_take(), "no wake for the refused frame");
}
