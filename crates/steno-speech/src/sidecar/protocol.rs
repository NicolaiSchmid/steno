//! The wire format between the app and `steno-speech-sidecar`, over the
//! child's stdin and stdout and nothing else: no socket, no file, so the
//! audio stays inside the two processes.
//!
//! A message is one frame: the length of its JSON header as a
//! little-endian `u32`, then the header, then the payload the header
//! declares. Only [`Request::Transcribe`] has a payload: `sampleCount`
//! samples of 16 kHz mono audio as little-endian `f32`, bit for bit what
//! the engine was given. The header follows the bridge's JSON convention
//! (`steno_core::json`: sorted keys, camelCase), with a `type` tag.
//!
//! The child sends [`Reply::Ready`] once it runs, then [`Reply::Memory`]
//! every heartbeat interval from a thread of its own, between and during
//! requests, and one reply per request with the request's `id`. It exits
//! after [`Request::Shutdown`] and when its stdin or stdout closes, so a
//! dead parent leaves no child behind.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use steno_core::{LanguageTag, RawSegment};
use thiserror::Error;

/// Bumped on any change a peer of the old version would misread; the
/// client refuses a child whose [`Reply::Ready`] names another.
pub const PROTOCOL_VERSION: u32 = 1;

/// The longest header either side accepts. A transcript of a long meeting
/// is a few megabytes of JSON.
pub const MAX_HEADER_BYTES: u32 = 256 << 20;

/// The most samples one request may carry: 24 hours of 16 kHz audio. A
/// sanity bound on the header, far above any meeting; the memory ceiling
/// is what limits a real request.
pub const MAX_SAMPLES: u64 = 16_000 * 60 * 60 * 24;

/// What the app asks of the child.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Request {
    /// Load the models from `modelsRoot`, a [`ModelStore`](crate::ModelStore)
    /// root the app has installed them into; the child never downloads.
    Load {
        id: u64,
        models_root: PathBuf,
        intra_threads: usize,
        inter_threads: usize,
    },
    /// Answer with [`Reply::Health`].
    Health { id: u64 },
    /// Transcribe the `sampleCount` samples that follow the header.
    Transcribe {
        id: u64,
        sample_count: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hint: Option<LanguageTag>,
    },
    /// Answer with [`Reply::Bye`] and exit.
    Shutdown { id: u64 },
}

impl Request {
    #[must_use]
    pub fn id(&self) -> u64 {
        match self {
            Request::Load { id, .. }
            | Request::Health { id }
            | Request::Transcribe { id, .. }
            | Request::Shutdown { id } => *id,
        }
    }

    /// The payload bytes that follow the header.
    #[must_use]
    pub fn payload_bytes(&self) -> u64 {
        match self {
            Request::Transcribe { sample_count, .. } => sample_count * 4,
            _ => 0,
        }
    }
}

/// What the child sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Reply {
    /// Sent once at start.
    Ready {
        protocol: u32,
        pid: u32,
    },
    /// The heartbeat: the child's resident set, for the parent's ceiling.
    Memory {
        rss_bytes: u64,
    },
    Loaded {
        id: u64,
    },
    Health {
        id: u64,
        pid: u32,
        rss_bytes: u64,
        loaded: bool,
    },
    Transcript {
        id: u64,
        segments: Vec<RawSegment>,
    },
    /// The request failed inside the child; the child keeps running.
    Failed {
        id: u64,
        error: String,
    },
    Bye {
        id: u64,
    },
}

impl Reply {
    /// The request this answers; `None` for `Ready` and `Memory`.
    #[must_use]
    pub fn id(&self) -> Option<u64> {
        match self {
            Reply::Ready { .. } | Reply::Memory { .. } => None,
            Reply::Loaded { id }
            | Reply::Health { id, .. }
            | Reply::Transcript { id, .. }
            | Reply::Failed { id, .. }
            | Reply::Bye { id } => Some(*id),
        }
    }
}

/// A frame that could not be read.
#[derive(Debug, Error)]
pub enum FrameError {
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The stream ended inside a frame.
    #[error("the stream ended inside a frame")]
    Truncated,
    /// A header length over [`MAX_HEADER_BYTES`] or a payload over
    /// [`MAX_SAMPLES`].
    #[error("a frame of {0} bytes is over the limit")]
    TooLarge(u64),
    #[error("not a protocol message: {0}")]
    Json(#[from] serde_json::Error),
}

/// Writes `message` and `payload` as one frame and flushes. Callers that
/// share a writer between threads hold its lock across the call.
pub fn write_frame<W: Write, T: Serialize>(
    out: &mut W,
    message: &T,
    payload: &[u8],
) -> io::Result<()> {
    let header = steno_core::json::to_column_string(message).map_err(io::Error::other)?;
    let length = u32::try_from(header.len())
        .ok()
        .filter(|&n| n <= MAX_HEADER_BYTES)
        .ok_or_else(|| io::Error::other("header over the frame limit"))?;
    let mut head = Vec::with_capacity(4 + header.len());
    head.extend_from_slice(&length.to_le_bytes());
    head.extend_from_slice(header.as_bytes());
    out.write_all(&head)?;
    out.write_all(payload)?;
    out.flush()
}

/// Reads one header; `None` when the stream ends cleanly before a frame.
pub fn read_header<R: Read, T: DeserializeOwned>(input: &mut R) -> Result<Option<T>, FrameError> {
    let mut length = [0u8; 4];
    let mut filled = 0;
    while filled < 4 {
        match input.read(&mut length[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(FrameError::Truncated),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let length = u32::from_le_bytes(length);
    if length > MAX_HEADER_BYTES {
        return Err(FrameError::TooLarge(u64::from(length)));
    }
    let mut header = vec![0u8; length as usize];
    read_exact(input, &mut header)?;
    Ok(Some(serde_json::from_slice(&header)?))
}

/// Reads the payload of a transcribe request: `sample_count` samples.
pub fn read_samples<R: Read>(input: &mut R, sample_count: u64) -> Result<Vec<f32>, FrameError> {
    if sample_count > MAX_SAMPLES {
        return Err(FrameError::TooLarge(sample_count * 4));
    }
    let mut samples = Vec::with_capacity(sample_count as usize);
    let mut buffer = vec![0u8; 1 << 16];
    let mut remaining = sample_count as usize * 4;
    while remaining > 0 {
        let chunk = &mut buffer[..remaining.min(1 << 16)];
        read_exact(input, chunk)?;
        samples.extend(
            chunk
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b)),
        );
        remaining -= chunk.len();
    }
    Ok(samples)
}

/// `samples` as the payload of a transcribe request.
#[must_use]
pub fn encode_samples(samples: &[f32]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn read_exact<R: Read>(input: &mut R, buffer: &mut [u8]) -> Result<(), FrameError> {
    input.read_exact(buffer).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            FrameError::Truncated
        } else {
            FrameError::Io(e)
        }
    })
}

#[cfg(test)]
mod tests {
    use steno_core::WordTiming;

    use super::*;

    fn requests() -> Vec<Request> {
        vec![
            Request::Load {
                id: 1,
                models_root: PathBuf::from("/models"),
                intra_threads: 4,
                inter_threads: 1,
            },
            Request::Health { id: 2 },
            Request::Transcribe {
                id: 3,
                sample_count: 3,
                hint: Some(LanguageTag::from("de")),
            },
            Request::Transcribe {
                id: 4,
                sample_count: 0,
                hint: None,
            },
            Request::Shutdown { id: 5 },
        ]
    }

    fn replies() -> Vec<Reply> {
        vec![
            Reply::Ready {
                protocol: PROTOCOL_VERSION,
                pid: 42,
            },
            Reply::Memory { rss_bytes: 1 << 30 },
            Reply::Loaded { id: 1 },
            Reply::Health {
                id: 2,
                pid: 42,
                rss_bytes: 7,
                loaded: true,
            },
            Reply::Transcript {
                id: 3,
                segments: vec![RawSegment {
                    start: 0.08,
                    end: 1.5,
                    text: "Guten Tag".to_owned(),
                    language: Some(LanguageTag::from("de")),
                    word_timings: Some(vec![WordTiming {
                        word: "Guten".to_owned(),
                        start: 0.08,
                        end: 0.6,
                    }]),
                }],
            },
            Reply::Failed {
                id: 4,
                error: "no".to_owned(),
            },
            Reply::Bye { id: 5 },
        ]
    }

    #[test]
    fn every_message_round_trips_and_payloads_follow_their_header() {
        let samples = [0.25f32, -1.0, f32::MIN_POSITIVE];
        let mut wire = Vec::new();
        for request in requests() {
            let payload = if request.payload_bytes() > 0 {
                encode_samples(&samples)
            } else {
                Vec::new()
            };
            assert_eq!(payload.len() as u64, request.payload_bytes());
            write_frame(&mut wire, &request, &payload).unwrap();
        }
        let mut input = wire.as_slice();
        for expected in requests() {
            let request: Request = read_header(&mut input).unwrap().unwrap();
            assert_eq!(request, expected);
            if let Request::Transcribe { sample_count, .. } = request {
                let read = read_samples(&mut input, sample_count).unwrap();
                assert_eq!(read, &samples[..sample_count as usize]);
            }
        }
        assert!(read_header::<_, Request>(&mut input).unwrap().is_none());

        let mut wire = Vec::new();
        for reply in replies() {
            write_frame(&mut wire, &reply, &[]).unwrap();
        }
        let mut input = wire.as_slice();
        for expected in replies() {
            let reply: Reply = read_header(&mut input).unwrap().unwrap();
            assert_eq!(reply.id(), expected.id());
            assert_eq!(reply, expected);
        }
    }

    #[test]
    fn headers_use_the_bridge_convention() {
        let mut wire = Vec::new();
        write_frame(
            &mut wire,
            &Request::Transcribe {
                id: 9,
                sample_count: 16_000,
                hint: Some(LanguageTag::from("en")),
            },
            &[],
        )
        .unwrap();
        assert_eq!(
            std::str::from_utf8(&wire[4..]).unwrap(),
            r#"{"hint":"en","id":9,"sampleCount":16000,"type":"transcribe"}"#
        );
        assert_eq!(
            u32::from_le_bytes(wire[..4].try_into().unwrap()) as usize,
            wire.len() - 4
        );
    }

    #[test]
    fn broken_frames_are_errors_not_messages() {
        let mut wire = Vec::new();
        write_frame(&mut wire, &Request::Health { id: 1 }, &[]).unwrap();
        // Cut inside the length, inside the header and inside a payload.
        for cut in [2, 10] {
            assert!(matches!(
                read_header::<_, Request>(&mut &wire[..cut]),
                Err(FrameError::Truncated)
            ));
        }
        assert!(matches!(
            read_samples(&mut &[0u8; 6][..], 2),
            Err(FrameError::Truncated)
        ));
        assert!(matches!(
            read_samples(&mut &[][..], MAX_SAMPLES + 1),
            Err(FrameError::TooLarge(_))
        ));
        let mut huge = (MAX_HEADER_BYTES + 1).to_le_bytes().to_vec();
        huge.extend_from_slice(b"{}");
        assert!(matches!(
            read_header::<_, Request>(&mut huge.as_slice()),
            Err(FrameError::TooLarge(_))
        ));
        let mut garbage = 4u32.to_le_bytes().to_vec();
        garbage.extend_from_slice(b"nope");
        assert!(matches!(
            read_header::<_, Request>(&mut garbage.as_slice()),
            Err(FrameError::Json(_))
        ));
        let mut unknown = Vec::new();
        write_frame(
            &mut unknown,
            &serde_json::json!({"type": "dance", "id": 1}),
            &[],
        )
        .unwrap();
        assert!(matches!(
            read_header::<_, Request>(&mut unknown.as_slice()),
            Err(FrameError::Json(_))
        ));
    }
}
