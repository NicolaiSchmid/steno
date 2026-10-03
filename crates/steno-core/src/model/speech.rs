//! What the speech engine and the diarizer return for one lane, and the
//! intermediate pieces the speech crate builds them from.
//! Swift: `RawSegment`, `WordTiming`, `DiarizationResult` and
//! `SpeakerCluster` in `Sources/StenoCore/Model/Transcript.swift`,
//! `TimedWord` in `Sources/StenoSpeech/Engines/TimedWord.swift` and
//! `ClusterChunk` in `Sources/StenoSpeech/Diarization/ClusterChunk.swift`.

use serde::{Deserialize, Serialize};

use super::{Embedding, LanguageTag, TimeRange};

/// What a `SpeechEngine` returns: untimed by speaker, tagged with the
/// detected language when the engine knows it.
/// Swift: `RawSegment` in `Sources/StenoCore/Model/Transcript.swift`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// The engine's detected language as a tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<LanguageTag>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_timings: Option<Vec<WordTiming>>,
}

impl RawSegment {
    #[must_use]
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}

/// One word of a [`RawSegment`] with its own timing.
/// Swift: `WordTiming` in `Sources/StenoCore/Model/Transcript.swift`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordTiming {
    pub word: String,
    pub start: f64,
    pub end: f64,
}

/// One timed piece of text with the decoder's confidence, the unit the
/// transcript segmenter works on. Parakeet's decoder tokens arrive as these
/// and the token aggregator joins them.
/// Swift: `Sources/StenoSpeech/Engines/TimedWord.swift`.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
    /// `1.0` when the engine reports none.
    pub confidence: f32,
}

impl TimedWord {
    /// The word as the transcript carries it, confidence dropped.
    #[must_use]
    pub fn word_timing(&self) -> WordTiming {
        WordTiming {
            word: self.text.clone(),
            start: self.start,
            end: self.end,
        }
    }
}

/// What a `Diarizer` returns for one lane.
/// Swift: `DiarizationResult` in `Sources/StenoCore/Model/Transcript.swift`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DiarizationResult {
    pub clusters: Vec<SpeakerCluster>,
}

/// One diarization cluster before it becomes a `Speaker` row.
/// Swift: `SpeakerCluster` in `Sources/StenoCore/Model/Transcript.swift`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerCluster {
    /// The diarizer's label, "Speaker 1" onwards in order of first speech.
    pub label: String,
    pub ranges: Vec<TimeRange>,
    /// The cluster's voice: its chunk embeddings at unit length, summed
    /// with duration as weight and L2-normalised (`ClusterEmbedding` in the
    /// speech crate); `None` when no chunk carried a usable vector. Skipped
    /// by serde like `Speaker::embedding` because the Rust `Embedding` has
    /// no serde form yet; Swift's cluster is synthesized `Codable` with its
    /// `Embedding`, and `Tests/StenoCoreTests/ModelCodableTests.swift`
    /// round-trips it, so this side drops the vector that side keeps.
    #[serde(skip)]
    pub embedding: Option<Embedding>,
    pub cluster_confidence: f32,
    /// The diarizer-chosen clip for manual naming, at most ten seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_clip_range: Option<TimeRange>,
}

/// One "who spoke when" turn from the diarizer, before ranges are merged.
/// Swift: `SpeakerTurn` in `Sources/StenoSpeech/Diarization/ClusterChunk.swift`.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerTurn {
    pub speaker_label: String,
    pub start: f64,
    pub end: f64,
    pub quality: f32,
}

impl SpeakerTurn {
    #[must_use]
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}

/// One embedding window of the offline diarizer after clustering: which
/// speaker it was assigned to, when, and its 256-dim `WeSpeaker` vector.
/// `quality` is not reported per chunk by the model; the mapping fills it
/// from the turn the chunk overlaps most.
/// Swift: `ClusterChunk` in `Sources/StenoSpeech/Diarization/ClusterChunk.swift`.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterChunk {
    pub speaker_label: String,
    pub start: f64,
    pub end: f64,
    pub embedding: Vec<f32>,
    /// `1.0` until the mapping fills it.
    pub quality: f32,
}

impl ClusterChunk {
    #[must_use]
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    /// The chunk's closed range; an inverted chunk collapses to its start.
    #[must_use]
    pub fn range(&self) -> TimeRange {
        TimeRange {
            lower: self.start,
            upper: self.end.max(self.start),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::to_column_string;

    #[test]
    fn raw_segments_encode_like_swift() {
        let segment = RawSegment {
            start: 0.0,
            end: 1.5,
            text: "hallo".to_owned(),
            language: Some("de".into()),
            word_timings: Some(vec![WordTiming {
                word: "hallo".to_owned(),
                start: 0.0,
                end: 0.5,
            }]),
        };
        let text = to_column_string(&segment).unwrap();
        assert_eq!(
            text,
            r#"{"end":1.5,"language":"de","start":0,"text":"hallo","wordTimings":[{"end":0.5,"start":0,"word":"hallo"}]}"#
        );
        assert_eq!(serde_json::from_str::<RawSegment>(&text).unwrap(), segment);
        let bare = RawSegment {
            language: None,
            word_timings: None,
            ..segment
        };
        assert_eq!(
            to_column_string(&bare).unwrap(),
            r#"{"end":1.5,"start":0,"text":"hallo"}"#
        );
        assert_eq!(bare.duration(), 1.5);
    }

    #[test]
    fn clusters_encode_ranges_as_pairs_and_skip_the_embedding() {
        let result = DiarizationResult {
            clusters: vec![SpeakerCluster {
                label: "Speaker 1".to_owned(),
                ranges: vec![TimeRange {
                    lower: 0.0,
                    upper: 2.0,
                }],
                embedding: Some(Embedding(vec![1.0, 0.0])),
                cluster_confidence: 0.9,
                sample_clip_range: Some(TimeRange {
                    lower: 0.0,
                    upper: 2.0,
                }),
            }],
        };
        let text = to_column_string(&result).unwrap();
        assert_eq!(
            text,
            r#"{"clusters":[{"clusterConfidence":0.9,"label":"Speaker 1","ranges":[[0,2]],"sampleClipRange":[0,2]}]}"#
        );
        let parsed: DiarizationResult = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.clusters[0].embedding, None);
        assert_eq!(parsed.clusters[0].ranges, result.clusters[0].ranges);
    }

    #[test]
    fn inverted_segments_and_turns_have_zero_duration() {
        let segment = RawSegment {
            start: 2.0,
            end: 1.0,
            text: String::new(),
            language: None,
            word_timings: None,
        };
        assert_eq!(segment.duration(), 0.0);
        assert_eq!(
            RawSegment {
                end: 2.75,
                ..segment
            }
            .duration(),
            0.75
        );
        let turn = SpeakerTurn {
            speaker_label: "Speaker 1".to_owned(),
            start: 5.0,
            end: 4.0,
            quality: 1.0,
        };
        assert_eq!(turn.duration(), 0.0);
        assert_eq!(SpeakerTurn { end: 5.5, ..turn }.duration(), 0.5);
    }

    #[test]
    fn an_inverted_chunk_has_zero_duration_and_an_empty_range() {
        let chunk = ClusterChunk {
            speaker_label: "Speaker 2".to_owned(),
            start: 3.0,
            end: 2.0,
            embedding: vec![],
            quality: 1.0,
        };
        assert_eq!(chunk.duration(), 0.0);
        assert_eq!(
            chunk.range(),
            TimeRange {
                lower: 3.0,
                upper: 3.0
            }
        );
    }

    #[test]
    fn a_timed_word_drops_its_confidence_to_become_a_word_timing() {
        let word = TimedWord {
            text: "ja".to_owned(),
            start: 0.1,
            end: 0.2,
            confidence: 0.5,
        };
        assert_eq!(
            word.word_timing(),
            WordTiming {
                word: "ja".to_owned(),
                start: 0.1,
                end: 0.2
            }
        );
    }
}
