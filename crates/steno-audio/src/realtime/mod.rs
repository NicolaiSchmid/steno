//! The real-time path: the rings the callbacks write, the processing thread
//! that drains them in 10 ms frames, the relay to the writer thread, and the
//! clocks that count missed audio. Nothing here allocates or locks once
//! constructed, except [`first_callback_line`], formatted off the audio
//! thread.

pub mod first_callback;
pub mod io_proc;
pub mod level_meter;
pub mod missed;
pub mod processing;
pub mod rate_converter;
pub mod relay;
pub mod ring;
pub mod rings;
pub mod sink;
pub mod streams;
pub mod wake;

pub use first_callback::{FirstCallback, first_callback_line};
pub use io_proc::{
    BufferView, MAX_SLICE_BUFFERS, SliceView, deliver, deliver_slices, interleaved_view,
};
#[cfg(target_os = "macos")]
pub use io_proc::{MAX_BUFFERS, silence_output};
pub use level_meter::{LevelMeter, LevelSlot};
pub use missed::{CycleClock, MISSED_FILL_LIMIT, Missed, Ratio};
pub use processing::{ProcessingConfiguration, ProcessingThread};
pub use rate_converter::RateConverter;
pub use relay::FrameRelay;
pub use ring::LaneRingBuffer;
pub use rings::LaneRings;
pub use sink::LaneFrameSink;
pub use streams::{FollowerLane, PacketRouter, StreamBody, count_at_stop};
pub use wake::Wake;
