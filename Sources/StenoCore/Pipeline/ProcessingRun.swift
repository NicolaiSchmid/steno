import Foundation

/// Elapsed time on a `Clock<Duration>` since the stopwatch was made. The
/// pipeline's clock is an existential, whose `Instant` cannot be named, so
/// the start instant is captured here once and only durations come out.
struct Stopwatch: Sendable {
  private let elapsedSinceStart: @Sendable () -> Duration

  init<C: Clock>(_ clock: C) where C.Duration == Duration {
    let start = clock.now
    elapsedSinceStart = { start.duration(to: clock.now) }
  }

  var elapsed: Duration { elapsedSinceStart() }
}

/// One run's progress state on the pipeline actor: the estimator with its
/// rates and token count, the stages the run posts, the run's start, the
/// last posted event and the stage timings. `process` creates it after the
/// rates load and both `prepare()` calls returned, `run` and `post` update
/// and post from it, `exclusively` removes it.
struct ProcessingRun: Sendable {
  var estimator: ProcessingEstimator
  /// The stages this run posts, in order: every stage for `process`,
  /// `[.summarize, .deliver]` for `rerunSummary`, `[.deliver]` for
  /// `redeliver`.
  let stages: [PipelineStage]
  let stopwatch: Stopwatch
  /// The last posted event; the next one never lands below the boundary it
  /// announced.
  private(set) var last: ProcessingProgress?
  /// Measured seconds per stage, summed over transcribe's lanes.
  private(set) var measured: [PipelineStage: Double] = [:]
  /// Stages during which another meeting was in flight; their timings are
  /// never recorded.
  private(set) var shared: Set<PipelineStage> = []

  init(estimator: ProcessingEstimator, stages: [PipelineStage], stopwatch: Stopwatch) {
    self.estimator = estimator
    self.stages = stages
    self.stopwatch = stopwatch
  }

  /// The event for `stage` (lane `lane` inside transcribe) at `elapsed`,
  /// clamped: the fraction never falls below the previous event's
  /// `nextFraction`, so a re-estimate never moves the bar backwards and an
  /// early event lands where the presenter already is; `nextFraction` stays
  /// within `fraction...1`.
  mutating func progress(_ stage: PipelineStage, lane: Int = 0, elapsed: Duration)
    -> ProcessingProgress
  {
    var event = estimator.progress(stage, lane: lane, elapsed: elapsed.timeInterval, in: stages)
    let floor = last?.nextFraction ?? 0
    event.fraction = max(event.fraction, floor)
    event.nextFraction = max(event.fraction, min(1, event.nextFraction))
    last = event
    return event
  }

  /// Adds a measured span of `stage`; `alone` false marks the stage shared.
  mutating func measure(_ stage: PipelineStage, seconds: Double, alone: Bool) {
    measured[stage, default: 0] += seconds
    if !alone { shared.insert(stage) }
  }

  /// The sample for a finished stage, or nil when it was shared, is never
  /// learned, or had no units of work.
  func sample(_ stage: PipelineStage, recordedAt: Date) -> StageSample? {
    guard ProcessingEstimator.learns(stage), !shared.contains(stage),
      let seconds = measured[stage]
    else { return nil }
    let units = estimator.units(stage)
    guard units > 0 else { return nil }
    return StageSample(
      stage: stage, key: estimator.key(stage), secondsPerUnit: seconds / units,
      recordedAt: recordedAt)
  }
}
