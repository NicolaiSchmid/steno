//! The IOProc body, separated from the HAL so the pointer arithmetic over
//! `StreamLayout` is tested on every OS with buffer lists built by hand.
//! Swift: `Sources/StenoAudio/RealTime/IOProcRunner.swift` (`deliver`).
//!
//! The macOS backend (`capture::live`) turns the HAL's `AudioBufferList`
//! into a slice of [`BufferView`]s on the stack and calls [`deliver`]. No
//! arrays are created, no closures called, nothing logged inside the
//! callback.

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
