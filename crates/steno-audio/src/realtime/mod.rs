//! The real-time path: the rings the IOProc writes, the callback views
//! and `deliver_slices` the live backends share (`io_proc`), the
//! two-stream bodies of the Windows backend (`streams`), the processing
//! thread that drains the rings in 10 ms frames, and the relay to the
//! writer thread.
//! Nothing here allocates or locks once constructed.
//! Swift: `Sources/StenoAudio/RealTime/`.

pub mod io_proc;
pub mod level_meter;
pub mod processing;
pub mod relay;
pub mod ring;
pub mod rings;
pub mod sink;
pub mod streams;
pub mod wake;

pub use io_proc::{
    BufferView, MAX_SLICE_BUFFERS, SliceView, deliver, deliver_slices, interleaved_view,
};
pub use level_meter::{LevelMeter, LevelSlot};
pub use processing::{ProcessingConfiguration, ProcessingThread};
pub use relay::FrameRelay;
pub use ring::LaneRingBuffer;
pub use rings::LaneRings;
pub use sink::LaneFrameSink;
pub use streams::{FollowerLane, PacketRouter, StreamBody};
pub use wake::Wake;
