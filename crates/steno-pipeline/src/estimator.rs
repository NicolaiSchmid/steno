//! The learned stage rates and the arithmetic behind `progress` events.
//! Swift: `Sources/StenoCore/Pipeline/ProcessingEstimator.swift`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use steno_core::{
    AudioLane, PipelineStage, ProcessingProgress, Settings, StageRate, StageRateRow,
    TranscriptSegment,
};

/// Weight of the newest sample in the moving average.
pub const ALPHA: f64 = 0.3;

/// The rate after one more measurement: the first sample replaces a seed
/// outright, so the second run already runs on this machine's numbers;
/// later samples are folded in as an exponential moving average with
/// [`ALPHA`]. Swift: `StageRate.absorbing`.
#[must_use]
pub fn absorbing(current: StageRate, seconds_per_unit: f64) -> StageRate {
    if current.samples <= 0 {
        return StageRate {
            seconds_per_unit,
            samples: 1,
        };
    }
    StageRate {
        seconds_per_unit: ALPHA * seconds_per_unit + (1.0 - ALPHA) * current.seconds_per_unit,
        samples: current.samples + 1,
    }
}

fn seed(seconds_per_unit: f64) -> StageRate {
    StageRate {
        seconds_per_unit,
        samples: 0,
    }
}

/// One measured stage: what the pipeline records once a stage ran alone in
/// flight and did not fail. Swift: `StageSample`.
#[derive(Debug, Clone, PartialEq)]
pub struct StageSample {
    pub stage: PipelineStage,
    /// The speech engine id for `transcribe`, the LLM model for `cleanup`
    /// and `summarize`, [`StageRates::UNKEYED`] for every other stage.
    pub key: String,
    pub seconds_per_unit: f64,
    pub recorded_at: DateTime<Utc>,
}

/// What a stage's cost depends on. Swift: `PipelineStage.CostDriver`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostDriver {
    /// Audio seconds summed over the lanes decoded, transcribed or encoded.
    AudioSecondsAllLanes,
    /// Audio seconds of the one diarized lane.
    AudioSecondsOneLane,
    /// Thousands of transcript tokens, guessed from the duration until the
    /// transcript exists.
    ThousandTokens,
    /// One unit: rows and files, independent of the meeting.
    Flat,
}

/// What a stage's rate is keyed by beside the stage itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateKeying {
    None,
    SpeechEngine,
    LlmModel,
}

/// The per-stage table: `units`, `key`, `is_keyed` and `is_learned` read it.
pub trait StageCost {
    fn cost_driver(self) -> CostDriver;
    fn rate_keying(self) -> RateKeying;
    /// Whether the pipeline learns the stage's rate. Decode is never
    /// learned: the CLI and the app decode through different decoders and
    /// a shared rate would mix them, so it stays on its flat seed.
    fn learns_rate(self) -> bool;
}

impl StageCost for PipelineStage {
    fn cost_driver(self) -> CostDriver {
        match self {
            PipelineStage::Decode | PipelineStage::Transcribe | PipelineStage::Persist => {
                CostDriver::AudioSecondsAllLanes
            }
            PipelineStage::Diarize => CostDriver::AudioSecondsOneLane,
            PipelineStage::Cleanup | PipelineStage::Summarize => CostDriver::ThousandTokens,
            PipelineStage::MatchSpeakers
            | PipelineStage::Merge
            | PipelineStage::Deliver
            | PipelineStage::Retention => CostDriver::Flat,
        }
    }

    fn rate_keying(self) -> RateKeying {
        match self {
            PipelineStage::Transcribe => RateKeying::SpeechEngine,
            PipelineStage::Cleanup | PipelineStage::Summarize => RateKeying::LlmModel,
            _ => RateKeying::None,
        }
    }

    fn learns_rate(self) -> bool {
        self != PipelineStage::Decode
    }
}

/// The per-stage rates the estimator weighs stages with, keyed by stage
/// and by what the stage depends on. [`StageRates::seeds`] is the one
/// source of the seed constants; a key without a sample falls back to its
/// seed, and a transcribe key without a seed (a fake engine) to the
/// `parakeet-v3` seed. Swift: `StageRates`.
#[derive(Debug, Clone, PartialEq)]
pub struct StageRates {
    entries: BTreeMap<(PipelineStage, String), StageRate>,
}

impl StageRates {
    /// The key of a stage whose cost depends on no engine or model.
    pub const UNKEYED: &'static str = "";
    /// The key of `cleanup` and `summarize` when no LLM model is configured
    /// and the passthrough passes run.
    pub const NO_MODEL: &'static str = "none";
    /// The transcribe seed an engine without a seed of its own uses.
    pub const FALLBACK_ENGINE: &'static str = "parakeet-v3";

    /// Seconds per unit before any run was measured on this machine, in
    /// the units of [`CostDriver`]. Basis and provenance:
    /// `.plans/2026-09-28-processing-progress.md` D2.
    #[must_use]
    pub fn seeds() -> Self {
        let mut rates = StageRates {
            entries: BTreeMap::new(),
        };
        let mut set = |stage, key: &str, value| rates.set(seed(value), stage, key);
        set(PipelineStage::Decode, Self::UNKEYED, 0.005);
        set(PipelineStage::Transcribe, "parakeet-v3", 1.0 / 90.0);
        set(PipelineStage::Transcribe, "parakeet-ultra", 1.0 / 90.0);
        set(PipelineStage::Transcribe, "parakeet-de", 1.0 / 90.0);
        set(
            PipelineStage::Transcribe,
            "whisperkit-large-v3-turbo",
            1.0 / 6.0,
        );
        set(PipelineStage::Diarize, Self::UNKEYED, 1.0 / 60.0);
        set(PipelineStage::MatchSpeakers, Self::UNKEYED, 0.05);
        set(PipelineStage::Merge, Self::UNKEYED, 0.1);
        set(PipelineStage::Cleanup, Self::UNKEYED, 12.0);
        set(PipelineStage::Summarize, Self::UNKEYED, 3.0);
        set(PipelineStage::Persist, Self::UNKEYED, 0.005);
        set(PipelineStage::Deliver, Self::UNKEYED, 0.5);
        set(PipelineStage::Retention, Self::UNKEYED, 0.05);
        rates
    }

    /// The seeds with every stored row laid over them. A row whose key is
    /// not empty for a stage that is not keyed, or whose rate is negative
    /// or not a finite number, is ignored. Swift: `MeetingStore.stageRates`.
    #[must_use]
    pub fn from_rows(rows: &[StageRateRow]) -> Self {
        let mut rates = Self::seeds();
        for row in rows {
            let keyed = Self::is_keyed(row.stage);
            if (!keyed && row.key != Self::UNKEYED)
                || !row.rate.seconds_per_unit.is_finite()
                || row.rate.seconds_per_unit < 0.0
            {
                continue;
            }
            rates.set(row.rate, row.stage, &row.key);
        }
        rates
    }

    /// Whether a stage's rate is keyed by an engine or a model.
    #[must_use]
    pub fn is_keyed(stage: PipelineStage) -> bool {
        stage.rate_keying() != RateKeying::None
    }

    fn seed_key(stage: PipelineStage) -> &'static str {
        if stage.rate_keying() == RateKeying::SpeechEngine {
            Self::FALLBACK_ENGINE
        } else {
            Self::UNKEYED
        }
    }

    /// The learned rate for `stage` under `key`, else its seed, else the
    /// stage's fallback seed. Every table starts from [`StageRates::seeds`],
    /// so a key missing here has no seed of its own either.
    #[must_use]
    pub fn rate(&self, stage: PipelineStage, key: &str) -> StageRate {
        self.entries
            .get(&(stage, key.to_owned()))
            .copied()
            .or_else(|| {
                Self::seeds()
                    .entries
                    .get(&(stage, Self::seed_key(stage).to_owned()))
                    .copied()
            })
            .unwrap_or_else(|| seed(0.0))
    }

    pub fn set(&mut self, rate: StageRate, stage: PipelineStage, key: &str) {
        self.entries.insert((stage, key.to_owned()), rate);
    }
}

/// Pure arithmetic behind `progress` events: from a meeting's duration,
/// its lanes, a transcript token count and [`StageRates`], the expected
/// seconds per stage, the expected remaining time from any point in the
/// run and the fraction at which the next event is expected.
/// Swift: `ProcessingEstimator`.
#[derive(Debug, Clone)]
pub struct ProcessingEstimator {
    /// The meeting's audio in seconds.
    pub duration: f64,
    pub lanes: Vec<AudioLane>,
    /// Transcript tokens, guessed or counted.
    pub tokens: i64,
    pub speech_engine_id: String,
    pub llm_model: String,
    pub rates: StageRates,
}

impl ProcessingEstimator {
    /// Tokens of transcript one second of audio yields, before the
    /// transcript exists.
    pub const GUESSED_TOKENS_PER_AUDIO_SECOND: f64 = 4.0;
    /// UTF-8 bytes per token for the transcript count.
    pub const BYTES_PER_TOKEN: f64 = 3.0;

    /// `tokens` `None` guesses from `duration`.
    #[must_use]
    pub fn new(
        duration: f64,
        lanes: Vec<AudioLane>,
        tokens: Option<i64>,
        speech_engine_id: &str,
        llm_model: &str,
        rates: StageRates,
    ) -> Self {
        ProcessingEstimator {
            duration,
            lanes,
            tokens: tokens.unwrap_or_else(|| Self::guessed_tokens(duration)),
            speech_engine_id: speech_engine_id.to_owned(),
            llm_model: llm_model.to_owned(),
            rates,
        }
    }

    #[must_use]
    pub fn guessed_tokens(duration: f64) -> i64 {
        // Rounded up and bounded by the audio length; exact as an integer.
        #[allow(clippy::cast_possible_truncation)]
        let tokens = (duration.max(0.0) * Self::GUESSED_TOKENS_PER_AUDIO_SECOND).ceil() as i64;
        tokens
    }

    /// The transcript's token count as the rates are learned in.
    #[must_use]
    pub fn token_count(segments: &[TranscriptSegment]) -> i64 {
        let bytes: usize = segments.iter().map(|segment| segment.text.len()).sum();
        // A byte count divided by three, rounded up: fits comfortably.
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        let tokens = (bytes as f64 / Self::BYTES_PER_TOKEN).ceil() as i64;
        tokens
    }

    /// The `cleanup` and `summarize` key for a settings value: the model,
    /// or [`StageRates::NO_MODEL`] while none is configured.
    #[must_use]
    pub fn llm_model_key(settings: &Settings) -> String {
        match &settings.llm_model {
            Some(model) if !model.is_empty() => model.clone(),
            _ => StageRates::NO_MODEL.to_owned(),
        }
    }

    /// The rate key for `stage` in this run.
    #[must_use]
    pub fn key(&self, stage: PipelineStage) -> String {
        match stage.rate_keying() {
            RateKeying::SpeechEngine => self.speech_engine_id.clone(),
            RateKeying::LlmModel => self.llm_model.clone(),
            RateKeying::None => StageRates::UNKEYED.to_owned(),
        }
    }

    #[must_use]
    pub fn rate(&self, stage: PipelineStage) -> StageRate {
        self.rates.rate(stage, &self.key(stage))
    }

    /// Units of the stage's cost driver for this meeting.
    #[must_use]
    pub fn units(&self, stage: PipelineStage) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        match stage.cost_driver() {
            CostDriver::AudioSecondsAllLanes => self.duration * self.lanes.len() as f64,
            CostDriver::AudioSecondsOneLane => self.duration,
            CostDriver::ThousandTokens => self.tokens as f64 / 1000.0,
            CostDriver::Flat => 1.0,
        }
    }

    /// Expected seconds for the whole stage.
    #[must_use]
    pub fn expected_seconds(&self, stage: PipelineStage) -> f64 {
        self.rate(stage).seconds_per_unit * self.units(stage)
    }

    /// Expected seconds until the next event: one lane inside
    /// `transcribe`, the whole stage otherwise.
    #[must_use]
    pub fn expected_step(&self, stage: PipelineStage) -> f64 {
        if stage != PipelineStage::Transcribe || self.lanes.is_empty() {
            return self.expected_seconds(stage);
        }
        self.rate(stage).seconds_per_unit * self.duration
    }

    /// Expected seconds from the start of `stage` (its lane `lane`, zero
    /// based, inside `transcribe`) to the end of `stages`.
    #[must_use]
    pub fn expected_remaining(
        &self,
        stage: PipelineStage,
        lane: usize,
        stages: &[PipelineStage],
    ) -> f64 {
        let later: f64 = stages
            .iter()
            .skip_while(|candidate| **candidate != stage)
            .map(|candidate| self.expected_seconds(*candidate))
            .sum();
        #[allow(clippy::cast_precision_loss)]
        let done = lane as f64 * self.expected_step(stage);
        (later - done).max(0.0)
    }

    /// True while any learned stage in `stages` still runs on a seed.
    #[must_use]
    pub fn is_seeded(&self, stages: &[PipelineStage]) -> bool {
        stages
            .iter()
            .any(|stage| stage.learns_rate() && self.rate(*stage).samples == 0)
    }

    /// The honest numbers at an event, before the run's monotonic clamp.
    #[must_use]
    pub fn progress(
        &self,
        stage: PipelineStage,
        lane: usize,
        elapsed: f64,
        stages: &[PipelineStage],
    ) -> ProcessingProgress {
        let remaining = self.expected_remaining(stage, lane, stages);
        let total = elapsed + remaining;
        let fraction = if total > 0.0 { elapsed / total } else { 0.0 };
        let next = if total > 0.0 {
            ((elapsed + self.expected_step(stage)) / total).min(1.0)
        } else {
            1.0
        };
        let transcribe = stage == PipelineStage::Transcribe;
        ProcessingProgress {
            stage,
            fraction,
            next_fraction: next.max(fraction),
            estimated_remaining_seconds: remaining,
            is_estimate_seeded: self.is_seeded(stages),
            lane: if transcribe { lane } else { 0 },
            lane_count: if transcribe {
                self.lanes.len().max(1)
            } else {
                1
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_cover_every_stage_and_fall_back_for_unknown_engines() {
        let rates = StageRates::seeds();
        for stage in PipelineStage::ALL {
            assert!(
                rates.rate(*stage, "whatever").seconds_per_unit > 0.0,
                "{stage:?}"
            );
        }
        assert_eq!(
            rates.rate(PipelineStage::Transcribe, "fake-engine"),
            rates.rate(PipelineStage::Transcribe, StageRates::FALLBACK_ENGINE)
        );
    }

    /// The numbers of `StageRates.seeds` in `ProcessingEstimator.swift`,
    /// every one a seed (`samples == 0`).
    #[test]
    fn the_seeds_are_the_swift_constants() {
        let rates = StageRates::seeds();
        let seed = |stage, key| {
            let rate = rates.rate(stage, key);
            assert_eq!(rate.samples, 0, "{stage:?} {key}");
            rate.seconds_per_unit
        };
        assert_eq!(seed(PipelineStage::Decode, ""), 0.005);
        assert_eq!(seed(PipelineStage::Transcribe, "parakeet-v3"), 1.0 / 90.0);
        assert_eq!(
            seed(PipelineStage::Transcribe, "parakeet-ultra"),
            1.0 / 90.0
        );
        assert_eq!(seed(PipelineStage::Transcribe, "parakeet-de"), 1.0 / 90.0);
        assert_eq!(
            seed(PipelineStage::Transcribe, "whisperkit-large-v3-turbo"),
            1.0 / 6.0
        );
        assert_eq!(seed(PipelineStage::Diarize, ""), 1.0 / 60.0);
        assert_eq!(seed(PipelineStage::MatchSpeakers, ""), 0.05);
        assert_eq!(seed(PipelineStage::Merge, ""), 0.1);
        assert_eq!(seed(PipelineStage::Cleanup, ""), 12.0);
        assert_eq!(seed(PipelineStage::Summarize, ""), 3.0);
        assert_eq!(seed(PipelineStage::Persist, ""), 0.005);
        assert_eq!(seed(PipelineStage::Deliver, ""), 0.5);
        assert_eq!(seed(PipelineStage::Retention, ""), 0.05);
    }

    #[test]
    fn the_first_sample_replaces_the_seed_and_later_ones_average() {
        let first = absorbing(seed(1.0), 4.0);
        assert_eq!(
            first,
            StageRate {
                seconds_per_unit: 4.0,
                samples: 1
            }
        );
        let second = absorbing(first, 2.0);
        assert_eq!(second.samples, 2);
        assert!((second.seconds_per_unit - (0.3 * 2.0 + 0.7 * 4.0)).abs() < 1e-9);
    }

    #[test]
    fn progress_is_elapsed_over_total_and_steps_per_lane() {
        let estimator = ProcessingEstimator::new(
            60.0,
            vec![AudioLane::Mic, AudioLane::System],
            None,
            "parakeet-v3",
            StageRates::NO_MODEL,
            StageRates::seeds(),
        );
        let first = estimator.progress(PipelineStage::Decode, 0, 0.0, PipelineStage::ALL);
        assert_eq!(first.fraction, 0.0);
        assert!(first.is_estimate_seeded);
        assert_eq!(first.lane_count, 1);
        let lane = estimator.progress(PipelineStage::Transcribe, 1, 10.0, PipelineStage::ALL);
        assert_eq!((lane.lane, lane.lane_count), (1, 2));
        assert!(lane.fraction > 0.0 && lane.next_fraction >= lane.fraction);
        assert!(lane.estimated_remaining_seconds < first.estimated_remaining_seconds);
    }
}
