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
    pub channels: usize,
    /// Interleaved samples; `channels * frames` floats.
    pub data: Option<*const f32>,
    /// Bytes in the buffer, as the HAL reports them.
    pub byte_size: usize,
}

/// The first sample of `channel` in `buffers`, `None` when the buffer is
/// missing or has no data.
#[inline(always)]
fn samples(channel: &ChannelRef, buffers: &[BufferView]) -> Option<*const f32> {
    let buffer = buffers.get(channel.buffer)?;
    let data = buffer.data?;
    // SAFETY: `offset` is below the buffer's channel count by construction
    // of the layout, so the pointer stays inside the first frame.
    Some(unsafe { data.add(channel.offset) })
}

/// One callback's input buffers into the sink's rings following `sources`.
/// The frame count comes from the first source's buffer; a buffer the HAL
/// delivered without data becomes silence so the lanes stay aligned.
///
/// # Safety
///
/// Every `data` pointer in `buffers` must be valid for `byte_size` bytes of
/// `f32` for the duration of the call, as the HAL guarantees inside the
/// IOProc and the tests guarantee with owned vectors.
#[inline(always)]
pub unsafe fn deliver(buffers: &[BufferView], sources: &[LaneSource], sink: &LaneFrameSink) {
    let Some(first_source) = sources.first() else {
        return;
    };
    let Some(first) = buffers.get(first_source.left.buffer) else {
        return;
    };
    if first.channels == 0 {
        return;
    }
    let frames = first.byte_size / (first.channels * 4);
    if frames == 0 || !sink.begin_callback(frames) {
        return;
    }
    for (lane, source) in sources.iter().enumerate() {
        match samples(&source.left, buffers) {
            Some(left) => {
                let right = source
                    .right
                    .and_then(|r| samples(&r, buffers).map(|p| (p, r.stride)));
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
