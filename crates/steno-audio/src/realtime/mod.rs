//! The real-time path: the rings the IOProc writes, the processing thread
//! that drains them in 10 ms frames, and the relay to the writer thread.
//! Nothing here allocates or locks once constructed.
//! Swift: `Sources/StenoAudio/RealTime/`.

pub mod io_proc;
pub mod level_meter;
pub mod processing;
pub mod relay;
pub mod ring;
pub mod rings;
pub mod sink;
pub mod wake;

pub use io_proc::{BufferView, deliver, interleaved_view};
pub use level_meter::{LevelMeter, LevelSlot};
pub use processing::{ProcessingConfiguration, ProcessingThread};
pub use relay::FrameRelay;
pub use ring::LaneRingBuffer;
pub use rings::LaneRings;
pub use sink::LaneFrameSink;
pub use wake::Wake;
