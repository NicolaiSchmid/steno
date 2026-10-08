//! The stream layout, the IOProc body on buffer lists built by hand, the
//! device snapshot comparison and the nominal-rate settle.
//! Swift: `Tests/StenoAudioTests/StreamLayoutTests.swift`,
//! `IOProcRunnerTests.swift`, `DeviceSnapshotTests.swift`,
//! `DeviceSetupTests.swift`.

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

use std::time::Duration;

use steno_audio::capture::{
    CaptureError, ChannelRef, DeviceChangeReason, DeviceSnapshot, LaneSource, NominalSampleRate,
    StreamLayout,
};
use steno_audio::realtime::{BufferView, LaneFrameSink, deliver};
use steno_core::AudioLane;

fn source(lane: AudioLane, left: ChannelRef, right: Option<ChannelRef>) -> LaneSource {
    LaneSource { lane, left, right }
}

/// Built-in speakers (no input), built-in mic (mono), interleaved stereo tap.
#[test]
fn call_with_separate_mic_and_interleaved_tap() {
    let layout = StreamLayout::resolve(
        &[AudioLane::Mic, AudioLane::System],
        &[1, 2],
        &[vec![], vec![1]],
        &[2],
        Some(1),
    )
    .unwrap();
    assert!(!layout.tap_first);
    assert_eq!(
        layout.sources,
        vec![
            source(AudioLane::Mic, ChannelRef::new(0, 0, 1), None),
            source(
                AudioLane::System,
                ChannelRef::new(1, 0, 2),
                Some(ChannelRef::new(1, 1, 2))
            ),
        ]
    );
}

/// The HAL puts the tap first and delivers it non-interleaved: the two tap
/// channels live in different buffers, each with stride 1.
#[test]
fn tap_first_non_interleaved() {
    let layout = StreamLayout::resolve(
        &[AudioLane::Mic, AudioLane::System],
        &[1, 1, 2],
        &[vec![], vec![2]],
        &[1, 1],
        Some(1),
    )
    .unwrap();
    assert!(layout.tap_first);
    assert_eq!(
        layout.sources[0],
        source(AudioLane::Mic, ChannelRef::new(2, 0, 2), None)
    );
    assert_eq!(
        layout.sources[1],
        source(
            AudioLane::System,
            ChannelRef::new(0, 0, 1),
            Some(ChannelRef::new(1, 0, 1))
        )
    );
}

/// A USB interface is both the output device and the microphone: one
/// sub-device carrying two mono input streams.
#[test]
fn shared_input_output_device() {
    let layout = StreamLayout::resolve(
        &[AudioLane::Mic, AudioLane::System],
        &[1, 1, 2],
        &[vec![1, 1]],
        &[2],
        Some(0),
    )
    .unwrap();
    assert_eq!(
        layout.sources[0],
        source(AudioLane::Mic, ChannelRef::new(0, 0, 1), None)
    );
    assert_eq!(layout.sources[1].left.buffer, 2);
}

#[test]
fn tap_first_interleaved() {
    let layout = StreamLayout::resolve(
        &[AudioLane::Mic, AudioLane::System],
        &[2, 1],
        &[vec![], vec![1]],
        &[2],
        Some(1),
    )
    .unwrap();
    assert!(layout.tap_first);
    assert_eq!(
        layout.sources,
        vec![
            source(AudioLane::Mic, ChannelRef::new(1, 0, 1), None),
            source(
                AudioLane::System,
                ChannelRef::new(0, 0, 2),
                Some(ChannelRef::new(0, 1, 2))
            ),
        ]
    );
}

/// A stereo microphone next to a stereo tap has the same shape in either
/// order; the resolver then assumes sub-devices first.
#[test]
fn equal_shapes_assume_sub_devices_first() {
    let layout = StreamLayout::resolve(
        &[AudioLane::Mic, AudioLane::System],
        &[2, 2],
        &[vec![], vec![2]],
        &[2],
        Some(1),
    )
    .unwrap();
    assert!(!layout.tap_first);
    assert_eq!(
        layout.sources[0],
        source(AudioLane::Mic, ChannelRef::new(0, 0, 2), None)
    );
    assert_eq!(layout.sources[1].left.buffer, 1);
    assert!(layout.sources[1].right.is_some(), "a stereo tap is folded");
}

#[test]
fn in_person_uses_the_mic_only_and_a_mono_tap_has_no_fold() {
    let layout =
        StreamLayout::resolve(&[AudioLane::Mixed], &[1], &[vec![], vec![1]], &[], Some(1)).unwrap();
    assert_eq!(
        layout.sources,
        vec![source(AudioLane::Mixed, ChannelRef::new(0, 0, 1), None)]
    );
    let mono = StreamLayout::resolve(&[AudioLane::System], &[1], &[vec![]], &[1], None).unwrap();
    assert_eq!(
        mono.sources,
        vec![source(AudioLane::System, ChannelRef::new(0, 0, 1), None)]
    );
}

#[test]
fn mismatched_shapes_fail() {
    let mismatch = |lanes: &[AudioLane], agg: &[usize], sub: &[Vec<usize>], tap: &[usize], mic| {
        matches!(
            StreamLayout::resolve(lanes, agg, sub, tap, mic),
            Err(CaptureError::UnexpectedStreamLayout(_))
        )
    };
    assert!(mismatch(
        &[AudioLane::Mic, AudioLane::System],
        &[2, 2],
        &[vec![], vec![1]],
        &[2],
        Some(1)
    ));
    assert!(mismatch(&[AudioLane::Mic], &[2], &[vec![]], &[2], None));
    assert!(mismatch(
        &[AudioLane::System],
        &[1],
        &[vec![1]],
        &[],
        Some(0)
    ));
}

/// One input buffer: its channel count and interleaved samples, or no
/// samples for a buffer the HAL delivered without data.
struct Buffer {
    channels: usize,
    samples: Option<Vec<f32>>,
}

fn run_deliver(buffers: &[Buffer], frames: usize, layout: &StreamLayout, sink: &LaneFrameSink) {
    let views: Vec<BufferView> = buffers
        .iter()
        .map(|b| BufferView {
            channels: b.channels,
            data: b.samples.as_ref().map(Vec::as_ptr),
            byte_size: frames * b.channels * 4,
        })
        .collect();
    // SAFETY: every data pointer refers to a vector alive for the call,
    // holding `frames * channels` floats.
    unsafe { deliver(&views, &layout.sources, sink) };
}

/// `samples` as a buffer of `channels` whose byte size reports `frames`
/// frames, which may be fewer than `samples` holds but never more.
fn view(samples: &[f32], channels: usize, frames: usize) -> BufferView {
    assert!(frames * channels <= samples.len());
    BufferView {
        channels,
        data: Some(samples.as_ptr()),
        byte_size: frames * channels * 4,
    }
}

fn read(sink: &LaneFrameSink, lane: usize, count: usize) -> Vec<f32> {
    let mut out = vec![f32::NAN; count];
    sink.ring(lane).read(&mut out);
    out
}

/// Built-in mic (mono) then the tap (interleaved stereo).
#[test]
fn sub_devices_first_with_an_interleaved_tap() {
    let layout = StreamLayout::resolve(
        &[AudioLane::Mic, AudioLane::System],
        &[1, 2],
        &[vec![], vec![1]],
        &[2],
        Some(1),
    )
    .unwrap();
    let sink = LaneFrameSink::new(&[AudioLane::Mic, AudioLane::System]);
    run_deliver(
        &[
            Buffer {
                channels: 1,
                samples: Some(vec![1.0, 2.0, 3.0, 4.0]),
            },
            Buffer {
                channels: 2,
                samples: Some(vec![10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0]),
            },
        ],
        4,
        &layout,
        &sink,
    );
    assert_eq!(sink.available_to_read(), 4);
    assert_eq!(read(&sink, 0, 4), vec![1.0, 2.0, 3.0, 4.0]);
    assert_eq!(
        read(&sink, 1, 4),
        vec![15.0, 35.0, 55.0, 75.0],
        "stereo folded to mono"
    );
    assert!(sink.wake().try_take());
    assert!(!sink.wake().try_take(), "one wake per callback");
    assert!(sink.dropped_samples().is_empty());
}

/// The tap first and non-interleaved, the microphone a stereo device: the
/// mic lane is channel 0 of buffer 2 read with stride 2.
#[test]
fn tap_first_non_interleaved_with_a_stereo_microphone() {
    let layout = StreamLayout::resolve(
        &[AudioLane::Mic, AudioLane::System],
        &[1, 1, 2],
        &[vec![], vec![2]],
        &[1, 1],
        Some(1),
    )
    .unwrap();
    let sink = LaneFrameSink::new(&[AudioLane::Mic, AudioLane::System]);
    run_deliver(
        &[
            Buffer {
                channels: 1,
                samples: Some(vec![1.0; 4]),
            },
            Buffer {
                channels: 1,
                samples: Some(vec![3.0; 4]),
            },
            Buffer {
                channels: 2,
                samples: Some(vec![5.0, -5.0, 6.0, -6.0, 7.0, -7.0, 8.0, -8.0]),
            },
        ],
        4,
        &layout,
        &sink,
    );
    assert_eq!(
        read(&sink, 0, 4),
        vec![5.0, 6.0, 7.0, 8.0],
        "left channel of the stereo mic"
    );
    assert_eq!(
        read(&sink, 1, 4),
        vec![2.0; 4],
        "two mono tap buffers averaged"
    );
}

#[test]
fn a_buffer_without_data_becomes_silence() {
    let layout = StreamLayout::resolve(
        &[AudioLane::Mic, AudioLane::System],
        &[1, 2],
        &[vec![], vec![1]],
        &[2],
        Some(1),
    )
    .unwrap();
    let sink = LaneFrameSink::new(&[AudioLane::Mic, AudioLane::System]);
    run_deliver(
        &[
            Buffer {
                channels: 1,
                samples: Some(vec![1.0, 2.0, 3.0, 4.0]),
            },
            Buffer {
                channels: 2,
                samples: None,
            },
        ],
        4,
        &layout,
        &sink,
    );
    assert_eq!(sink.available_to_read(), 4);
    assert_eq!(read(&sink, 0, 4), vec![1.0, 2.0, 3.0, 4.0]);
    assert_eq!(read(&sink, 1, 4), vec![0.0; 4]);
}

/// The layout was resolved for the first channel of a stereo microphone
/// and an interleaved stereo tap. A live buffer list shaped differently
/// must never be read through those pointers: a tap that arrives mono (the
/// stride no longer matches), a microphone channel beyond the buffer's
/// channels, and a buffer shorter than the callback's frames, by half or by
/// a single frame, each become silence, and the lanes stay aligned on the
/// frames the first buffer carried. Where a wrong read would stay inside
/// the vector, the vector is longer than its reported size, so a guard that
/// lets it through shows up as audio instead of an out-of-bounds read.
#[test]
fn a_buffer_shaped_unlike_the_layout_becomes_silence() {
    let layout = StreamLayout::resolve(
        &[AudioLane::Mic, AudioLane::System],
        &[2, 2],
        &[vec![], vec![2]],
        &[2],
        Some(1),
    )
    .unwrap();
    // Stereo mic (channel 0 of buffer 0 at stride 2) and a stereo tap.
    assert_eq!(layout.sources[0].left, ChannelRef::new(0, 0, 2));
    assert_eq!(layout.sources[1].left, ChannelRef::new(1, 0, 2));
    assert_eq!(layout.sources[1].right, Some(ChannelRef::new(1, 1, 2)));

    // The tap arrives mono: its stride is 1 where the layout says 2.
    let sink = LaneFrameSink::new(&[AudioLane::Mic, AudioLane::System]);
    run_deliver(
        &[
            Buffer {
                channels: 2,
                samples: Some(vec![1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0, -4.0]),
            },
            Buffer {
                channels: 1,
                samples: Some(vec![9.0; 4]),
            },
        ],
        4,
        &layout,
        &sink,
    );
    assert_eq!(sink.available_to_read(), 4);
    assert_eq!(read(&sink, 0, 4), vec![1.0, 2.0, 3.0, 4.0]);
    assert_eq!(
        read(&sink, 1, 4),
        vec![0.0; 4],
        "mis-strided tap is silence"
    );

    // The tap buffer holds two frames where the microphone's holds four:
    // its byte size is short of the callback's frames.
    let sink = LaneFrameSink::new(&[AudioLane::Mic, AudioLane::System]);
    let mic = [1.0f32, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0, -4.0];
    let short_tap = [9.0f32; 4];
    let views = [view(&mic, 2, 4), view(&short_tap, 2, 2)];
    // SAFETY: both pointers refer to arrays alive for the call, each at
    // least as long as its byte size says (`view` asserts it).
    unsafe { deliver(&views, &layout.sources, &sink) };
    assert_eq!(sink.available_to_read(), 4);
    assert_eq!(read(&sink, 0, 4), vec![1.0, 2.0, 3.0, 4.0]);
    assert_eq!(
        read(&sink, 1, 4),
        vec![0.0; 4],
        "short tap buffer is silence"
    );

    // The microphone's channel is beyond a buffer that shrank to mono; the
    // frame count still comes from that first buffer.
    let mut shifted = layout.clone();
    shifted.sources[0].left = ChannelRef::new(0, 1, 2);
    let sink = LaneFrameSink::new(&[AudioLane::Mic, AudioLane::System]);
    run_deliver(
        &[
            Buffer {
                channels: 1,
                samples: Some(vec![1.0, 2.0, 3.0, 4.0]),
            },
            Buffer {
                channels: 2,
                samples: Some(vec![10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0]),
            },
        ],
        4,
        &shifted,
        &sink,
    );
    assert_eq!(sink.available_to_read(), 4);
    assert_eq!(read(&sink, 0, 4), vec![0.0; 4], "channel beyond the buffer");
    assert_eq!(read(&sink, 1, 4), vec![15.0, 35.0, 55.0, 75.0]);

    // The tap's buffer is exactly one frame short of the callback's four
    // (three frames by its byte size, four in the vector).
    let sink = LaneFrameSink::new(&[AudioLane::Mic, AudioLane::System]);
    let tap = [9.0f32; 8];
    let views = [view(&mic, 2, 4), view(&tap, 2, 3)];
    // SAFETY: as above.
    unsafe { deliver(&views, &layout.sources, &sink) };
    assert_eq!(sink.available_to_read(), 4);
    assert_eq!(read(&sink, 0, 4), vec![1.0, 2.0, 3.0, 4.0]);
    assert_eq!(
        read(&sink, 1, 4),
        vec![0.0; 4],
        "a tap buffer one frame short is silence"
    );

    // The microphone's channel is the buffer's channel count at the right
    // stride: only the offset compare rejects it.
    let mut beyond = layout.clone();
    beyond.sources[0].left = ChannelRef::new(0, 2, 2);
    let sink = LaneFrameSink::new(&[AudioLane::Mic, AudioLane::System]);
    let padded_mic = [1.0f32, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0, -4.0, 5.0, -5.0];
    let stereo_tap = [10.0f32, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0];
    let views = [view(&padded_mic, 2, 4), view(&stereo_tap, 2, 4)];
    // SAFETY: as above.
    unsafe { deliver(&views, &beyond.sources, &sink) };
    assert_eq!(sink.available_to_read(), 4);
    assert_eq!(
        read(&sink, 0, 4),
        vec![0.0; 4],
        "offset equal to the channels"
    );
    assert_eq!(read(&sink, 1, 4), vec![15.0, 35.0, 55.0, 75.0]);
}

/// The microphone's buffer arrives with no channels, with no bytes, or not
/// at all: the callback's length comes from the tap's buffer, the microphone lane is
/// silence and the tap's audio still lands.
#[test]
fn an_unusable_first_buffer_costs_only_its_own_lane() {
    let layout = StreamLayout::resolve(
        &[AudioLane::Mic, AudioLane::System],
        &[1, 2],
        &[vec![], vec![1]],
        &[2],
        Some(1),
    )
    .unwrap();
    let tap = [10.0f32, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0];
    let tap_view = view(&tap, 2, 4);
    let no_channels = BufferView {
        channels: 0,
        data: None,
        byte_size: 0,
    };
    let sink = LaneFrameSink::new(&[AudioLane::Mic, AudioLane::System]);
    // SAFETY: the one data pointer refers to an array alive for the call.
    unsafe { deliver(&[no_channels, tap_view], &layout.sources, &sink) };
    assert_eq!(sink.available_to_read(), 4);
    assert_eq!(read(&sink, 0, 4), vec![0.0; 4]);
    assert_eq!(read(&sink, 1, 4), vec![15.0, 35.0, 55.0, 75.0]);

    // The shape the HAL delivers for an input with nothing this cycle: its
    // channel count, no data and no bytes.
    let empty = BufferView {
        channels: 1,
        data: None,
        byte_size: 0,
    };
    let sink = LaneFrameSink::new(&[AudioLane::Mic, AudioLane::System]);
    // SAFETY: as above.
    unsafe { deliver(&[empty, tap_view], &layout.sources, &sink) };
    assert_eq!(sink.available_to_read(), 4);
    assert_eq!(read(&sink, 0, 4), vec![0.0; 4]);
    assert_eq!(read(&sink, 1, 4), vec![15.0, 35.0, 55.0, 75.0]);

    // A buffer list with no buffer for the microphone at all, and one with
    // no usable buffer: the second has no length and writes nothing.
    let mut missing = layout.clone();
    missing.sources[0].left = ChannelRef::new(5, 0, 1);
    let sink = LaneFrameSink::new(&[AudioLane::Mic, AudioLane::System]);
    // SAFETY: as above.
    unsafe { deliver(&[no_channels, tap_view], &missing.sources, &sink) };
    assert_eq!(read(&sink, 0, 4), vec![0.0; 4]);
    assert_eq!(read(&sink, 1, 4), vec![15.0, 35.0, 55.0, 75.0]);
    let sink = LaneFrameSink::new(&[AudioLane::Mic, AudioLane::System]);
    // SAFETY: no buffer carries a pointer.
    unsafe { deliver(&[no_channels, no_channels], &layout.sources, &sink) };
    assert_eq!(sink.available_to_read(), 0);
    assert!(sink.dropped_samples().is_empty());
}

/// Rings of four samples: the second callback of four is refused for both
/// lanes and counted, and nothing is half-written.
#[test]
fn a_callback_that_does_not_fit_is_refused_for_every_lane() {
    let layout = StreamLayout::resolve(
        &[AudioLane::Mic, AudioLane::System],
        &[1, 2],
        &[vec![], vec![1]],
        &[2],
        Some(1),
    )
    .unwrap();
    let sink = LaneFrameSink::with_handler(
        &[AudioLane::Mic, AudioLane::System],
        4.0,
        1.0,
        Box::new(|_| {}),
    );
    for _ in 0..2 {
        run_deliver(
            &[
                Buffer {
                    channels: 1,
                    samples: Some(vec![1.0, 2.0, 3.0, 4.0]),
                },
                Buffer {
                    channels: 2,
                    samples: Some(vec![1.0; 8]),
                },
            ],
            4,
            &layout,
            &sink,
        );
    }
    assert_eq!(sink.available_to_read(), 4);
    assert_eq!(
        sink.dropped_samples().into_iter().collect::<Vec<_>>(),
        vec![(AudioLane::Mic, 4), (AudioLane::System, 4)]
    );
    assert!(sink.wake().try_take());
    assert!(!sink.wake().try_take(), "no wake for the refused callback");
}

fn baseline() -> DeviceSnapshot {
    DeviceSnapshot {
        output_uid: Some("AirPods".into()),
        default_output_uid: Some("AirPods".into()),
        input_uid: Some("AirPods".into()),
        output_alive: true,
        input_alive: true,
        sample_rate: 48_000.0,
    }
}

#[test]
fn identical_devices_are_no_change_and_a_moved_default_is_reported() {
    let base = baseline();
    assert_eq!(base.difference(&base), None);
    let mut output = baseline();
    output.output_uid = Some("MacBook Pro Speakers".into());
    assert_eq!(
        output.difference(&base),
        Some(DeviceChangeReason::DefaultOutputChanged)
    );
    let mut input = baseline();
    input.input_uid = Some("MacBook Pro Microphone".into());
    assert_eq!(
        input.difference(&base),
        Some(DeviceChangeReason::DefaultInputChanged)
    );
    output.input_uid = Some("MacBook Pro Microphone".into());
    assert_eq!(
        output.difference(&base),
        Some(DeviceChangeReason::DefaultOutputChanged),
        "the first difference"
    );
    let mut moved = baseline();
    moved.default_output_uid = Some("Headphones".into());
    assert_eq!(
        moved.difference(&base),
        Some(DeviceChangeReason::DefaultOutputChanged)
    );
}

/// The re-check on the fallback sees only another microphone: a bad read
/// of the outputs or the rate is left to their own notifications, and a
/// microphone that did not resolve is no other one.
#[test]
fn the_input_difference_is_another_microphone_alone() {
    let base = baseline();
    let mut other = baseline();
    other.input_uid = Some("USB Microphone".into());
    assert_eq!(
        other.input_difference(&base),
        Some(DeviceChangeReason::DefaultInputChanged)
    );
    let mut misread = baseline();
    misread.output_uid = None;
    misread.default_output_uid = None;
    misread.output_alive = false;
    misread.sample_rate = 0.0;
    assert_eq!(
        misread.difference(&base),
        Some(DeviceChangeReason::OutputDeviceGone)
    );
    assert_eq!(misread.input_difference(&base), None);
    let mut unresolved = baseline();
    unresolved.input_uid = None;
    assert_eq!(
        unresolved.difference(&base),
        Some(DeviceChangeReason::DefaultInputChanged)
    );
    assert_eq!(unresolved.input_difference(&base), None);
}

#[test]
fn a_dead_device_is_reported_before_the_default_that_moved_because_of_it() {
    let base = baseline();
    let mut output = baseline();
    output.output_alive = false;
    output.output_uid = Some("MacBook Pro Speakers".into());
    assert_eq!(
        output.difference(&base),
        Some(DeviceChangeReason::OutputDeviceGone)
    );
    let mut input = baseline();
    input.input_alive = false;
    input.input_uid = None;
    assert_eq!(
        input.difference(&base),
        Some(DeviceChangeReason::InputDeviceGone)
    );
    let mut rate = baseline();
    rate.sample_rate = 44_100.0;
    assert_eq!(
        rate.difference(&base),
        Some(DeviceChangeReason::SampleRateChanged)
    );
    rate.sample_rate = 0.0;
    assert_eq!(
        rate.difference(&base),
        Some(DeviceChangeReason::SampleRateChanged),
        "an unreadable aggregate"
    );
    let tap_only = DeviceSnapshot {
        output_uid: Some("Speakers".into()),
        default_output_uid: Some("Speakers".into()),
        input_uid: None,
        output_alive: true,
        input_alive: false,
        sample_rate: 48_000.0,
    };
    assert_eq!(tap_only.difference(&tap_only), None);
}

#[test]
fn the_nominal_rate_settles_or_reports_the_last_read() {
    let mut reads = 0;
    let mut waits = 0;
    let rate = NominalSampleRate::settle(
        48_000.0,
        NominalSampleRate::ATTEMPTS,
        || {
            reads += 1;
            48_000.0
        },
        || waits += 1,
    );
    assert_eq!((rate, reads, waits), (48_000.0, 1, 0));

    let mut readings = vec![44_100.0, 44_100.0, 48_000.0, 48_000.0];
    let mut waits = 0;
    let rate = NominalSampleRate::settle(
        48_000.0,
        NominalSampleRate::ATTEMPTS,
        || readings.remove(0),
        || waits += 1,
    );
    assert_eq!((rate, waits), (48_000.0, 2));
    assert_eq!(readings, vec![48_000.0], "stops reading once it matches");

    let mut reads = 0;
    let mut waits = 0;
    let rate = NominalSampleRate::settle(
        48_000.0,
        4,
        || {
            reads += 1;
            44_100.0
        },
        || waits += 1,
    );
    assert_eq!((rate, reads, waits), (44_100.0, 4, 3));
    assert_eq!(
        NominalSampleRate::INTERVAL * NominalSampleRate::ATTEMPTS as u32,
        Duration::from_millis(200)
    );
}
