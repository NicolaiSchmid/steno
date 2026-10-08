//! Random access to a file's bytes, so the CAF and WAV readers walk their
//! chunk headers the same way over a slice in memory and over a file on
//! disk, and the streaming readers fetch their samples a block at a time.
//! No Swift counterpart (the Swift readers load the file whole).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Bytes read by offset; see the module doc. A file's errors name it.
pub(crate) trait ReadAt {
    /// The bytes there are.
    fn len(&self) -> usize;

    /// Fills `buffer` from `offset`; `offset + buffer.len()` is at most
    /// `len()`.
    fn read_at(&mut self, offset: usize, buffer: &mut [u8]) -> std::io::Result<()>;
}

impl ReadAt for &[u8] {
    fn len(&self) -> usize {
        <[u8]>::len(self)
    }

    fn read_at(&mut self, offset: usize, buffer: &mut [u8]) -> std::io::Result<()> {
        buffer.copy_from_slice(&self[offset..offset + buffer.len()]);
        Ok(())
    }
}

/// A file read through a small window, so a walk over many small chunk
/// headers costs a read per window rather than one per header; a read of
/// a window or more goes to the file directly.
pub(crate) struct WindowedFile {
    file: File,
    /// Named in every error.
    path: PathBuf,
    len: usize,
    window: Vec<u8>,
    /// The file offset of `window[0]`.
    window_start: usize,
}

impl WindowedFile {
    /// Bytes per window read.
    const WINDOW: usize = 64 * 1024;

    pub(crate) fn open(path: &Path) -> std::io::Result<Self> {
        let named = |error| named(path, &error);
        let file = File::open(path).map_err(named)?;
        // A file longer than the address space cannot be a recording.
        let len = usize::try_from(file.metadata().map_err(named)?.len()).unwrap_or(usize::MAX);
        Ok(Self {
            file,
            path: path.to_path_buf(),
            len,
            window: Vec::new(),
            window_start: 0,
        })
    }

    fn read_window(&mut self, offset: usize, buffer: &mut [u8]) -> std::io::Result<()> {
        let end = offset + buffer.len();
        if offset >= self.window_start && end <= self.window_start + self.window.len() {
            let from = offset - self.window_start;
            buffer.copy_from_slice(&self.window[from..from + buffer.len()]);
            return Ok(());
        }
        self.file.seek(SeekFrom::Start(offset as u64))?;
        if buffer.len() >= Self::WINDOW {
            return self.file.read_exact(buffer);
        }
        let size = Self::WINDOW.min(self.len - offset);
        self.window.resize(size, 0);
        self.window_start = offset;
        if let Err(error) = self.file.read_exact(&mut self.window) {
            self.window.clear();
            return Err(error);
        }
        buffer.copy_from_slice(&self.window[..buffer.len()]);
        Ok(())
    }
}

impl ReadAt for WindowedFile {
    fn len(&self) -> usize {
        self.len
    }

    fn read_at(&mut self, offset: usize, buffer: &mut [u8]) -> std::io::Result<()> {
        self.read_window(offset, buffer)
            .map_err(|error| named(&self.path, &error))
    }
}

/// `error` with `path` in front of its message.
fn named(path: &Path, error: &std::io::Error) -> std::io::Error {
    std::io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}
