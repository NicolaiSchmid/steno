//! A speech engine and a diarizer that need no models.
//! Swift: `Sources/StenoCore/Testing/FakeSpeech.swift`.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;

use super::{CallLog, FakeFailure, sample_data};
use crate::{
    AudioBuffer16k, BoundaryResult, DiarizationResult, Diarizer, LanguageTag, RawSegment,
    SpeakerCluster, SpeechEngine, TimeRange, WordTiming,
};

/// One `transcribe` call as the fake saw it.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscribeCall {
    pub duration: f64,
    pub hint: Option<LanguageTag>,
}

/// A `SpeechEngine` that emits one segment per `segment_seconds` of audio,
/// tagged with `language`, text `"<prefix> segment <n>"`. Deterministic and
/// configurable; records every hint it was given, every `prepare` and
/// every `release`.
#[derive(Debug)]
pub struct FakeSpeechEngine {
    pub id: String,
    pub supported_languages: BTreeSet<LanguageTag>,
    pub segment_seconds: f64,
    pub language: Option<LanguageTag>,
    pub text_prefix: String,
    pub word_timings: bool,
    pub failure: Option<String>,
    /// A buffer whose peak stays below this is silence and yields no
    /// segment, the way a real engine hears a tap that recorded nothing.
    /// `None` (the default) transcribes every buffer.
    pub silent_below_peak: Option<f32>,
    pub transcriptions: CallLog<TranscribeCall>,
    pub preparations: CallLog<()>,
    pub releases: CallLog<()>,
}

impl Default for FakeSpeechEngine {
    fn default() -> Self {
        FakeSpeechEngine {
            id: "fake-engine".to_owned(),
            supported_languages: ["de", "en"].into_iter().map(LanguageTag::from).collect(),
            segment_seconds: 1.0,
            language: Some("de".into()),
            text_prefix: "fake".to_owned(),
            word_timings: false,
            failure: None,
            silent_below_peak: None,
            transcriptions: CallLog::new(),
            preparations: CallLog::new(),
            releases: CallLog::new(),
        }
    }
}

impl FakeSpeechEngine {
    /// The segments for `duration` seconds: one per `segment_seconds`, each
    /// starting exactly where the previous one ended, the last one clipped
    /// to the end; when asked, one word timing per word of the text, evenly
    /// spaced, the last word ending exactly at the segment's `end`.
    #[must_use]
    pub fn segments(
        duration: f64,
        segment_seconds: f64,
        language: Option<&LanguageTag>,
        text_prefix: &str,
        word_timings: bool,
    ) -> Vec<RawSegment> {
        if duration <= 0.0 || segment_seconds <= 0.0 {
            return Vec::new();
        }
        // Exact: a count of segments is small.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let count = (duration / segment_seconds).ceil() as usize;
        (0..count)
            .map(|index| {
                // Exact: segment indexes are small. Both bounds come from
                // the index so neighbouring segments share a boundary bit
                // for bit.
                #[allow(clippy::cast_precision_loss)]
                let start = index as f64 * segment_seconds;
                #[allow(clippy::cast_precision_loss)]
                let end = duration.min((index + 1) as f64 * segment_seconds);
                let text = format!("{text_prefix} segment {}", index + 1);
                let timings = word_timings.then(|| {
                    let words: Vec<&str> = text.split(' ').collect();
                    #[allow(clippy::cast_precision_loss)]
                    let step = (end - start) / words.len() as f64;
                    let last = words.len() - 1;
                    words
                        .iter()
                        .enumerate()
                        .map(|(index, word)| {
                            #[allow(clippy::cast_precision_loss)]
                            let offset = index as f64;
                            WordTiming {
                                word: (*word).to_owned(),
                                start: start + offset * step,
                                // `start + 3 * step` can miss `end` by an
                                // ulp; the last word ends where the
                                // segment does.
                                end: if index == last {
                                    end
                                } else {
                                    start + (offset + 1.0) * step
                                },
                            }
                        })
                        .collect()
                });
                RawSegment {
                    start,
                    end,
                    text,
                    language: language.cloned(),
                    word_timings: timings,
                }
            })
            .collect()
    }
}

#[async_trait]
impl SpeechEngine for FakeSpeechEngine {
    fn id(&self) -> &str {
        &self.id
    }

    fn supported_languages(&self) -> &BTreeSet<LanguageTag> {
        &self.supported_languages
    }

    async fn prepare(&self) -> BoundaryResult<()> {
        self.preparations.record(());
        Ok(())
    }

    async fn transcribe(
        &self,
        audio: &AudioBuffer16k,
        hint: Option<&LanguageTag>,
    ) -> BoundaryResult<Vec<RawSegment>> {
        self.transcriptions.record(TranscribeCall {
            duration: audio.duration(),
            hint: hint.cloned(),
        });
        FakeFailure::check(self.failure.as_ref())?;
        if let Some(peak) = self.silent_below_peak
            && audio.samples.iter().all(|sample| sample.abs() < peak)
        {
            return Ok(Vec::new());
        }
        Ok(Self::segments(
            audio.duration(),
            self.segment_seconds,
            self.language.as_ref(),
            &self.text_prefix,
            self.word_timings,
        ))
    }

    async fn release(&self) -> BoundaryResult<()> {
        self.releases.record(());
        Ok(())
    }
}

/// What a [`FakeDiarizer`] answers instead of its round-robin.
pub type DiarizationFn = dyn Fn(&AudioBuffer16k) -> DiarizationResult + Send + Sync;

/// A `Diarizer` that hands out `cluster_count` speakers round-robin over
/// `turn_seconds` turns, with unit embeddings along successive axes and a
/// sample clip range of at most ten seconds from the cluster's first turn.
pub struct FakeDiarizer {
    pub cluster_count: usize,
    pub turn_seconds: f64,
    /// Answers every buffer when set; the round-robin otherwise.
    pub result: Option<Arc<DiarizationFn>>,
    pub failure: Option<String>,
    /// Buffer durations of every `diarize` call.
    pub diarizations: CallLog<f64>,
    pub preparations: CallLog<()>,
}

impl std::fmt::Debug for FakeDiarizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeDiarizer")
            .field("cluster_count", &self.cluster_count)
            .field("turn_seconds", &self.turn_seconds)
            .field("result", &self.result.as_ref().map(|_| "fn"))
            .field("failure", &self.failure)
            .field("diarizations", &self.diarizations)
            .field("preparations", &self.preparations)
            .finish()
    }
}

impl Default for FakeDiarizer {
    fn default() -> Self {
        FakeDiarizer {
            cluster_count: 2,
            turn_seconds: 1.5,
            result: None,
            failure: None,
            diarizations: CallLog::new(),
            preparations: CallLog::new(),
        }
    }
}

impl FakeDiarizer {
    /// A diarizer returning exactly `result(audio)` for every buffer.
    #[must_use]
    pub fn answering(
        result: impl Fn(&AudioBuffer16k) -> DiarizationResult + Send + Sync + 'static,
    ) -> Self {
        FakeDiarizer {
            cluster_count: 0,
            turn_seconds: 0.0,
            result: Some(Arc::new(result)),
            ..Self::default()
        }
    }

    /// Speakers round-robin over `turn_seconds` turns.
    #[must_use]
    pub fn round_robin(
        duration: f64,
        cluster_count: usize,
        turn_seconds: f64,
    ) -> DiarizationResult {
        if cluster_count == 0 || turn_seconds <= 0.0 || duration <= 0.0 {
            return DiarizationResult::default();
        }
        let mut ranges: Vec<Vec<TimeRange>> = vec![Vec::new(); cluster_count];
        let mut start = 0.0;
        let mut index = 0;
        while start < duration {
            let end = duration.min(start + turn_seconds);
            ranges[index % cluster_count].push(TimeRange {
                lower: start,
                upper: end,
            });
            start += turn_seconds;
            index += 1;
        }
        DiarizationResult {
            clusters: ranges
                .into_iter()
                .enumerate()
                .filter_map(|(offset, turns)| {
                    let first = *turns.first()?;
                    // Exact: cluster offsets are small.
                    #[allow(clippy::cast_precision_loss)]
                    let confidence = 0.9_f32 - 0.1 * offset as f32;
                    Some(SpeakerCluster {
                        label: format!("Speaker {}", offset + 1),
                        ranges: turns,
                        embedding: Some(sample_data::embedding(offset)),
                        cluster_confidence: confidence,
                        sample_clip_range: Some(TimeRange {
                            lower: first.lower,
                            upper: first.upper.min(first.lower + 10.0),
                        }),
                    })
                })
                .collect(),
        }
    }
}

#[async_trait]
impl Diarizer for FakeDiarizer {
    async fn prepare(&self) -> BoundaryResult<()> {
        self.preparations.record(());
        Ok(())
    }

    async fn diarize(&self, audio: &AudioBuffer16k) -> BoundaryResult<DiarizationResult> {
        self.diarizations.record(audio.duration());
        FakeFailure::check(self.failure.as_ref())?;
        if let Some(result) = &self.result {
            return Ok(result(audio));
        }
        Ok(Self::round_robin(
            audio.duration(),
            self.cluster_count,
            self.turn_seconds,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn the_engine_emits_one_segment_per_second_and_records_hints() {
        let engine = Arc::new(FakeSpeechEngine {
            word_timings: true,
            ..Default::default()
        });
        let shared: Arc<dyn SpeechEngine> = engine.clone();
        shared.prepare().await.unwrap();
        let audio = AudioBuffer16k::silence(2.5);
        let hint = LanguageTag::from("en");
        let segments = shared.transcribe(&audio, Some(&hint)).await.unwrap();
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[2].start, 2.0);
        assert_eq!(segments[2].end, 2.5);
        assert_eq!(segments[0].text, "fake segment 1");
        assert_eq!(segments[0].language, Some("de".into()));
        let timings = segments[0].word_timings.as_ref().unwrap();
        assert_eq!(timings.len(), 3);
        assert_eq!(timings[2].word, "1");
        assert_eq!(timings[2].end, 1.0);
        assert_eq!(engine.preparations.count(), 1);
        shared.release().await.unwrap();
        assert_eq!(engine.releases.count(), 1);
        assert_eq!(
            engine.transcriptions.entries(),
            vec![TranscribeCall {
                duration: 2.5,
                hint: Some(hint)
            }]
        );
    }

    #[test]
    fn segments_share_their_boundaries_bit_for_bit() {
        // 0.7 s steps over 10 s: a boundary summed from the previous end
        // would drift by an ulp from one computed from the index.
        let segments = FakeSpeechEngine::segments(10.0, 0.7, None, "fake", true);
        assert_eq!(segments.len(), 15);
        assert_eq!(segments[0].start, 0.0);
        assert_eq!(segments[14].end, 10.0);
        for pair in segments.windows(2) {
            assert_eq!(pair[0].end, pair[1].start, "{pair:?}");
            let words = pair[0].word_timings.as_ref().unwrap();
            assert_eq!(words[0].start, pair[0].start);
            assert_eq!(words[1].start, words[0].end);
            assert_eq!(words[2].start, words[1].end);
        }
    }

    #[test]
    fn the_last_word_ends_where_its_segment_does() {
        // Over 0.9 s steps `start + 3 * step` misses `end` by an ulp in two
        // of the fourteen segments.
        let segments = FakeSpeechEngine::segments(12.0, 0.9, None, "fake", true);
        assert_eq!(segments.len(), 14);
        for segment in &segments {
            let words = segment.word_timings.as_ref().unwrap();
            assert_eq!(words[2].end.to_bits(), segment.end.to_bits(), "{segment:?}");
        }
    }

    #[tokio::test]
    async fn a_failing_engine_returns_its_message() {
        let engine = FakeSpeechEngine {
            failure: Some("no model".to_owned()),
            ..Default::default()
        };
        let error = engine
            .transcribe(&AudioBuffer16k::silence(1.0), None)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "no model");
        assert_eq!(engine.transcriptions.count(), 1);
    }

    #[tokio::test]
    async fn the_diarizer_alternates_speakers_and_clips_the_sample() {
        let diarizer = FakeDiarizer {
            turn_seconds: 15.0,
            ..Default::default()
        };
        let result = diarizer
            .diarize(&AudioBuffer16k::silence(40.0))
            .await
            .unwrap();
        assert_eq!(result.clusters.len(), 2);
        let first = &result.clusters[0];
        assert_eq!(first.label, "Speaker 1");
        assert_eq!(
            first.ranges,
            vec![
                TimeRange {
                    lower: 0.0,
                    upper: 15.0
                },
                TimeRange {
                    lower: 30.0,
                    upper: 40.0
                }
            ]
        );
        assert_eq!(
            first.sample_clip_range,
            Some(TimeRange {
                lower: 0.0,
                upper: 10.0
            })
        );
        assert_eq!(first.embedding.as_ref().unwrap().0[0], 1.0);
        assert_eq!(result.clusters[1].embedding.as_ref().unwrap().0[1], 1.0);
        // Confidence falls by a tenth per cluster from 0.9, in `f32`.
        assert_eq!(first.cluster_confidence, 0.9);
        assert_eq!(result.clusters[1].cluster_confidence, 0.9_f32 - 0.1);
        assert_eq!(diarizer.diarizations.entries(), vec![40.0]);
        assert_eq!(
            FakeDiarizer::round_robin(0.0, 2, 1.0),
            DiarizationResult::default()
        );
    }

    #[tokio::test]
    async fn an_answering_diarizer_returns_the_closure_result() {
        let diarizer = FakeDiarizer::answering(|audio| DiarizationResult {
            clusters: vec![SpeakerCluster {
                label: format!("{}", audio.len()),
                ranges: Vec::new(),
                embedding: None,
                cluster_confidence: 1.0,
                sample_clip_range: None,
            }],
        });
        let result = diarizer
            .diarize(&AudioBuffer16k::new(vec![0.0; 3]))
            .await
            .unwrap();
        assert_eq!(result.clusters[0].label, "3");
    }
}
