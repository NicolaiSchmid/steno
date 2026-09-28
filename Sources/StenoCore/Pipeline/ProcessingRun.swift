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

  /// The event for `stage` (lane `lane` inside transcribe) at `elapsed`,
  /// clamped: the fraction never falls below the previous event's
  /// `nextFraction`, so a re-estimate never moves the bar backwards and an
  /// early event lands where the presenter already is. `nextFraction` is
  /// rescaled with it so the honest step keeps its share of the remaining
  /// time: a stage that finished early lifts the bar but does not shorten
  /// the next window.
  mutating func progress(_ stage: PipelineStage, lane: Int = 0, elapsed: Duration)
    -> ProcessingProgress
  {
    let honest = estimator.progress(stage, lane: lane, elapsed: elapsed / .seconds(1), in: stages)
    let floor = last?.nextFraction ?? 0
    guard honest.fraction < floor else {
      last = honest
      return honest
    }
    var event = honest
    event.fraction = floor
    let share =
      honest.fraction < 1 ? (honest.nextFraction - honest.fraction) / (1 - honest.fraction) : 0
    event.nextFraction = min(1, floor + share * (1 - floor))
    last = event
    return event
  }

  /// Adds a measured span of `stage` (lane `lane` inside transcribe);
  /// `alone` false marks the stage shared. Returns the rate sample once the
  /// stage is complete, its last lane for transcribe, or nil when it was
  /// shared, is never learned, had no units of work or measured a span that
  /// is not a finite number.
  mutating func measure(
    _ stage: PipelineStage, lane: Int, seconds: Double, alone: Bool, recordedAt: Date
  ) -> StageSample? {
    let total = measured[stage, default: 0] + seconds
    measured[stage] = total
    if !alone { shared.insert(stage) }
    guard stage != .transcribe || lane == estimator.lanes.count - 1,
      ProcessingEstimator.learns(stage), !shared.contains(stage)
    else { return nil }
    let units = estimator.units(stage)
    guard units > 0, total.isFinite else { return nil }
    return StageSample(
      stage: stage, key: estimator.key(stage), secondsPerUnit: total / units,
      recordedAt: recordedAt)
  }
}
