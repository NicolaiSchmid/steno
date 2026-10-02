//! Where each lane's samples sit in the aggregate's input `AudioBufferList`,
//! resolved once at start. Port of `Sources/StenoAudio/Capture/StreamLayout.swift`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    Mic,
    System,
}

impl Lane {
    pub fn name(self) -> &'static str {
        match self {
            Lane::Mic => "mic",
            Lane::System => "system",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelRef {
    pub buffer: usize,
    pub offset: usize,
    pub stride: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaneSource {
    pub lane: Lane,
    pub left: ChannelRef,
    pub right: Option<ChannelRef>,
}

#[derive(Clone, Debug)]
pub struct StreamLayout {
    pub sources: Vec<LaneSource>,
    pub tap_first: bool,
}

impl StreamLayout {
    /// `aggregate`: channel count per input buffer of the aggregate.
    /// `sub_devices`: the same per sub-device in aggregate order.
    /// `tap`: the tap's buffers (`[2]` interleaved, `[1, 1]` non-interleaved).
    pub fn resolve(
        lanes: &[Lane],
        aggregate: &[usize],
        sub_devices: &[Vec<usize>],
        tap: &[usize],
        mic_sub_device: Option<usize>,
    ) -> Result<Self, String> {
        let flat: Vec<usize> = sub_devices.iter().flatten().copied().collect();
        let mut sub_then_tap = flat.clone();
        sub_then_tap.extend_from_slice(tap);
        let mut tap_then_sub = tap.to_vec();
        tap_then_sub.extend_from_slice(&flat);
        let tap_first = if aggregate == sub_then_tap.as_slice() {
            false
        } else if aggregate == tap_then_sub.as_slice() {
            true
        } else {
            return Err(format!(
                "unexpected stream layout: aggregate buffers {aggregate:?}, sub-devices {sub_devices:?}, tap {tap:?}"
            ));
        };
        let tap_offset = if tap_first { 0 } else { flat.len() };
        let sub_offset = if tap_first { tap.len() } else { 0 };
        let mut sources = Vec::new();
        for &lane in lanes {
            match lane {
                Lane::Mic => {
                    let index = mic_sub_device.ok_or("no microphone sub-device")?;
                    let first = *sub_devices
                        .get(index)
                        .and_then(|b| b.first())
                        .ok_or("microphone sub-device has no input buffers")?;
                    let start =
                        sub_offset + sub_devices[..index].iter().map(|b| b.len()).sum::<usize>();
                    sources.push(LaneSource {
                        lane,
                        left: ChannelRef { buffer: start, offset: 0, stride: first },
                        right: None,
                    });
                }
                Lane::System => {
                    if tap.is_empty() {
                        return Err("no tap buffers for the system lane".into());
                    }
                    let left = ChannelRef { buffer: tap_offset, offset: 0, stride: tap[0] };
                    let right = if tap[0] >= 2 {
                        Some(ChannelRef { buffer: tap_offset, offset: 1, stride: tap[0] })
                    } else if tap.len() >= 2 {
                        Some(ChannelRef { buffer: tap_offset + 1, offset: 0, stride: 1 })
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
