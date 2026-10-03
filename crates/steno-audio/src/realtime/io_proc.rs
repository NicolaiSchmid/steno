//! The IOProc body, separated from the HAL so the pointer arithmetic over
//! `StreamLayout` is tested on every OS with buffer lists built by hand.
//! Swift: `Sources/StenoAudio/RealTime/IOProcRunner.swift` (`deliver`).
//!
//! The macOS backend (`capture::live`) turns the HAL's `AudioBufferList`
//! into a slice of [`BufferView`]s on the stack and calls [`deliver`]. No
//! arrays are created, no closures called, nothing logged inside the
//! callback. The PipeWire backend turns its one interleaved buffer into a
//! view with [`interleaved_view`] and calls [`deliver`] the same way.

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
                    None => unsafe { sink.write(lane, left, source.left.stride) },
                }
            }
            None => sink.write_silence(lane),
        }
    }
    sink.end_callback();
}

/// One PipeWire buffer of interleaved `f32` as a [`BufferView`]: `memory`
/// is the mapped `spa_data` (`maxsize` bytes), `offset`, `size` and
/// `stride` its `spa_chunk`, `channels` the format's channel count. The
/// chunk's offset is taken modulo the memory size as SPA defines it, and a
/// chunk running past the end is cut to what the memory holds (audio chunks
/// do not wrap). Memory that is missing, misaligned for `f32`, or strided
/// unlike `channels` interleaved samples becomes a view without data, so
/// [`deliver`] writes silence of the chunk's length instead of reading past
/// the buffer. A stride of 0 (a producer that leaves it unset) is read as
/// `channels` samples. A handful of integer operations, no allocation; the
/// view borrows `memory`'s pointer, valid as long as the buffer stays
/// dequeued.
#[inline(always)]
#[must_use]
pub fn interleaved_view(
    memory: Option<&[u8]>,
    offset: u32,
    size: u32,
    stride: i32,
    channels: usize,
) -> BufferView {
    let frame_bytes = channels * 4;
    let size = size as usize;
    let without_data = |byte_size| BufferView {
        channels,
        data: None,
        byte_size,
    };
    if frame_bytes == 0 {
        return without_data(0);
    }
    let Some(memory) = memory.filter(|m| !m.is_empty()) else {
        return without_data(size - size % frame_bytes);
    };
    let start = offset as usize % memory.len();
    let size = size.min(memory.len() - start);
    let size = size - size % frame_bytes;
    let stride = usize::try_from(stride).unwrap_or(usize::MAX);
    let first = memory[start..].as_ptr();
    if (stride != 0 && stride != frame_bytes) || first.addr() % align_of::<f32>() != 0 {
        return without_data(size);
    }
    // The alignment is checked just above.
    #[allow(clippy::cast_ptr_alignment)]
    let data = first.cast::<f32>();
    BufferView {
        channels,
        data: Some(data),
        byte_size: size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `samples` as the bytes PipeWire maps, 4-byte aligned.
    fn bytes(samples: &[f32]) -> &[u8] {
        // SAFETY: any initialised `f32` slice is valid as bytes; the length
        // is the slice's in bytes.
        unsafe { std::slice::from_raw_parts(samples.as_ptr().cast::<u8>(), samples.len() * 4) }
    }

    #[test]
    fn a_whole_chunk_views_from_its_offset() {
        let samples = [0.0f32, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let view = interleaved_view(Some(bytes(&samples)), 12, 24, 12, 3);
        assert_eq!(view.channels, 3);
        assert_eq!(view.byte_size, 24);
        // SAFETY: the view points into `samples`.
        assert_eq!(unsafe { *view.data.unwrap() }, 3.0);
    }

    #[test]
    fn the_offset_wraps_and_an_overlong_chunk_is_cut() {
        let samples = [0.0f32; 6];
        let memory = bytes(&samples);
        // 36 wraps to 12 in 24 bytes of memory; the 12 bytes left there
        // hold one whole stereo frame of 8.
        let view = interleaved_view(Some(memory), 36, 400, 8, 2);
        assert_eq!(view.byte_size, 8);
        assert_eq!(
            view.data.map(<*const f32>::cast::<u8>),
            Some(memory[12..].as_ptr())
        );
    }

    #[test]
    fn a_partial_frame_is_dropped_and_a_zero_stride_is_accepted() {
        let samples = [0.0f32; 8];
        let view = interleaved_view(Some(bytes(&samples)), 0, 30, 0, 2);
        assert_eq!(view.byte_size, 24);
        assert!(view.data.is_some());
    }

    #[test]
    fn a_foreign_stride_or_misalignment_becomes_silence_of_the_same_length() {
        let samples = [0.0f32; 8];
        let memory = bytes(&samples);
        let strided = interleaved_view(Some(memory), 0, 32, 16, 2);
        assert_eq!((strided.data, strided.byte_size), (None, 32));
        let misaligned = interleaved_view(Some(memory), 2, 16, 8, 2);
        assert_eq!((misaligned.data, misaligned.byte_size), (None, 16));
    }

    #[test]
    fn missing_memory_or_channels_carry_no_data() {
        let none = interleaved_view(None, 0, 48, 12, 3);
        assert_eq!((none.data, none.byte_size), (None, 48));
        let empty = interleaved_view(Some(&[]), 0, 48, 12, 3);
        assert_eq!((empty.data, empty.byte_size), (None, 48));
        let no_channels = interleaved_view(Some(bytes(&[0.0; 4])), 0, 16, 0, 0);
        assert_eq!((no_channels.data, no_channels.byte_size), (None, 0));
    }
}
