//! The IOProc body, separated from the HAL so the pointer arithmetic over
//! `StreamLayout` is tested on every OS with hand-built buffer lists. The
//! macOS backend calls [`deliver`] with [`BufferView`]s on the stack; the
//! WASAPI and PipeWire backends call the safe [`deliver_slices`]. Nothing
//! allocates, locks or logs inside the callback.

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
/// missing, has no data, or is not shaped as the layout expects (channel
/// inside the buffer's count, stride equal to it, `frames` whole frames).
/// A buffer list reshaped since start becomes silence, not a read past its
/// end.
#[inline(always)]
fn samples(channel: &ChannelRef, buffers: &[BufferView], frames: usize) -> Option<*const f32> {
    let buffer = buffers.get(channel.buffer)?;
    let data = buffer.data?;
    if channel.offset >= buffer.channels
        || channel.stride != buffer.channels
        || buffer.byte_size < frames * buffer.channels * size_of::<f32>()
    {
        return None;
    }
    // SAFETY: `offset` is below the buffer's channel count (checked above),
    // so the pointer stays inside the first frame.
    Some(unsafe { data.add(channel.offset) })
}

/// The callback's frame count, from the first source whose buffer carries
/// any; 0 when none does.
#[inline(always)]
pub(crate) fn callback_frames(buffers: &[BufferView], sources: &[LaneSource]) -> usize {
    for source in sources {
        if let Some(buffer) = buffers.get(source.left.buffer)
            && buffer.channels > 0
        {
            let frames = buffer.byte_size / (buffer.channels * size_of::<f32>());
            if frames > 0 {
                return frames;
            }
        }
    }
    0
}

/// One callback's input buffers into the sink's rings following `sources`.
/// A missing or misshaped buffer becomes silence so the lanes stay aligned;
/// a callback with no frames at all is skipped. Returns whether it was
/// written (`false` when skipped or refused by full rings).
///
/// # Safety
///
/// Every `data` pointer in `buffers` must be valid for `byte_size` bytes of
/// `f32` for the duration of the call, as the HAL guarantees inside the
/// IOProc and the tests guarantee with owned vectors.
#[inline(always)]
pub unsafe fn deliver(
    buffers: &[BufferView],
    sources: &[LaneSource],
    sink: &LaneFrameSink,
) -> bool {
    let frames = callback_frames(buffers, sources);
    if frames == 0 || !sink.begin_callback(frames) {
        return false;
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
    true
}

/// The most buffers of one list an IOProc of the macOS backend reads or
/// writes; room for a many-channel interface.
#[cfg(target_os = "macos")]
pub const MAX_BUFFERS: usize = 16;

/// The body of the call capture's silent output IOProc: zero-fills the
/// first [`MAX_BUFFERS`] output buffers, skipping a buffer without data.
/// The HAL already zeroes them; writing zeros again keeps the promise of
/// digital silence from resting on the HAL.
///
/// # Safety
///
/// `list` must point at an `AudioBufferList` whose `mNumberBuffers`
/// buffers are laid out after it, each `mData` either null or valid for
/// writes of `mDataByteSize` bytes for the duration of the call, as the HAL
/// guarantees for an IOProc's output list and the tests with owned memory.
#[cfg(target_os = "macos")]
#[inline(always)]
pub unsafe fn silence_output(list: std::ptr::NonNull<objc2_core_audio_types::AudioBufferList>) {
    let list = list.as_ptr();
    // SAFETY: `list` is valid for reads by the caller's guarantee, and its
    // buffers follow it (`mBuffers` is declared with one element; the
    // pointer keeps the provenance of the whole list).
    unsafe {
        let count = ((*list).mNumberBuffers as usize).min(MAX_BUFFERS);
        let buffers = (&raw mut (*list).mBuffers).cast::<objc2_core_audio_types::AudioBuffer>();
        for index in 0..count {
            let buffer = &*buffers.add(index);
            if !buffer.mData.is_null() && buffer.mDataByteSize > 0 {
                // SAFETY: a non-null `mData` is valid for writes of
                // `mDataByteSize` bytes by the caller's guarantee; zero
                // bytes are digital silence in the float and signed
                // integer formats an IOProc is handed.
                std::ptr::write_bytes(buffer.mData.cast::<u8>(), 0, buffer.mDataByteSize as usize);
            }
        }
    }
}

/// One input buffer as safe code holds it (a WASAPI packet, the follower's
/// staging copy, or a dequeued PipeWire buffer), its interleaved samples
/// borrowed for the call.
#[derive(Debug, Clone, Copy)]
pub struct SliceView<'a> {
    pub channels: usize,
    pub frames: usize,
    /// `channels * frames` interleaved samples, or `None` for silence.
    pub samples: Option<&'a [f32]>,
}

/// The most buffers [`deliver_slices`] takes: the master packet and the
/// follower's staging copy, with room to spare.
pub const MAX_SLICE_BUFFERS: usize = 4;

/// [`deliver`] for buffers held as slices, so the caller needs no
/// `unsafe`. A slice shorter than `channels * frames` is treated as silent
/// (it gets no pointer). Buffers beyond [`MAX_SLICE_BUFFERS`] are ignored.
#[inline(always)]
pub fn deliver_slices(
    buffers: &[SliceView<'_>],
    sources: &[LaneSource],
    sink: &LaneFrameSink,
) -> bool {
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
            byte_size: wanted * size_of::<f32>(),
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
/// is the mapped `spa_data`, `offset`, `size` and `stride` its `spa_chunk`.
/// The offset is taken modulo the memory size as SPA defines it, a chunk
/// past the end is cut, and a partial last frame dropped. Missing,
/// misaligned or oddly strided memory becomes a view without samples
/// (silence); a stride of 0 means `channels` samples.
#[inline(always)]
#[must_use]
pub fn interleaved_view(
    memory: Option<&[u8]>,
    offset: u32,
    size: u32,
    stride: i32,
    channels: usize,
) -> SliceView<'_> {
    let without_samples = |frames| SliceView {
        channels,
        frames,
        samples: None,
    };
    let frame_bytes = channels * size_of::<f32>();
    if frame_bytes == 0 {
        return without_samples(0);
    }
    let size = size as usize;
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
