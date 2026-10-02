//! The file system seam: atomic writes and one folder addressed by relative
//! paths.

mod atomic;
mod sink;

pub use atomic::{AtomicFileWriter, WriteFailure};
pub use sink::LocalFolderSink;
