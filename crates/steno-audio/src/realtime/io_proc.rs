//! The IOProc body, separated from the HAL so the pointer arithmetic over
//! `StreamLayout` is tested on every OS with buffer lists built by hand.
//! Swift: `Sources/StenoAudio/RealTime/IOProcRunner.swift` (`deliver`).
//!
//! The macOS backend (`capture::live`) turns the HAL's `AudioBufferList`
//! into a slice of [`BufferView`]s on the stack and calls [`deliver`]. No
//! arrays are created, no closures called, nothing logged inside the
//! callback. The WASAPI capture threads hold their packets as slices and
//! go through [`deliver_slices`], the safe form, via
//! [`PacketRouter`](super::PacketRouter).

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
/// from `GetBuffer` to `ReleaseBuffer`) or the follower's staging copy, its
/// interleaved samples borrowed for the call, `None` for a buffer flagged
/// silent (`AUDCLNT_BUFFERFLAGS_SILENT`), which becomes zeros.
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
