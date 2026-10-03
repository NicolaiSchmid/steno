//! Limits on what a phone may announce. Everything here is checked before a
//! partial file is created, so a bad announce costs nothing on disk.
//! Swift: `Upload/MetadataValidation.swift`.

use steno_core::{AudioFormat, RecordingMetadata};

use crate::configuration::HandoverConfiguration;

pub struct MetadataValidation;

impl MetadataValidation {
    /// 4 GiB: about 140 hours at the phone's 64 kbps preset.
    pub const MAX_BYTE_COUNT: i64 = 4 * 1024 * 1024 * 1024;
    pub const MIN_CHUNK_SIZE: i64 = 64 * 1024;
    /// Seven days; the phone records to a file and uploads later.
    pub const MAX_DURATION_SECONDS: f64 = 7.0 * 24.0 * 3600.0;
    pub const MAX_DEVICE_NAME_LENGTH: usize = 128;
    /// Formats the intake can place and the pipeline can decode.
    pub const ACCEPTED_FORMATS: [AudioFormat; 2] = [AudioFormat::M4aAac, AudioFormat::Wav16kInt16];

    /// The first problem with `metadata`, or `None` when it is acceptable.
    #[must_use]
    pub fn problem(
        metadata: &RecordingMetadata,
        configuration: &HandoverConfiguration,
    ) -> Option<String> {
        if metadata.byte_count <= 0 || metadata.byte_count > Self::MAX_BYTE_COUNT {
            return Some(format!(
                "byteCount must be between 1 and {}",
                Self::MAX_BYTE_COUNT
            ));
        }
        if metadata.chunk_size < Self::MIN_CHUNK_SIZE
            || metadata.chunk_size > configuration.chunk_size
        {
            return Some(format!(
                "chunkSize must be between {} and {}",
                Self::MIN_CHUNK_SIZE,
                configuration.chunk_size
            ));
        }
        if metadata.sha256.len() != 32 {
            return Some("sha256 must be 32 bytes".to_owned());
        }
        if !metadata.duration_seconds.is_finite()
            || metadata.duration_seconds < 0.0
            || metadata.duration_seconds > Self::MAX_DURATION_SECONDS
        {
            #[allow(clippy::cast_possible_truncation)]
            let max = Self::MAX_DURATION_SECONDS as i64;
            return Some(format!("durationSeconds must be between 0 and {max}"));
        }
        if let Err(problem) = Self::device_name(&metadata.device_name) {
            return Some(problem);
        }
        if !Self::ACCEPTED_FORMATS.contains(&metadata.format) {
            return Some(format!("format {} is not accepted", metadata.format));
        }
        None
    }

    /// `name` trimmed, or the problem with it: the announce and the pairing
    /// request hold a device name to the same rule.
    pub(crate) fn device_name(name: &str) -> Result<&str, String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > Self::MAX_DEVICE_NAME_LENGTH {
            return Err(format!(
                "deviceName must be 1 to {} characters",
                Self::MAX_DEVICE_NAME_LENGTH
            ));
        }
        Ok(name)
    }

    /// How many chunks a recording of `byte_count` bytes has at
    /// `chunk_size`.
    #[must_use]
    pub fn chunk_count(byte_count: i64, chunk_size: i64) -> i64 {
        (byte_count + chunk_size - 1) / chunk_size
    }

    /// The byte length of chunk `index`; the last chunk may be short.
    #[must_use]
    pub fn chunk_length(index: i64, byte_count: i64, chunk_size: i64) -> i64 {
        let offset = index * chunk_size;
        chunk_size.min(byte_count - offset)
    }
}
