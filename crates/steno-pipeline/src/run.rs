//! One run's progress state. Swift: `Sources/StenoCore/Pipeline/ProcessingRun.swift`.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use steno_core::{PipelineStage, ProcessingProgress};

use crate::estimator::{ProcessingEstimator, StageCost as _, StageSample};

/// One run's progress state: the estimator with its rates and token count,
/// the stages the run posts, the run's start on the monotonic clock, the
/// last posted event and the stage timings.
#[derive(Debug, Clone)]
pub struct ProcessingRun {
    pub estimator: ProcessingEstimator,
    /// The stages this run posts, in order: every stage for `process`,
    /// `[Summarize, Deliver]` for a summary re-run, `[Deliver]` for a
    /// re-export.
    pub stages: Vec<PipelineStage>,
    /// The clock reading when the run started.
    pub started_at: f64,
    last: Option<ProcessingProgress>,
    measured: BTreeMap<PipelineStage, f64>,
    shared: BTreeSet<PipelineStage>,
}

impl ProcessingRun {
    #[must_use]
    pub fn new(
        estimator: ProcessingEstimator,
        stages: Vec<PipelineStage>,
        started_at: f64,
    ) -> Self {
        ProcessingRun {
            estimator,
            stages,
            started_at,
            last: None,
            measured: BTreeMap::new(),
            shared: BTreeSet::new(),
        }
    }

    /// The event for `stage` (lane `lane` inside transcribe) at `elapsed`
    /// seconds, clamped: the fraction never falls below the previous
    /// event's `next_fraction`, and `next_fraction` is rescaled with it so
    /// the honest step keeps its share of the remaining time.
    pub fn progress(
        &mut self,
        stage: PipelineStage,
        lane: usize,
        elapsed: f64,
    ) -> ProcessingProgress {
        let honest = self.estimator.progress(stage, lane, elapsed, &self.stages);
        let floor = self.last.as_ref().map_or(0.0, |last| last.next_fraction);
        if honest.fraction >= floor {
            self.last = Some(honest.clone());
            return honest;
        }
        let mut event = honest.clone();
        event.fraction = floor;
        let share = if honest.fraction < 1.0 {
            (honest.next_fraction - honest.fraction) / (1.0 - honest.fraction)
        } else {
            0.0
        };
        event.next_fraction = (floor + share * (1.0 - floor)).min(1.0);
        self.last = Some(event.clone());
        event
    }

    /// Adds a measured span of `stage` (lane `lane` inside transcribe);
    /// `alone` false marks the stage shared. Returns the rate sample once
    /// the stage is complete (its last lane for transcribe), or `None` when
    /// it was shared, is never learned, had no units of work or measured a
    /// span that is not a finite number.
    pub fn measure(
        &mut self,
        stage: PipelineStage,
        lane: usize,
        seconds: f64,
        alone: bool,
        recorded_at: DateTime<Utc>,
    ) -> Option<StageSample> {
        let total = self.measured.entry(stage).or_insert(0.0);
        *total += seconds;
        let total = *total;
        if !alone {
            self.shared.insert(stage);
        }
        let last_lane = self.estimator.lanes.len().saturating_sub(1);
        if (stage == PipelineStage::Transcribe && lane != last_lane)
            || !stage.learns_rate()
            || self.shared.contains(&stage)
        {
            return None;
        }
        let units = self.estimator.units(stage);
        if units <= 0.0 || !total.is_finite() {
            return None;
        }
        Some(StageSample {
            stage,
            key: self.estimator.key(stage),
            seconds_per_unit: total / units,
            recorded_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::estimator::StageRates;
    use steno_core::AudioLane;

    fn run() -> ProcessingRun {
        ProcessingRun::new(
            ProcessingEstimator::new(
                10.0,
                vec![AudioLane::Mixed],
                None,
                "fake",
                StageRates::NO_MODEL,
                StageRates::seeds(),
            ),
            PipelineStage::ALL.to_vec(),
            0.0,
        )
    }

    #[test]
    fn the_bar_never_moves_backwards() {
        let mut run = run();
        let first = run.progress(PipelineStage::Decode, 0, 0.0);
        let far = run.progress(PipelineStage::Summarize, 0, 1000.0);
        assert!(far.fraction >= first.next_fraction);
        let early = run.progress(PipelineStage::Persist, 0, 1000.0);
        assert!(early.fraction >= far.next_fraction);
        assert!(early.next_fraction >= early.fraction && early.next_fraction <= 1.0);
    }

    #[test]
    fn samples_are_recorded_only_when_the_run_was_alone() {
        let mut run = run();
        let now = Utc::now();
        assert!(
            run.measure(PipelineStage::Decode, 0, 1.0, true, now)
                .is_none()
        );
        assert!(
            run.measure(PipelineStage::Merge, 0, 0.5, false, now)
                .is_none()
        );
        let sample = run
            .measure(PipelineStage::Diarize, 0, 2.0, true, now)
            .unwrap();
        assert_eq!(sample.key, "");
        assert!((sample.seconds_per_unit - 0.2).abs() < 1e-9);
        let keyed = run
            .measure(PipelineStage::Transcribe, 0, 5.0, true, now)
            .unwrap();
        assert_eq!(keyed.key, "fake");
    }
}
