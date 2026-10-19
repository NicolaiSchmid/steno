//! The real-time promise, checked rather than grepped: every body on the
//! real-time path (the IOProc's `deliver`, PipeWire's gate and view, the
//! WASAPI stream bodies, the missed-audio fill, the processing loop with
//! Speex, the silent output) runs on this thread under the crate's
//! counting allocator and allocates nothing.

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

use std::collections::BTreeMap;
use std::sync::Arc;

use steno_audio::capture::{ChannelRef, LaneSource, SplitStreamPlan, StreamLayout};
use steno_audio::realtime::{
    BufferView, CycleClock, FirstCallback, FollowerLane, FrameRelay, LaneFrameSink, Missed,
    PacketRouter, ProcessingConfiguration, ProcessingThread, SliceView, StreamBody, deliver,
    deliver_slices, interleaved_view,
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
    /// material, through `deliver` with the resolved layout.
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

    // Warm-up outside the count: Speex touches its lazy state once.
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

/// The macOS IOProc's first-callback mark allocates nothing.
#[test]
fn the_first_callback_mark_allocates_nothing() {
    let first = FirstCallback::new();
    let allocations = CountingAllocator::allocations_during(|| {
        for host_time in 1_000..2_000 {
            first.mark(host_time);
        }
    });
    assert_eq!(allocations, 0);
    assert_eq!(first.host_time(), Some(1_000));
}

/// At 24 and 22.05 kHz the loop converts every lane to 48 kHz before the
/// canceller without allocating, leaving partial frames behind.
#[test]
fn the_converting_processing_loop_allocates_nothing_after_warm_up() {
    // Half a window is held back, so 4 800 and then 52 800 device samples
    // convert to 19 and 219 whole frames at 24 kHz, 21 and 239 at 22.05.
    for (rate, warm_frames, frames) in [(24_000.0, 19, 219), (22_050.0, 21, 239)] {
        let lanes = [AudioLane::Mic, AudioLane::System];
        let layout =
            StreamLayout::resolve(&lanes, &[1, 2], &[vec![], vec![1]], &[2], Some(1)).unwrap();
        let sink = Arc::new(LaneFrameSink::new(&lanes));
        let relay = Arc::new(FrameRelay::new(3, FRAME_SIZE, 256));
        let mut configuration = ProcessingConfiguration::new(
            &lanes,
            Some(Box::new(
                SpeexEchoCanceller::new(48_000.0, FRAME_SIZE).unwrap(),
            )),
        );
        configuration.device_rate = rate;
        configuration.far_end_delay_frames = 7_200;
        configuration.keep_raw_mic = true;
        let mut thread =
            ProcessingThread::new(Arc::clone(&sink), Arc::clone(&relay), configuration, None);

        let warm_up = Material::new(0.1);
        warm_up.deliver(&sink, &layout.sources);
        thread.drain_on_caller();
        assert_eq!(thread.frames_processed(), warm_frames, "{rate} Hz");

        let second = Material::new(1.0);
        let allocations = CountingAllocator::allocations_during(|| {
            second.deliver(&sink, &layout.sources);
            thread.drain_on_caller();
        });
        assert_eq!(
            allocations, 0,
            "{rate} Hz: {allocations} allocations on the converting path"
        );
        assert_eq!(thread.frames_processed(), frames, "{rate} Hz");
        assert_eq!(sink.available_to_read(), 0);
        assert!(sink.dropped_samples().is_empty());
        assert_eq!(relay.dropped_frames(), vec![0, 0, 0]);
    }
}

/// `work` through the PipeWire capture's gate, as `process` delivers it.
fn through_the_gate(work: impl FnOnce()) {
    #[cfg(target_os = "linux")]
    assert!(steno_audio::capture::live::pipewire::through_an_open_gate(
        work
    ));
    #[cfg(not(target_os = "linux"))]
    work();
}

/// `samples` as the bytes of a mapped PipeWire buffer.
fn as_bytes(samples: &[f32]) -> &[u8] {
    // SAFETY: initialised `f32`s are valid bytes; the length is the
    // slice's in bytes.
    unsafe { std::slice::from_raw_parts(samples.as_ptr().cast::<u8>(), samples.len() * 4) }
}

#[test]
fn the_pipewire_process_body_allocates_nothing() {
    // A call: one buffer of three channels, the microphone and the
    // monitor's two.
    const CHANNELS: usize = 3;
    const CYCLE: usize = 1_024;
    let lanes = [AudioLane::Mic, AudioLane::System];
    let sources = [
        LaneSource {
            lane: AudioLane::Mic,
            left: ChannelRef::new(0, 0, CHANNELS),
            right: None,
        },
        LaneSource {
            lane: AudioLane::System,
            left: ChannelRef::new(0, 1, CHANNELS),
            right: Some(ChannelRef::new(0, 2, CHANNELS)),
        },
    ];
    let sink = LaneFrameSink::new(&lanes);
    let material = Material::new(1.0);
    let mut interleaved = vec![0.0f32; material.frames * CHANNELS];
    for (index, frame) in interleaved
        .as_chunks_mut::<CHANNELS>()
        .0
        .iter_mut()
        .enumerate()
    {
        frame[0] = material.mic[index];
        frame[1] = material.tap[2 * index];
        frame[2] = material.tap[2 * index + 1];
    }
    // The buffer PipeWire maps, refilled every cycle as the graph does.
    let mut memory = vec![0.0f32; CYCLE * CHANNELS];

    let allocations = CountingAllocator::allocations_during(|| {
        let mut offset = 0;
        while offset < material.frames {
            let count = CYCLE.min(material.frames - offset);
            memory[..count * CHANNELS]
                .copy_from_slice(&interleaved[offset * CHANNELS..(offset + count) * CHANNELS]);
            let stride = CHANNELS * 4;
            let view = interleaved_view(
                Some(as_bytes(&memory)),
                0,
                (count * stride) as u32,
                i32::try_from(stride).unwrap(),
                CHANNELS,
            );
            through_the_gate(|| {
                deliver_slices(&[view], &sources, &sink);
            });
            offset += count;
        }
    });
    assert_eq!(
        allocations, 0,
        "{allocations} allocations in the PipeWire process body"
    );
    assert!(sink.dropped_samples().is_empty());
    assert_eq!(sink.available_to_read(), material.frames);
    let mut mic = vec![0.0f32; material.frames];
    let mut system = vec![0.0f32; material.frames];
    assert!(sink.ring(0).read(&mut mic) && sink.ring(1).read(&mut system));
    assert_eq!(mic, material.mic, "the microphone is channel 0, untouched");
    for (index, sample) in system.iter().enumerate() {
        let expected = f32::midpoint(material.tap[2 * index], material.tap[2 * index + 1]);
        assert_eq!(*sample, expected, "the monitor folds to mono at {index}");
    }
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

/// The WASAPI threads' per-packet bodies over synthetic packets (a silent
/// packet, an underrun and re-prime, a burst that slips) allocate nothing.
/// On the Windows runner, which has no device, this is the backend's
/// real-time proof.
#[test]
fn the_two_stream_bodies_allocate_nothing() {
    const PERIOD: usize = 480;
    let lanes = [AudioLane::Mic, AudioLane::System];
    let plan = SplitStreamPlan::new(&lanes).unwrap();
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let follower = Arc::new(FollowerLane::for_streams(PERIOD, 4_800));
    let mut master = StreamBody::Master {
        router: PacketRouter::new(
            plan.layout.sources.clone(),
            Some(Arc::clone(&follower)),
            4_800,
        ),
        sink: Arc::clone(&sink),
    };
    let mut staging = StreamBody::follower(Arc::clone(&follower), 4_800);
    let material = Material::new(1.7);
    let mut processed = vec![0.0f32; 160 * PERIOD];

    let mut step = |period: usize| {
        let offset = period * PERIOD;
        let mic = &material.mic[offset..offset + PERIOD];
        let tap = &material.tap[2 * offset..2 * (offset + PERIOD)];
        // Periods 40 to 44 deliver no loopback (underrun, re-prime); period
        // 60 a burst of six that slips later; period 30 is flagged silent.
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
        for period in 10..160 {
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
        "160 periods were delivered and drained"
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

/// The missed-audio fill (the cycle clock and the zeros through the sink
/// and both two-stream bodies) allocates nothing; the rings end up with
/// every frame of the timeline, the missed ones as zeros.
#[test]
fn the_missed_cycle_fill_allocates_nothing() {
    const CYCLE: usize = 480;
    let lanes = [AudioLane::Mic, AudioLane::System];
    let sources = [
        LaneSource {
            lane: AudioLane::Mic,
            left: ChannelRef::new(0, 0, 2),
            right: None,
        },
        LaneSource {
            lane: AudioLane::System,
            left: ChannelRef::new(0, 1, 2),
            right: None,
        },
    ];
    let sink = LaneFrameSink::new(&lanes);
    let memory = vec![0.25f32; CYCLE * 2];
    let mut clock = CycleClock::new(48_000.0);
    let allocations = CountingAllocator::allocations_during(|| {
        for cycle in 0..100u64 {
            if cycle % 10 == 9 {
                continue;
            }
            let view = SliceView {
                channels: 2,
                frames: CYCLE,
                samples: Some(&memory),
            };
            let missed = clock.advance(cycle * CYCLE as u64, CYCLE);
            through_the_gate(|| {
                sink.write_missed(missed);
                deliver_slices(&[view], &sources, &sink);
            });
        }
    });
    assert_eq!(allocations, 0, "{allocations} allocations in the fill");
    assert_eq!(
        sink.available_to_read(),
        99 * CYCLE,
        "the last miss is never seen"
    );
    let missed = 9 * CYCLE;
    assert_eq!(
        sink.filled_samples(),
        BTreeMap::from([(AudioLane::Mic, missed), (AudioLane::System, missed)]),
    );
    assert!(sink.dropped_samples().is_empty());

    // The master misses 4 periods and the follower 2, then each a jump past
    // the fill limit.
    let filled = |fill| Missed { fill, unfilled: 0 };
    let unfilled = |unfilled| Missed { fill: 0, unfilled };
    let plan = SplitStreamPlan::new(&lanes).unwrap();
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let follower = Arc::new(FollowerLane::for_streams(CYCLE, 4_800));
    let mut master = StreamBody::Master {
        router: PacketRouter::new(
            plan.layout.sources.clone(),
            Some(Arc::clone(&follower)),
            4_800,
        ),
        sink: Arc::clone(&sink),
    };
    let mut staging = StreamBody::follower(Arc::clone(&follower), 4_800);
    let mic = vec![0.5f32; CYCLE];
    let tap = vec![0.25f32; 2 * CYCLE];
    let (mono, stereo) = (
        SliceView {
            frames: CYCLE,
            channels: 1,
            samples: Some(&mic),
        },
        SliceView {
            frames: CYCLE,
            channels: 2,
            samples: Some(&tap),
        },
    );
    let allocations = CountingAllocator::allocations_during(|| {
        for period in 0..50 {
            if period == 20 {
                staging.write_missed(&stereo, filled(2 * CYCLE));
            }
            if period == 35 {
                staging.write_missed(&stereo, unfilled(150 * CYCLE));
            }
            staging.handle(stereo);
            if period == 5 {
                master.write_missed(&mono, filled(4 * CYCLE));
            }
            if period == 40 {
                master.write_missed(&mono, unfilled(200 * CYCLE));
            }
            master.handle(mono);
        }
    });
    assert_eq!(
        allocations, 0,
        "{allocations} allocations in the bodies' fill"
    );
    assert_eq!(
        sink.available_to_read(),
        54 * CYCLE,
        "50 periods and the 4 missed"
    );
    // The follower lane padded 2 periods and re-primed; its next
    // discontinuity comes past the hold, so those zeros are not its loss.
    assert_eq!(
        sink.filled_samples(),
        BTreeMap::from([(AudioLane::Mic, 4 * CYCLE), (AudioLane::System, 2 * CYCLE)]),
        "the master's zeros on its lane, the follower's count on its own"
    );
    assert_eq!(
        sink.unfilled_samples(),
        BTreeMap::from([
            (AudioLane::Mic, 200 * CYCLE),
            (AudioLane::System, 150 * CYCLE)
        ]),
        "each thread's jump past the limit on its own lane, nothing written"
    );
}

/// The silent output IOProc zeroes every buffer it is handed within its
/// size, skips null and empty ones, and allocates nothing.
#[cfg(target_os = "macos")]
#[test]
fn the_silent_output_allocates_nothing() {
    use std::ptr::NonNull;

    use objc2_core_audio_types::AudioBuffer;
    use steno_audio::realtime::silence_output;

    /// An `AudioBufferList` of four buffers, laid out as the HAL's.
    #[repr(C)]
    struct List {
        count: u32,
        buffers: [AudioBuffer; 4],
    }
    let mut stereo = vec![0.75f32; 2 * CALLBACK_FRAMES];
    let mut sized_zero = vec![0.5f32; 16];
    let mut mono = vec![-0.25f32; CALLBACK_FRAMES];
    let buffer = |channels: u32, data: *mut f32, floats: usize| AudioBuffer {
        mNumberChannels: channels,
        mDataByteSize: (4 * floats) as u32,
        mData: data.cast(),
    };
    let mut list = List {
        count: 4,
        buffers: [
            buffer(2, stereo.as_mut_ptr(), stereo.len()),
            buffer(2, std::ptr::null_mut(), stereo.len()),
            buffer(1, sized_zero.as_mut_ptr(), 0),
            buffer(1, mono.as_mut_ptr(), CALLBACK_FRAMES / 2),
        ],
    };
    let output = NonNull::from(&mut list).cast();
    let allocations = CountingAllocator::allocations_during(|| {
        // SAFETY: every non-null `mData` points at a vector of at least
        // `mDataByteSize` bytes that outlives the call, and `List` has the
        // layout of an `AudioBufferList` of four buffers.
        unsafe { silence_output(output) };
    });
    assert_eq!(
        allocations, 0,
        "{allocations} allocations in the silent output"
    );
    assert!(stereo.iter().all(|s| s.to_bits() == 0), "zeroed in full");
    assert!(
        sized_zero.iter().all(|s| *s == 0.5),
        "a size of 0 is left alone"
    );
    let (written, past) = mono.split_at(CALLBACK_FRAMES / 2);
    assert!(
        written.iter().all(|s| s.to_bits() == 0),
        "the last buffer, past the null and the empty one"
    );
    assert!(past.iter().all(|s| *s == -0.25), "nothing past the size");
}
