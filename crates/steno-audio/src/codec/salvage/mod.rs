//! The salvage of a phone recording without a `moov`: [`frame`] reads an
//! AAC-LC frame's length off its bits, for a stream with no sample table.

pub mod frame;
mod tables;

pub use frame::{FrameError, MAX_FRAME_BYTES, frame_length};
