//! The partial file of one recording: chunks land at `index * chunk_size`,
//! so the file is sparse until the last chunk arrives and the phone may
//! send chunks in any order or twice. Whole-file hashing streams in 1 MiB
//! reads.
//!
//! Every read and write runs on the blocking pool, never on the engine's
//! task: a chunk's fsync or the hash of a 4 GiB file would otherwise hold
//! every other request, including the auth gate of unrelated connections
//! and `/v1/hello`. Swift: `Upload/ReceivingFile.swift`.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest as _, Sha256};

use crate::pinning::constant_time_equals;

pub const READ_BLOCK: usize = 1024 * 1024;

/// Creates an empty partial file (or leaves an existing one alone).
pub fn create(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        File::create(path)?;
    }
    Ok(())
}

/// Writes `data` at `offset` and flushes it to disk.
pub async fn write(
    data: impl AsRef<[u8]> + Send + 'static,
    offset: u64,
    path: PathBuf,
) -> std::io::Result<()> {
    off_task(move || {
        let mut file = OpenOptions::new().write(true).open(&path)?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(data.as_ref())?;
        file.sync_all()
    })
    .await
}

/// Whether the whole of `file`, streamed through SHA-256 from its start,
/// hashes to `expected`. `file` is freshly opened: the hash reads from the
/// current position.
pub async fn hash_matches(file: Arc<File>, expected: Vec<u8>) -> std::io::Result<bool> {
    off_task(move || {
        let mut file = &*file;
        let mut hasher = Sha256::new();
        let mut block = vec![0u8; READ_BLOCK];
        loop {
            let read = file.read(&mut block)?;
            if read == 0 {
                break;
            }
            hasher.update(&block[..read]);
        }
        Ok(constant_time_equals(&hasher.finalize(), &expected))
    })
    .await
}

/// The volume and file number of a file. While a handle to a file is
/// open, no other file gets its number, even after the file is unlinked:
/// a path whose identity matches the identity of an open handle names
/// that very file. The verify keeps the partial open from before the
/// `verifying` write to the promote, so the file it hashed is the file it
/// promotes. Swift: `ReceivingFile.Identity`, which relies on APFS never
/// reusing a file number instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    volume: u64,
    file: u128,
}

impl Identity {
    /// The identity of the open `file`.
    #[cfg(unix)]
    pub fn of(file: &File) -> std::io::Result<Identity> {
        Ok(Self::from_metadata(&file.metadata()?))
    }

    /// The identity of the file at `path` now.
    #[cfg(unix)]
    pub fn at(path: &Path) -> std::io::Result<Identity> {
        Ok(Self::from_metadata(&std::fs::metadata(path)?))
    }

    #[cfg(unix)]
    fn from_metadata(metadata: &std::fs::Metadata) -> Identity {
        use std::os::unix::fs::MetadataExt as _;
        Identity {
            volume: metadata.dev(),
            file: u128::from(metadata.ino()),
        }
    }

    /// The identity of the open `file`: the 128-bit file id where the
    /// file system has one (NTFS, ReFS), else the 64-bit file index. std's
    /// `MetadataExt::file_index` is not stable yet.
    #[cfg(windows)]
    #[allow(unsafe_code)]
    pub fn of(file: &File) -> std::io::Result<Identity> {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO, FileIdInfo, GetFileInformationByHandle,
            GetFileInformationByHandleEx,
        };

        let handle = file.as_raw_handle();
        let mut id = FILE_ID_INFO::default();
        // SAFETY: `handle` is open for the call, and `id` is a writable
        // `FILE_ID_INFO` of the size passed, the buffer `FileIdInfo` fills.
        let filled = unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileIdInfo,
                (&raw mut id).cast(),
                u32::try_from(size_of::<FILE_ID_INFO>()).expect("a small struct"),
            )
        };
        if filled != 0 {
            return Ok(Identity {
                volume: id.VolumeSerialNumber,
                file: u128::from_le_bytes(id.FileId.Identifier),
            });
        }
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: `handle` is open for the call, and `information` is a
        // writable `BY_HANDLE_FILE_INFORMATION`.
        if unsafe { GetFileInformationByHandle(handle, &raw mut information) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Identity {
            volume: u64::from(information.dwVolumeSerialNumber),
            file: (u128::from(information.nFileIndexHigh) << 32)
                | u128::from(information.nFileIndexLow),
        })
    }

    /// The identity of the file at `path` now, through a handle open for
    /// the call. std opens with delete sharing, so the handle keeps no
    /// rename or delete of the file from going through.
    #[cfg(windows)]
    pub fn at(path: &Path) -> std::io::Result<Identity> {
        Self::of(&File::open(path)?)
    }
}

async fn off_task<T: Send + 'static>(
    body: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> std::io::Result<T> {
    tokio::task::spawn_blocking(body)
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?
}
