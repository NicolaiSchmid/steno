//! The wire format between the parent (the app) and `steno-speech-sidecar`,
//! over the child's stdin and stdout and nothing else: no socket, no file,
//! so the audio stays inside the two processes.
//!
//! A message is one frame: the length of its JSON header as a
//! little-endian `u32`, then the header, then the payload the header
//! declares. Only [`Request::Transcribe`] and [`Request::Diarize`] have a
//! payload: `sampleCount` samples of 16 kHz mono audio as little-endian
//! `f32`, bit for bit what the engine or the diarizer was given. The
//! header follows the bridge's JSON convention (`steno_core::json`: sorted
//! keys, camelCase), with a `type` tag.
//!
//! The child sends [`Reply::Ready`] first, then [`Reply::Memory`] every
//! heartbeat interval from a thread of its own, between and during
//! requests, and one reply per request with the request's `id`. Speech
//! ([`Request::Load`], [`Request::Transcribe`]) and the diarizer
//! ([`Request::LoadDiarizer`], [`Request::Diarize`]) load and run
//! independently in the one child. It exits
//! with status 0 after [`Request::Shutdown`] and when its stdin or stdout
//! closes, so a dead parent leaves no child behind. It exits with status
//! 2, after a line on stderr, when it cannot read a frame, write a reply
//! or start its heartbeat.
//!
//! ```
//! use steno_speech::sidecar::protocol::{self, Request};
//!
//! let mut frame = Vec::new();
//! protocol::write_frame(&mut frame, &Request::Health { id: 1 }, &[]).unwrap();
//! let header = br#"{"id":1,"type":"health"}"#;
//! assert_eq!(frame[..4], (header.len() as u32).to_le_bytes());
//! assert_eq!(&frame[4..], header);
//! let read: Request = protocol::read_header(&mut &frame[..]).unwrap().unwrap();
//! assert_eq!(read, Request::Health { id: 1 });
//! ```
//!
//! Swift: none; the Mac app runs `FluidAudio` in-process only.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use steno_core::{Embedding, LanguageTag, RawSegment, SpeakerCluster, TimeRange};
use thiserror::Error;

use crate::onnx::EncoderProvider;

/// Bumped on any change a peer of the old version would misread; the
/// client refuses a child whose [`Reply::Ready`] names another. A field
/// added with a default for its absence (`directml`, `provider`) is no such
/// change; a new request is (version 2: the diarizer's two).
pub const PROTOCOL_VERSION: u32 = 2;

/// The longest header either side accepts. A transcript of a long meeting
/// is a few megabytes of JSON; one of 24 hours (see [`MAX_SAMPLES`]) stays
/// under 20 MB with every word timed.
pub const MAX_HEADER_BYTES: u32 = 64 << 20;

/// The most samples one request may carry: 24 hours of 16 kHz audio. A
/// sanity bound on the header, far above any meeting; the memory ceiling
/// is what limits a real request.
pub const MAX_SAMPLES: u64 = 16_000 * 60 * 60 * 24;

/// What the parent asks of the child.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Request {
    /// Load the models from `modelsRoot`, a [`ModelStore`](crate::ModelStore)
    /// root the parent has installed them into; the child never downloads.
    Load {
        id: u64,
        models_root: PathBuf,
        /// `intraThreads`: ONNX Runtime's threads within one operator
        /// ([`OnnxOptions::intra_threads`](crate::OnnxOptions::intra_threads)).
        intra_threads: usize,
        /// `interThreads`: its threads across operators
        /// ([`OnnxOptions::inter_threads`](crate::OnnxOptions::inter_threads)).
        inter_threads: usize,
        /// [`OnnxOptions::directml`](crate::OnnxOptions::directml); absent
        /// is `false`, so a parent from before it still loads on the CPU.
        #[serde(default)]
        directml: bool,
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
    /// Load the diarizer's two models from these files, which the parent
    /// has installed (`steno_diarize::models`); the child never downloads.
    /// Independent of [`Request::Load`]: a child may hold either or both.
    LoadDiarizer {
        id: u64,
        /// pyannote segmentation 3.0.
        segmentation: PathBuf,
        /// `WeSpeaker` ResNet34-LM.
        embedding: PathBuf,
        /// `intraThreads`: ONNX Runtime's threads within one operator, for
        /// both sessions; zero lets ONNX Runtime decide.
        intra_threads: usize,
    },
    /// Diarize the `sampleCount` samples that follow the header with the
    /// diarizer's default configuration, after [`Request::LoadDiarizer`].
    Diarize { id: u64, sample_count: u64 },
    /// Answer with [`Reply::Bye`] and exit.
    Shutdown { id: u64 },
}

impl Request {
    /// The id its reply carries.
    #[must_use]
    pub fn id(&self) -> u64 {
        match self {
            Request::Load { id, .. }
            | Request::Health { id }
            | Request::Transcribe { id, .. }
            | Request::LoadDiarizer { id, .. }
            | Request::Diarize { id, .. }
            | Request::Shutdown { id } => *id,
        }
    }

    /// The payload bytes that follow the header.
    #[must_use]
    pub fn payload_bytes(&self) -> u64 {
        match self {
            Request::Transcribe { sample_count, .. } | Request::Diarize { sample_count, .. } => {
                sample_count.saturating_mul(4)
            }
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
    /// Sent once at start, before any other frame; by then a child on
    /// Linux or macOS ignores SIGINT, SIGTERM and SIGHUP (the sidecar
    /// crate's docs).
    Ready {
        /// The child's [`PROTOCOL_VERSION`].
        protocol: u32,
        /// The child's process id.
        pid: u32,
    },
    /// The heartbeat: the child's resident set, for the parent's ceiling.
    Memory { rss_bytes: u64 },
    /// The models are loaded, the encoder on `provider`
    /// ([`OnnxBackend::provider`](crate::OnnxBackend::provider)); absent is
    /// the CPU, which a child from before it always used.
    Loaded {
        id: u64,
        #[serde(default)]
        provider: EncoderProvider,
    },
    /// The answer to [`Request::Health`]; `loaded` once a load succeeded.
    Health {
        id: u64,
        pid: u32,
        rss_bytes: u64,
        loaded: bool,
        /// Where the encoder runs now; a failed run on `DirectML` moves it
        /// to the CPU after [`Reply::Loaded`]. Absent before a load and
        /// from a child that does not say; the parent then keeps the
        /// provider it last heard.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<EncoderProvider>,
    },
    /// The answer to [`Request::Transcribe`], with `provider` as in
    /// [`Reply::Health`], after the run.
    Transcript {
        id: u64,
        segments: Vec<RawSegment>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<EncoderProvider>,
    },
    /// The answer to [`Request::LoadDiarizer`]: both models are loaded.
    DiarizerLoaded { id: u64 },
    /// The answer to [`Request::Diarize`]: the clusters in the order the
    /// diarizer returned them.
    Diarization {
        id: u64,
        clusters: Vec<DiarizedCluster>,
    },
    /// The request failed inside the child; the child keeps running.
    Failed { id: u64, error: String },
    /// The answer to [`Request::Shutdown`], the child's last message.
    Bye { id: u64 },
}

impl Reply {
    /// The request this answers; `None` for `Ready` and `Memory`.
    #[must_use]
    pub fn id(&self) -> Option<u64> {
        match self {
            Reply::Ready { .. } | Reply::Memory { .. } => None,
            Reply::Loaded { id, .. }
            | Reply::Health { id, .. }
            | Reply::Transcript { id, .. }
            | Reply::DiarizerLoaded { id }
            | Reply::Diarization { id, .. }
            | Reply::Failed { id, .. }
            | Reply::Bye { id } => Some(*id),
        }
    }
}

/// A [`SpeakerCluster`] on the wire, its embedding included, which
/// `SpeakerCluster`'s own serde form leaves out. The `f32` values travel
/// widened to `f64`, which round-trips the JSON exactly
/// (`serde_json`'s `float_roundtrip`, which `steno-core` turns on)
/// whatever `serde_json`'s `f32` path does, and narrowing it back is
/// exact, so the parent gets the child's clusters bit for bit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiarizedCluster {
    pub label: String,
    pub ranges: Vec<TimeRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f64>>,
    pub cluster_confidence: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_clip_range: Option<TimeRange>,
}

impl From<&SpeakerCluster> for DiarizedCluster {
    fn from(cluster: &SpeakerCluster) -> Self {
        DiarizedCluster {
            label: cluster.label.clone(),
            ranges: cluster.ranges.clone(),
            embedding: cluster
                .embedding
                .as_ref()
                .map(|embedding| embedding.0.iter().copied().map(f64::from).collect()),
            cluster_confidence: f64::from(cluster.cluster_confidence),
            sample_clip_range: cluster.sample_clip_range,
        }
    }
}

impl From<DiarizedCluster> for SpeakerCluster {
    // Each value was an `f32` widened by the child, so narrowing it back
    // is exact.
    #[allow(clippy::cast_possible_truncation)]
    fn from(cluster: DiarizedCluster) -> Self {
        SpeakerCluster {
            label: cluster.label,
            ranges: cluster.ranges,
            embedding: cluster
                .embedding
                .map(|values| Embedding(values.into_iter().map(|value| value as f32).collect())),
            cluster_confidence: cluster.cluster_confidence as f32,
            sample_clip_range: cluster.sample_clip_range,
        }
    }
}

/// A frame that could not be read.
#[derive(Debug, Error)]
pub enum FrameError {
    /// The stream failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The stream ended inside a frame.
    #[error("the stream ended inside a frame")]
    Truncated,
    /// A header length over [`MAX_HEADER_BYTES`] or a payload over
    /// [`MAX_SAMPLES`].
    #[error("a frame of {0} bytes is over the limit")]
    TooLarge(u64),
    /// A header that does not start with `{`, so it cannot be a JSON
    /// object: binary garbage that only looked like a length prefix.
    #[error("not a protocol message: the header starts with byte {0:#04x}")]
    NotAHeader(u8),
    /// A JSON header that is no message of the protocol.
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
    let mut first = [0u8; 1];
    if length > 0 {
        read_exact(input, &mut first)?;
    }
    if first[0] != b'{' {
        return Err(FrameError::NotAHeader(first[0]));
    }
    // Grown as the bytes arrive, so a length prefix alone allocates little.
    let mut header = Vec::with_capacity((length as usize).min(INITIAL_CAPACITY));
    header.push(b'{');
    input
        .by_ref()
        .take(u64::from(length) - 1)
        .read_to_end(&mut header)?;
    if header.len() < length as usize {
        return Err(FrameError::Truncated);
    }
    Ok(Some(serde_json::from_slice(&header)?))
}

/// What a reader reserves before the bytes it was promised arrive, in
/// bytes or samples: a frame's prefix may claim far more than ever comes.
const INITIAL_CAPACITY: usize = 1 << 16;

/// The bytes of payload [`read_samples`] reads at a time.
const READ_CHUNK: usize = 1 << 16;

/// Reads the payload of a transcribe or diarize request: `sample_count`
/// samples.
pub fn read_samples<R: Read>(input: &mut R, sample_count: u64) -> Result<Vec<f32>, FrameError> {
    if sample_count > MAX_SAMPLES {
        return Err(FrameError::TooLarge(sample_count.saturating_mul(4)));
    }
    // At most 24 hours of samples, so the count and its bytes fit a usize.
    let total = sample_count as usize;
    let mut samples = Vec::with_capacity(total.min(INITIAL_CAPACITY));
    let mut buffer = vec![0u8; READ_CHUNK];
    let mut remaining = total * 4;
    while remaining > 0 {
        let chunk = &mut buffer[..remaining.min(READ_CHUNK)];
        read_exact(input, chunk)?;
        if samples.capacity() - samples.len() < chunk.len() / 4 {
            // Doubles as the bytes arrive, never past the declared count.
            samples.reserve_exact(
                samples
                    .capacity()
                    .max(chunk.len() / 4)
                    .min(total - samples.len()),
            );
        }
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

/// `samples` as the payload of a transcribe or diarize request.
#[must_use]
pub fn encode_samples(samples: &[f32]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(samples.len() * 4);
    for sample in samples {
        payload.extend_from_slice(&sample.to_le_bytes());
    }
    payload
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
                directml: true,
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
            Request::LoadDiarizer {
                id: 6,
                segmentation: PathBuf::from("/models/diarization/segmentation.onnx"),
                embedding: PathBuf::from("/models/diarization/embedding.onnx"),
                intra_threads: 4,
            },
            Request::Diarize {
                id: 7,
                sample_count: 3,
            },
            Request::Shutdown { id: 5 },
        ]
    }

    /// A cluster whose values need every bit of their `f32`.
    fn cluster() -> SpeakerCluster {
        SpeakerCluster {
            label: "Speaker 1".to_owned(),
            ranges: vec![
                TimeRange {
                    lower: 0.1,
                    upper: 1.0 / 3.0,
                },
                TimeRange {
                    lower: 2.016_875,
                    upper: 9.335_75,
                },
            ],
            embedding: Some(Embedding(
                (0..256u16)
                    .map(|i| (f32::from(i) * 0.731).sin() / 16.0 + f32::EPSILON)
                    .collect(),
            )),
            cluster_confidence: 0.1 + f32::EPSILON,
            sample_clip_range: Some(TimeRange {
                lower: 0.1,
                upper: 1.0 / 3.0,
            }),
        }
    }

    fn replies() -> Vec<Reply> {
        vec![
            Reply::Ready {
                protocol: PROTOCOL_VERSION,
                pid: 42,
            },
            Reply::Memory { rss_bytes: 1 << 30 },
            Reply::Loaded {
                id: 1,
                provider: EncoderProvider::DirectMl,
            },
            Reply::Health {
                id: 2,
                pid: 42,
                rss_bytes: 7,
                loaded: true,
                provider: Some(EncoderProvider::DirectMl),
            },
            Reply::Transcript {
                id: 3,
                provider: Some(EncoderProvider::Cpu),
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
            Reply::DiarizerLoaded { id: 6 },
            Reply::Diarization {
                id: 7,
                clusters: vec![
                    DiarizedCluster::from(&cluster()),
                    DiarizedCluster::from(&SpeakerCluster {
                        embedding: None,
                        sample_clip_range: None,
                        ..cluster()
                    }),
                ],
            },
            Reply::Bye { id: 5 },
        ]
    }

    #[test]
    fn a_cluster_crosses_the_wire_bit_for_bit_embedding_included() {
        let mut wire = Vec::new();
        let reply = Reply::Diarization {
            id: 1,
            clusters: vec![DiarizedCluster::from(&cluster())],
        };
        write_frame(&mut wire, &reply, &[]).unwrap();
        let Some(Reply::Diarization { clusters, .. }) = read_header(&mut wire.as_slice()).unwrap()
        else {
            panic!("a diarization");
        };
        let back: Vec<SpeakerCluster> = clusters.into_iter().map(SpeakerCluster::from).collect();
        assert_eq!(back, [cluster()]);
        let bits = |cluster: &SpeakerCluster| -> Vec<u32> {
            cluster
                .embedding
                .as_ref()
                .unwrap()
                .0
                .iter()
                .map(|v| v.to_bits())
                .collect()
        };
        assert_eq!(bits(&back[0]), bits(&cluster()));
        assert_eq!(
            back[0].cluster_confidence.to_bits(),
            cluster().cluster_confidence.to_bits()
        );
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
            if let Request::Transcribe { sample_count, .. }
            | Request::Diarize { sample_count, .. } = request
            {
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

    /// [`Reply::Transcript`] as a parent from before its `provider` reads it.
    #[derive(Deserialize)]
    #[serde(tag = "type", rename_all = "camelCase")]
    enum OldReply {
        Transcript { id: u64, segments: Vec<RawSegment> },
    }

    #[test]
    fn the_provider_fields_are_camel_case_and_default_when_absent() {
        fn header(message: &impl Serialize) -> String {
            let mut wire = Vec::new();
            write_frame(&mut wire, message, &[]).unwrap();
            String::from_utf8(wire[4..].to_vec()).unwrap()
        }
        assert_eq!(
            header(&Request::Load {
                id: 1,
                models_root: PathBuf::from("/m"),
                intra_threads: 4,
                inter_threads: 1,
                directml: true,
            }),
            r#"{"directml":true,"id":1,"interThreads":1,"intraThreads":4,"modelsRoot":"/m","type":"load"}"#
        );
        assert_eq!(
            header(&Reply::Loaded {
                id: 1,
                provider: EncoderProvider::DirectMl,
            }),
            r#"{"id":1,"provider":"directml","type":"loaded"}"#
        );
        let old_load: Request = serde_json::from_str(
            r#"{"id":1,"interThreads":1,"intraThreads":4,"modelsRoot":"/m","type":"load"}"#,
        )
        .unwrap();
        assert!(matches!(
            old_load,
            Request::Load {
                directml: false,
                ..
            }
        ));
        let old_loaded: Reply = serde_json::from_str(r#"{"id":1,"type":"loaded"}"#).unwrap();
        assert_eq!(
            old_loaded,
            Reply::Loaded {
                id: 1,
                provider: EncoderProvider::Cpu,
            }
        );

        // The live provider: by name when known, absent (not null) when
        // not, and absent reads as unknown.
        assert_eq!(
            header(&Reply::Health {
                id: 2,
                pid: 42,
                rss_bytes: 7,
                loaded: true,
                provider: Some(EncoderProvider::Cpu),
            }),
            r#"{"id":2,"loaded":true,"pid":42,"provider":"cpu","rssBytes":7,"type":"health"}"#
        );
        assert_eq!(
            header(&Reply::Transcript {
                id: 3,
                segments: Vec::new(),
                provider: None,
            }),
            r#"{"id":3,"segments":[],"type":"transcript"}"#
        );
        let old_health: Reply = serde_json::from_str(
            r#"{"id":2,"loaded":false,"pid":42,"rssBytes":7,"type":"health"}"#,
        )
        .unwrap();
        assert!(matches!(old_health, Reply::Health { provider: None, .. }));
        let old_transcript: Reply =
            serde_json::from_str(r#"{"id":3,"segments":[],"type":"transcript"}"#).unwrap();
        assert!(matches!(
            old_transcript,
            Reply::Transcript { provider: None, .. }
        ));
        // A parent from before the field reads a reply that has it.
        let OldReply::Transcript { id, segments } = serde_json::from_str(
            r#"{"id":3,"provider":"directml","segments":[],"type":"transcript"}"#,
        )
        .unwrap();
        assert_eq!((id, segments.len()), (3, 0));
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
            Err(FrameError::NotAHeader(b'n'))
        ));
        let mut braces = 3u32.to_le_bytes().to_vec();
        braces.extend_from_slice(b"{x}");
        assert!(matches!(
            read_header::<_, Request>(&mut braces.as_slice()),
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

    #[test]
    fn binary_garbage_is_refused_before_its_claimed_length_is_awaited() {
        // A real header is a JSON object; anything else fails at its first
        // byte, whatever length the four bytes before it claimed.
        for (length, bytes) in [
            (MAX_HEADER_BYTES, &b"\x00\x01binary"[..]),
            (2, b"[]"),
            (1, b" "),
            (0, b""),
        ] {
            let mut wire = length.to_le_bytes().to_vec();
            wire.extend_from_slice(bytes);
            let first = bytes.first().copied().unwrap_or(0);
            assert!(
                matches!(
                    read_header::<_, Request>(&mut wire.as_slice()),
                    Err(FrameError::NotAHeader(byte)) if byte == first
                ),
                "{length}"
            );
        }
        // A header that starts right but stops short is cut, not awaited.
        let mut cut = MAX_HEADER_BYTES.to_le_bytes().to_vec();
        cut.extend_from_slice(br#"{"id":1"#);
        assert!(matches!(
            read_header::<_, Request>(&mut cut.as_slice()),
            Err(FrameError::Truncated)
        ));
        const { assert!(MAX_HEADER_BYTES <= 64 << 20) };
    }

    #[test]
    fn huge_sample_counts_neither_overflow_nor_panic() {
        let request = Request::Transcribe {
            id: 1,
            sample_count: u64::MAX,
            hint: None,
        };
        assert_eq!(request.payload_bytes(), u64::MAX);
        assert!(matches!(
            read_samples(&mut &[][..], u64::MAX),
            Err(FrameError::TooLarge(u64::MAX))
        ));
        // The largest count allowed, with eight bytes behind it.
        assert!(matches!(
            read_samples(&mut &[0u8; 8][..], MAX_SAMPLES),
            Err(FrameError::Truncated)
        ));
    }

    #[test]
    fn a_long_payload_arrives_whole_across_many_reads() {
        // Past the initial capacity and the read buffer, at an odd length.
        let samples: Vec<f32> = (0..200_003u32).map(|i| (i as f32).sin()).collect();
        let payload = encode_samples(&samples);
        assert_eq!(payload.len(), samples.len() * 4);
        let read = read_samples(&mut payload.as_slice(), samples.len() as u64).unwrap();
        assert_eq!(read, samples);
        assert_eq!(read.capacity(), samples.len());
    }
}
