//! Where each lane's samples sit in the aggregate's input `AudioBufferList`.
//! Swift: `Sources/StenoAudio/Capture/StreamLayout.swift`.
//!
//! Resolved once at start from the channel count of every buffer, so the
//! IOProc only follows precomputed indices. Pure, so the two possible HAL
//! orderings (sub-devices then taps, or taps first) are tested on CI.

use steno_core::AudioLane;

use super::configuration::CaptureError;

/// One channel in the buffer list: which buffer, the offset of the first
/// sample in it, and the buffer's channel count as the sample stride (1
/// for a non-interleaved channel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelRef {
    pub buffer: usize,
    pub offset: usize,
    pub stride: usize,
}

impl ChannelRef {
    #[must_use]
    pub const fn new(buffer: usize, offset: usize, stride: usize) -> Self {
        Self {
            buffer,
            offset,
            stride,
        }
    }
}

/// One lane's source: its channel, plus a second channel when a stereo tap
/// is folded to the mono system lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneSource {
    pub lane: AudioLane,
    pub left: ChannelRef,
    pub right: Option<ChannelRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamLayout {
    pub sources: Vec<LaneSource>,
    /// True when the HAL placed the tap's streams before the sub-devices'.
    pub tap_first: bool,
}

impl StreamLayout {
    /// `aggregate`: channel count per input buffer of the aggregate device.
    /// `sub_devices`: the same per sub-device, in sub-device order (the
    /// main output device first, then the microphone when it is a different
    /// device). `tap`: the tap's buffers (`[2]` interleaved stereo, `[1, 1]`
    /// non-interleaved, `[]` when there is no tap). `mic_sub_device` is the
    /// index into `sub_devices` whose first channel is the microphone,
    /// `None` when no lane needs it.
    pub fn resolve(
        lanes: &[AudioLane],
        aggregate: &[usize],
        sub_devices: &[Vec<usize>],
        tap: &[usize],
        mic_sub_device: Option<usize>,
    ) -> Result<Self, CaptureError> {
        let flat: Vec<usize> = sub_devices.iter().flatten().copied().collect();
        let sub_then_tap: Vec<usize> = flat.iter().chain(tap).copied().collect();
        let tap_then_sub: Vec<usize> = tap.iter().chain(&flat).copied().collect();
        let tap_first = if aggregate == sub_then_tap.as_slice() {
            false
        } else if aggregate == tap_then_sub.as_slice() {
            true
        } else {
            return Err(CaptureError::UnexpectedStreamLayout(format!(
                "aggregate buffers {aggregate:?}, sub-devices {sub_devices:?}, tap {tap:?}"
            )));
        };
        let tap_offset = if tap_first { 0 } else { flat.len() };
        let sub_device_offset = if tap_first { tap.len() } else { 0 };

        let mut sources = Vec::with_capacity(lanes.len());
        for &lane in lanes {
            match lane {
                AudioLane::Mic | AudioLane::Mixed => {
                    let (index, first) = mic_sub_device
                        .and_then(|index| Some((index, *sub_devices.get(index)?.first()?)))
                        .ok_or_else(|| {
                            CaptureError::UnexpectedStreamLayout(format!(
                                "no microphone sub-device for {}",
                                lane.as_str()
                            ))
                        })?;
                    let start = sub_device_offset
                        + sub_devices[..index].iter().map(Vec::len).sum::<usize>();
                    sources.push(LaneSource {
                        lane,
                        left: ChannelRef::new(start, 0, first),
                        right: None,
                    });
                }
                AudioLane::System => {
                    let Some(&first) = tap.first() else {
                        return Err(CaptureError::UnexpectedStreamLayout(
                            "no tap buffers for the system lane".into(),
                        ));
                    };
                    let left = ChannelRef::new(tap_offset, 0, first);
                    let right = if first >= 2 {
                        // Interleaved stereo: the right channel is the next sample.
                        Some(ChannelRef::new(tap_offset, 1, first))
                    } else if tap.len() >= 2 {
                        // Non-interleaved: the right channel is the next buffer.
                        Some(ChannelRef::new(tap_offset + 1, 0, 1))
                    } else {
                        None
                    };
                    sources.push(LaneSource { lane, left, right });
                }
            }
        }
        Ok(Self { sources, tap_first })
    }
}
