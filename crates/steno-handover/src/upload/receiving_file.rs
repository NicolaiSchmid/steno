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
pub async fn write(data: Vec<u8>, offset: u64, path: PathBuf) -> std::io::Result<()> {
    off_task(move || {
        let mut file = OpenOptions::new().write(true).open(&path)?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(&data)?;
        file.sync_all()
    })
    .await
}

/// Whether the whole file, streamed through SHA-256, hashes to `expected`.
pub async fn hash_matches(path: PathBuf, expected: Vec<u8>) -> std::io::Result<bool> {
    off_task(move || {
        let mut file = File::open(&path)?;
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

pub fn size(path: &Path) -> std::io::Result<u64> {
    Ok(std::fs::metadata(path)?.len())
}

async fn off_task<T: Send + 'static>(
    body: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> std::io::Result<T> {
    tokio::task::spawn_blocking(body)
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?
}
