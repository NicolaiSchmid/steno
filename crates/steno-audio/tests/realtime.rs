//! The real-time promise, checked rather than grepped: the IOProc body
//! (`LaneFrameSink` producer calls through `deliver`), the processing loop
//! with the real Speex canceller, its far-end delay line, metering, the
//! raw-mic copy and the relay hand-off run one second of audio on the
//! test's thread under the counting allocator and allocate nothing.
//! Swift: `Tests/StenoAudioTests/RealTimeAllocationTests.swift` (Darwin's
//! `malloc_logger` hook); here the crate's own `#[global_allocator]`, so it
//! runs on every OS. Plan invariant 5.

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

use steno_audio::capture::{ChannelRef, LaneSource, SplitStreamPlan, StreamLayout};
use steno_audio::realtime::{
    BufferView, FollowerLane, FrameRelay, LaneFrameSink, PacketRouter, ProcessingConfiguration,
    ProcessingThread, SliceView, StreamBody, deliver,
};
use steno_audio::testing::AudioFixtures;
use steno_audio::testing::rt::CountingAllocator;
use steno_audio::writer::Resampler48kTo16k;
use steno_audio::{FRAME_SIZE, SpeexEchoCanceller};
use steno_core::AudioLane;

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const CALLBACK_FRAMES: usize = 512;

/// Preallocated 48 kHz material: a mono mic and an interleaved stereo tap.
struct Material {
    mic: Vec<f32>,
    tap: Vec<f32>,
    frames: usize,
}

impl Material {
    fn new(seconds: f64) -> Self {
        let frames = (seconds * 48_000.0) as usize;
        let near = AudioFixtures::tone(320.0, seconds, 0.25);
        let far = AudioFixtures::tone(1_000.0, seconds, 0.5);
        let mut mic = vec![0.0f32; frames];
        let mut tap = vec![0.0f32; frames * 2];
        for index in 0..frames {
            mic[index] = near[index]
                + if index >= 2_880 {
                    0.5 * far[index - 2_880]
                } else {
                    0.0
                };
            tap[2 * index] = far[index];
            tap[2 * index + 1] = far[index] * 0.5;
        }
        Self { mic, tap, frames }
    }

    /// What the IOProc does per callback, for every callback of the
    /// material: the HAL's two buffers (mono mic, interleaved stereo tap)
    /// through `deliver` with the resolved layout.
    fn deliver(&self, sink: &LaneFrameSink, sources: &[LaneSource]) {
        let mut offset = 0;
        while offset < self.frames {
            let count = CALLBACK_FRAMES.min(self.frames - offset);
            let views = [
                BufferView {
                    channels: 1,
                    data: Some(self.mic[offset..].as_ptr()),
                    byte_size: count * 4,
                },
                BufferView {
                    channels: 2,
                    data: Some(self.tap[2 * offset..].as_ptr()),
                    byte_size: count * 8,
                },
            ];
            // SAFETY: both vectors hold at least `count` frames from `offset`.
            unsafe { deliver(&views, sources, sink) };
            offset += count;
        }
    }
}

#[test]
fn the_allocator_sees_a_deliberate_allocation_on_this_thread() {
    let seen = CountingAllocator::allocations_during(|| {
        let vector = vec![1.0f32; 100_000];
        std::hint::black_box(&vector);
        drop(vector);
    });
    assert!(
        seen >= 1,
        "the counter must observe a deliberate allocation, or it proves nothing"
    );
    assert!(CountingAllocator::total() > 0);
}

#[test]
fn producer_processing_and_relay_allocate_nothing_after_warm_up() {
    let lanes = [AudioLane::Mic, AudioLane::System];
    let layout = StreamLayout::resolve(&lanes, &[1, 2], &[vec![], vec![1]], &[2], Some(1)).unwrap();
    assert_eq!(layout.sources[0].left, ChannelRef::new(0, 0, 1));
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let relay = Arc::new(FrameRelay::new(3, FRAME_SIZE, 128));
    let mut configuration = ProcessingConfiguration::new(
        &lanes,
        Some(Box::new(
            SpeexEchoCanceller::new(48_000.0, FRAME_SIZE).unwrap(),
        )),
    );
    configuration.far_end_delay_frames = 7_200;
    configuration.keep_raw_mic = true;
    // Never started: `drain_on_caller` runs the loop body on this thread.
    let mut thread =
        ProcessingThread::new(Arc::clone(&sink), Arc::clone(&relay), configuration, None);

    // Warm-up outside the count: ten frames let Speex touch any lazily
    // initialised state once (the plan's "after the first second").
    let warm_up = Material::new(0.1);
    warm_up.deliver(&sink, &layout.sources);
    thread.drain_on_caller();
    assert_eq!(thread.frames_processed(), 10);

    let second = Material::new(1.0);
    let allocations = CountingAllocator::allocations_during(|| {
        second.deliver(&sink, &layout.sources);
        thread.drain_on_caller();
    });
    assert_eq!(
        allocations, 0,
        "{allocations} allocations on the real-time path"
    );
    assert_eq!(thread.frames_processed(), 110);
    assert!(sink.dropped_samples().is_empty());
    assert_eq!(relay.dropped_frames(), vec![0, 0, 0]);
    assert_eq!(relay.available_frames(), 110);
    assert_eq!(thread.levels().current_generation(), 11);
    assert!(
        (thread.system_peak() - 0.375).abs() < 0.01,
        "the folded stereo tap peaks at (0.5 + 0.25) / 2"
    );
    println!(
        "allocations inside the IOProc and processing path: {allocations} (process total {})",
        CountingAllocator::total()
    );
}

#[test]
fn the_sidecar_resampler_allocates_nothing_after_init() {
    let mut resampler = Resampler48kTo16k::new(FRAME_SIZE);
    let input = AudioFixtures::tone(1_000.0, 0.01, 0.5);
    let mut output = vec![0i16; resampler.output_frame_size()];
    resampler.process(&input, &mut output);
    let allocations = CountingAllocator::allocations_during(|| {
        for _ in 0..100 {
            resampler.process(&input, &mut output);
        }
    });
    assert_eq!(
        allocations, 0,
        "{allocations} allocations in 100 resampler frames"
    );
}

/// The WASAPI capture threads' per-packet bodies (WP10a), driven with
/// synthetic packets, no audio device: the follower folding stereo loopback
/// packets into its staging ring, the master routing mono microphone
/// packets plus the staged frames into the sink. One second at WASAPI's
/// 10 ms period, with a silent loopback packet, a follower burst that
/// slips at the high-water mark and a gap that underruns and re-primes, all
/// on this thread under the counting allocator: nothing allocates. On the
/// Windows runner this is the backend's real-time proof, since the runner
/// has no device to drive the COM loop with.
#[test]
fn the_two_stream_bodies_allocate_nothing() {
    const PERIOD: usize = 480;
    let lanes = [AudioLane::Mic, AudioLane::System];
    let plan = SplitStreamPlan::new(&lanes).unwrap();
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let follower = Arc::new(FollowerLane::for_period(PERIOD));
    let mut master = StreamBody::Master {
        router: PacketRouter::new(
            plan.layout.sources.clone(),
            Some(Arc::clone(&follower)),
            4_800,
        ),
        sink: Arc::clone(&sink),
    };
    let mut staging = StreamBody::follower(Arc::clone(&follower), 4_800);
    let material = Material::new(1.1);
    let mut processed = vec![0.0f32; 110 * PERIOD];

    let mut step = |period: usize| {
        let offset = period * PERIOD;
        let mic = &material.mic[offset..offset + PERIOD];
        let tap = &material.tap[2 * offset..2 * (offset + PERIOD)];
        // Periods 40 to 44 deliver no loopback packets (an underrun and a
        // re-prime); period 60 delivers a burst of six (a slip); period 30
        // is flagged silent.
        let loopback_packets = match period {
            40..45 => 0,
            60 => 6,
            _ => 1,
        };
        for _ in 0..loopback_packets {
            staging.handle(SliceView {
                frames: PERIOD,
                channels: 2,
                samples: (period != 30).then_some(tap),
            });
        }
        master.handle(SliceView {
            frames: PERIOD,
            channels: 1,
            samples: Some(mic),
        });
    };
    // Warm-up outside the count.
    for period in 0..10 {
        step(period);
    }
    let allocations = CountingAllocator::allocations_during(|| {
        for period in 10..110 {
            step(period);
        }
        // The consumer side as the processing thread drains it.
        let available = sink.available_to_read();
        sink.ring(0).read(&mut processed[..available]);
        sink.ring(1).read(&mut processed[..available]);
    });
    assert_eq!(
        allocations, 0,
        "{allocations} allocations in the WASAPI per-packet bodies"
    );
    assert_eq!(
        sink.available_to_read(),
        0,
        "110 periods were delivered and drained"
    );
    assert!(follower.underrun_frames() > 0, "the gap underran");
    assert!(follower.slipped_frames() > 0, "the burst slipped");
    assert_eq!(
        sink.dropped_samples().get(&AudioLane::System),
        Some(&follower.slipped_frames()),
        "slipped follower frames count as dropped system samples"
    );
    assert_eq!(sink.dropped_samples().get(&AudioLane::Mic), None);
    println!(
        "allocations in the two-stream bodies: {allocations}; underrun {} frames, slipped {}",
        follower.underrun_frames(),
        follower.slipped_frames()
    );
}
