//! The IOProc body, separated from the HAL so the pointer arithmetic over
//! `StreamLayout` is tested on every OS with buffer lists built by hand.
//! Swift: `Sources/StenoAudio/RealTime/IOProcRunner.swift` (`deliver`).
//!
//! The macOS backend (`capture::live`) turns the HAL's `AudioBufferList`
//! into a slice of [`BufferView`]s on the stack and calls [`deliver`]. No
//! arrays are created, no closures called, nothing logged inside the
//! callback. Safe code holds its buffers as [`SliceView`]s and goes
//! through [`deliver_slices`], the safe form: the WASAPI capture threads
//! via [`PacketRouter`](super::PacketRouter), and the PipeWire backend
//! after [`interleaved_view`] has turned its one mapped buffer into a view.

use super::sink::LaneFrameSink;
use crate::capture::layout::{ChannelRef, LaneSource};

/// One input buffer of a callback: its channel count and the first sample,
/// `None` for a buffer the HAL delivered without data.
#[derive(Debug, Clone, Copy)]
pub struct BufferView {
    /// Interleaved channels in the buffer.
    pub channels: usize,
    /// Interleaved samples; `channels * frames` floats.
    pub data: Option<*const f32>,
    /// Bytes in the buffer, as the HAL reports them.
    pub byte_size: usize,
}

/// The first sample of `channel` in `buffers`, `None` when the buffer is
/// missing, has no data, or is not shaped as the layout expects: the
/// channel must sit inside the buffer's channel count, the stride must be
/// that count, and the buffer must hold `frames` whole frames. The layout
/// was resolved from the shapes the HAL reported at start; a buffer list
/// shaped differently (an input that was reconfigured underneath the
/// aggregate) becomes silence instead of a read past its end. Three
/// integer compares, nothing else.
#[inline(always)]
fn samples(channel: &ChannelRef, buffers: &[BufferView], frames: usize) -> Option<*const f32> {
    let buffer = buffers.get(channel.buffer)?;
    let data = buffer.data?;
    if channel.offset >= buffer.channels
        || channel.stride != buffer.channels
        || buffer.byte_size < frames * buffer.channels * 4
    {
        return None;
    }
    // SAFETY: `offset` is below the buffer's channel count (checked above),
    // so the pointer stays inside the first frame.
    Some(unsafe { data.add(channel.offset) })
}

/// The callback's frame count: from the first source whose buffer carries
/// any, so a first lane whose buffer arrived missing or with no channels
/// becomes silence instead of costing the other lanes their audio. 0 when
/// no source's buffer carries a frame.
#[inline(always)]
fn callback_frames(buffers: &[BufferView], sources: &[LaneSource]) -> usize {
    for source in sources {
        if let Some(buffer) = buffers.get(source.left.buffer)
            && buffer.channels > 0
        {
            let frames = buffer.byte_size / (buffer.channels * 4);
            if frames > 0 {
                return frames;
            }
        }
    }
    0
}

/// One callback's input buffers into the sink's rings following `sources`.
/// The frame count comes from the first source whose buffer carries frames;
/// a buffer the HAL delivered without data, or shaped unlike the layout the
/// pointers were resolved for, becomes silence so the lanes stay aligned. A
/// callback where no source's buffer carries a frame has no length to write
/// or to count as a drop, and is skipped.
///
/// # Safety
///
/// Every `data` pointer in `buffers` must be valid for `byte_size` bytes of
/// `f32` for the duration of the call, as the HAL guarantees inside the
/// IOProc and the tests guarantee with owned vectors.
#[inline(always)]
pub unsafe fn deliver(buffers: &[BufferView], sources: &[LaneSource], sink: &LaneFrameSink) {
    let frames = callback_frames(buffers, sources);
    if frames == 0 || !sink.begin_callback(frames) {
        return;
    }
    for (lane, source) in sources.iter().enumerate() {
        match samples(&source.left, buffers, frames) {
            Some(left) => {
                let right = source
                    .right
                    .and_then(|r| samples(&r, buffers, frames).map(|p| (p, r.stride)));
                match right {
                    // SAFETY: the pointers come from `buffers`, valid for
                    // `frames` at their strides by the caller's guarantee.
                    Some((right, right_stride)) => unsafe {
                        sink.write_mixed(lane, left, right, source.left.stride, right_stride);
                    },
                    // SAFETY: as above, for the one pointer.
                    None => unsafe { sink.write(lane, left, source.left.stride) },
                }
            }
            None => sink.write_silence(lane),
        }
    }
    sink.end_callback();
}

/// One input buffer as safe code holds it: a WASAPI capture packet (valid
/// from `GetBuffer` to `ReleaseBuffer`), the follower's staging copy, or a
/// dequeued PipeWire buffer ([`interleaved_view`]), its interleaved samples
/// borrowed for the call. `None` for a buffer flagged silent
/// (`AUDCLNT_BUFFERFLAGS_SILENT`) or memory that cannot be read as
/// interleaved `f32`, which becomes zeros.
#[derive(Debug, Clone, Copy)]
pub struct SliceView<'a> {
    /// Interleaved channels in the buffer.
    pub channels: usize,
    /// Frames in the buffer.
    pub frames: usize,
    /// `channels * frames` interleaved samples, or `None` for silence.
    pub samples: Option<&'a [f32]>,
}

/// The most buffers [`deliver_slices`] takes: the master packet and the
/// follower's staging copy, with room to spare.
pub const MAX_SLICE_BUFFERS: usize = 4;

/// [`deliver`] for buffers held as slices: builds the [`BufferView`]s on
/// the stack and delivers them, so the caller needs no `unsafe`. A slice
/// shorter than `channels * frames` is treated as silent: its buffer keeps
/// the length it claims and becomes zeros, so the lanes stay aligned and
/// nothing reads past its end. Buffers beyond [`MAX_SLICE_BUFFERS`] are
/// ignored.
#[inline(always)]
pub fn deliver_slices(buffers: &[SliceView<'_>], sources: &[LaneSource], sink: &LaneFrameSink) {
    let count = buffers.len().min(MAX_SLICE_BUFFERS);
    let mut views = [BufferView {
        channels: 0,
        data: None,
        byte_size: 0,
    }; MAX_SLICE_BUFFERS];
    for (view, buffer) in views.iter_mut().zip(&buffers[..count]) {
        let wanted = buffer.frames * buffer.channels;
        *view = BufferView {
            channels: buffer.channels,
            data: buffer
                .samples
                .filter(|samples| samples.len() >= wanted)
                .map(<[f32]>::as_ptr),
            byte_size: wanted * 4,
        };
    }
    // SAFETY: every `data` pointer is the start of a slice borrowed for
    // this call and holding at least `byte_size` bytes (shorter slices get
    // no pointer above); `deliver` reads a buffer only within the
    // callback's frames at the buffer's channel count, which its shape
    // check keeps inside `byte_size`.
    unsafe { deliver(&views[..count], sources, sink) }
}

/// One PipeWire buffer of interleaved `f32` as a [`SliceView`]: `memory`
/// is the mapped `spa_data` (`maxsize` bytes), `offset`, `size` and
/// `stride` its `spa_chunk`, `channels` the format's channel count. The
/// chunk's offset is taken modulo the memory size as SPA defines it, a
/// chunk running past the end is cut to what the memory holds (audio chunks
/// do not wrap), and a partial last frame is dropped. Memory that is
/// missing, misaligned for `f32`, or strided unlike `channels` interleaved
/// samples becomes a view without samples, so [`deliver_slices`] writes
/// silence of the chunk's length instead of reading past the buffer. A
/// stride of 0 (a producer that leaves it unset) is read as `channels`
/// samples. A handful of integer operations, no allocation; the view
/// borrows `memory`, valid as long as the buffer stays dequeued.
#[inline(always)]
#[must_use]
pub fn interleaved_view(
    memory: Option<&[u8]>,
    offset: u32,
    size: u32,
    stride: i32,
    channels: usize,
) -> SliceView<'_> {
    let frame_bytes = channels * size_of::<f32>();
    if frame_bytes == 0 {
        return SliceView {
            channels,
            frames: 0,
            samples: None,
        };
    }
    let size = size as usize;
    let without_samples = |frames| SliceView {
        channels,
        frames,
        samples: None,
    };
    let Some(memory) = memory.filter(|m| !m.is_empty()) else {
        return without_samples(size / frame_bytes);
    };
    let start = offset as usize % memory.len();
    let frames = size.min(memory.len() - start) / frame_bytes;
    let bytes = &memory[start..start + frames * frame_bytes];
    let stride = usize::try_from(stride).unwrap_or(usize::MAX);
    if (stride != 0 && stride != frame_bytes) || bytes.as_ptr().addr() % align_of::<f32>() != 0 {
        return without_samples(frames);
    }
    // The alignment is checked just above.
    #[allow(clippy::cast_ptr_alignment)]
    let first = bytes.as_ptr().cast::<f32>();
    // SAFETY: `bytes` is aligned for `f32` (checked above) and holds
    // exactly `frames * channels` of them (`frames * frame_bytes` bytes);
    // every bit pattern is a valid `f32`, and the slice borrows `memory`
    // for the lifetime it returns with.
    let samples = unsafe { std::slice::from_raw_parts(first, frames * channels) };
    SliceView {
        channels,
        frames,
        samples: Some(samples),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `samples` as the bytes PipeWire maps, 4-byte aligned.
    fn bytes(samples: &[f32]) -> &[u8] {
        // SAFETY: any initialised `f32` slice is valid as bytes; the length
        // is the slice's in bytes.
        unsafe { std::slice::from_raw_parts(samples.as_ptr().cast::<u8>(), size_of_val(samples)) }
    }

    #[test]
    fn a_view_delivers_whole_frames_in_channel_order_within_the_memory() {
        use steno_core::AudioLane;

        // Four frames of three channels, sample `10 * frame + channel`.
        let samples: Vec<f32> = (0..12u8).map(|i| f32::from(i / 3 * 10 + i % 3)).collect();
        let memory = bytes(&samples);
        // From the second frame, a chunk far longer than the memory.
        let view = interleaved_view(Some(memory), 12, 4_096, 12, 3);
        assert_eq!(view.frames, 3, "three whole frames are left");
        let lanes = [AudioLane::Mic, AudioLane::System];
        let sources = [
            LaneSource {
                lane: AudioLane::Mic,
                left: ChannelRef::new(0, 0, 3),
                right: None,
            },
            LaneSource {
                lane: AudioLane::System,
                left: ChannelRef::new(0, 2, 3),
                right: None,
            },
        ];
        let sink = LaneFrameSink::new(&lanes);
        deliver_slices(&[view], &sources, &sink);
        assert_eq!(sink.available_to_read(), 3);
        let (mut mic, mut system) = ([0.0f32; 3], [0.0f32; 3]);
        assert!(sink.ring(0).read(&mut mic) && sink.ring(1).read(&mut system));
        assert_eq!(mic, [10.0, 20.0, 30.0], "channel 0 of frames 1 to 3");
        assert_eq!(system, [12.0, 22.0, 32.0], "channel 2 of frames 1 to 3");
    }

    #[test]
    fn a_whole_chunk_views_from_its_offset() {
        let samples = [0.0f32, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let view = interleaved_view(Some(bytes(&samples)), 12, 24, 12, 3);
        assert_eq!(view.channels, 3);
        assert_eq!(view.frames, 2);
        assert_eq!(view.samples, Some(&samples[3..]));
    }

    #[test]
    fn the_offset_wraps_and_an_overlong_chunk_is_cut() {
        let samples = [0.0f32; 6];
        let memory = bytes(&samples);
        // 36 wraps to 12 in 24 bytes of memory; the 12 bytes left there
        // hold one whole stereo frame of 8.
        let view = interleaved_view(Some(memory), 36, 400, 8, 2);
        assert_eq!(view.frames, 1);
        assert_eq!(
            view.samples.map(|samples| samples.as_ptr().cast::<u8>()),
            Some(memory[12..].as_ptr())
        );
    }

    #[test]
    fn a_partial_frame_is_dropped_and_a_zero_stride_is_accepted() {
        let samples = [0.0f32; 8];
        let view = interleaved_view(Some(bytes(&samples)), 0, 30, 0, 2);
        assert_eq!(view.frames, 3);
        assert_eq!(view.samples.map(<[f32]>::len), Some(6));
    }

    #[test]
    fn a_foreign_stride_or_misalignment_becomes_silence_of_the_same_length() {
        let samples = [0.0f32; 8];
        let memory = bytes(&samples);
        let strided = interleaved_view(Some(memory), 0, 32, 16, 2);
        assert_eq!((strided.samples, strided.frames), (None, 4));
        let misaligned = interleaved_view(Some(memory), 2, 16, 8, 2);
        assert_eq!((misaligned.samples, misaligned.frames), (None, 2));
    }

    #[test]
    fn missing_memory_or_channels_carry_no_data() {
        let none = interleaved_view(None, 0, 48, 12, 3);
        assert_eq!((none.samples, none.frames), (None, 4));
        let empty = interleaved_view(Some(&[]), 0, 48, 12, 3);
        assert_eq!((empty.samples, empty.frames), (None, 4));
        let no_channels = interleaved_view(Some(bytes(&[0.0; 4])), 0, 16, 0, 0);
        assert_eq!((no_channels.samples, no_channels.frames), (None, 0));
    }
}
