//! A speech engine and a diarizer that need no models.

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
/// text `"<prefix> segment <n>"`, and records every call.
#[derive(Debug)]
pub struct FakeSpeechEngine {
    pub id: String,
    pub supported_languages: BTreeSet<LanguageTag>,
    pub segment_seconds: f64,
    pub language: Option<LanguageTag>,
    pub text_prefix: String,
    pub word_timings: bool,
    pub failure: Option<String>,
    /// `prepare` fails with this after recording the call.
    pub prepare_failure: Option<String>,
    /// A buffer whose peak stays below this is silence and yields no segment;
    /// `None` transcribes every buffer.
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
            prepare_failure: None,
            silent_below_peak: None,
            transcriptions: CallLog::new(),
            preparations: CallLog::new(),
            releases: CallLog::new(),
        }
    }
}

impl FakeSpeechEngine {
    /// The segments for `duration` seconds, each starting where the previous one
    /// ended; when asked, evenly spaced word timings, the last ending at `end`.
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
                // Both bounds come from the index so neighbours share a boundary bit for bit.
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
                                // The last word ends where the segment does, not an ulp off.
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
        FakeFailure::check(self.prepare_failure.as_ref())
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
/// `turn_seconds` turns, with unit embeddings along successive axes.
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
