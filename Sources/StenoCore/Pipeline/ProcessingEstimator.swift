import Foundation

/// Seconds of work per unit of a stage's cost driver, and how many runs it
/// was learned from. `samples == 0` marks a seed.
public struct StageRate: Sendable, Equatable, Hashable {
  public var secondsPerUnit: Double
  public var samples: Int

  public init(secondsPerUnit: Double, samples: Int) {
    self.secondsPerUnit = secondsPerUnit
    self.samples = samples
  }

  /// A seed: the constant until the first measurement replaces it.
  static func seed(_ secondsPerUnit: Double) -> StageRate {
    StageRate(secondsPerUnit: secondsPerUnit, samples: 0)
  }

  /// Weight of the newest sample in the moving average.
  public static let alpha = 0.3

  /// The rate after one more measurement: the first sample replaces a seed
  /// outright, so the second run already runs on this Mac's numbers; later
  /// samples are folded in as an exponential moving average with `alpha`.
  public func absorbing(_ secondsPerUnit: Double) -> StageRate {
    guard samples > 0 else { return StageRate(secondsPerUnit: secondsPerUnit, samples: 1) }
    return StageRate(
      secondsPerUnit: Self.alpha * secondsPerUnit + (1 - Self.alpha) * self.secondsPerUnit,
      samples: samples + 1)
  }
}

/// One measured stage: what `ProcessingPipeline` hands `MeetingStore.record`
/// once a stage ran alone in flight and did not throw.
public struct StageSample: Sendable, Equatable, Hashable {
  public var stage: PipelineStage
  /// The speech engine id for `transcribe`, the LLM model for `cleanup` and
  /// `summarize`, `StageRates.unkeyed` for every other stage.
  public var key: String
  public var secondsPerUnit: Double
  public var recordedAt: Date

  public init(stage: PipelineStage, key: String, secondsPerUnit: Double, recordedAt: Date) {
    self.stage = stage
    self.key = key
    self.secondsPerUnit = secondsPerUnit
    self.recordedAt = recordedAt
  }
}

/// What each stage's cost depends on, said once: `units`, `key`,
/// `isKeyed`, `seedKey` and `isLearned` read this table.
extension PipelineStage {
  /// The unit a stage's rate is learned per.
  enum CostDriver: Sendable {
    /// Audio seconds summed over the lanes decoded, transcribed or encoded.
    case audioSecondsAllLanes
    /// Audio seconds of the one diarized lane.
    case audioSecondsOneLane
    /// Thousands of transcript tokens, guessed from the duration until the
    /// transcript exists.
    case thousandTokens
    /// One unit: rows and files, independent of the meeting.
    case flat
  }

  /// What a stage's rate is keyed by beside the stage itself.
  enum RateKeying: Sendable {
    case none
    case speechEngine
    case llmModel
  }

  var costDriver: CostDriver {
    switch self {
    case .decode, .transcribe, .persist: .audioSecondsAllLanes
    case .diarize: .audioSecondsOneLane
    case .cleanup, .summarize: .thousandTokens
    case .matchSpeakers, .merge, .deliver, .retention: .flat
    }
  }

  var rateKeying: RateKeying {
    switch self {
    case .transcribe: .speechEngine
    case .cleanup, .summarize: .llmModel
    case .decode, .diarize, .matchSpeakers, .merge, .persist, .deliver, .retention: .none
    }
  }

  /// Whether the pipeline learns the stage's rate. Decode is never learned:
  /// the CLI and the app decode through different decoders and a shared
  /// rate would mix them, so it stays on its flat seed.
  var isLearned: Bool { self != .decode }
}

/// The per-stage rates the estimator weighs stages with, keyed by stage and
/// by what the stage depends on. `seeds` is the one source of the seed
/// constants; a key without a sample falls back to its seed, and a
/// transcribe key without a seed (a fake engine) to the `parakeet-v3` seed.
public struct StageRates: Sendable, Equatable, Hashable {
  public struct Key: Sendable, Equatable, Hashable {
    public var stage: PipelineStage
    /// The speech engine id, the LLM model, or `StageRates.unkeyed`.
    public var dependency: String

    public init(_ stage: PipelineStage, _ dependency: String) {
      self.stage = stage
      self.dependency = dependency
    }
  }

  /// The key of a stage whose cost depends on no engine or model.
  public static let unkeyed = ""
  /// The key of `cleanup` and `summarize` when no LLM model is configured
  /// and the passthrough passes run.
  public static let noModel = "none"
  /// The transcribe seed an engine without a seed of its own uses.
  public static let fallbackEngine = "parakeet-v3"

  public private(set) var entries: [Key: StageRate]

  public init(entries: [Key: StageRate] = [:]) {
    self.entries = entries
  }

  /// Seconds per unit before any run was measured on this Mac, in the
  /// units of `PipelineStage.costDriver`. Basis and provenance:
  /// `.plans/2026-09-28-processing-progress.md` D2; `steno process` prints
  /// the lines that replace these.
  public static let seeds = StageRates(entries: [
    Key(.decode, unkeyed): .seed(0.005),
    Key(.transcribe, "parakeet-v3"): .seed(1.0 / 90),
    Key(.transcribe, "parakeet-ultra"): .seed(1.0 / 90),
    Key(.transcribe, "parakeet-de"): .seed(1.0 / 90),
    Key(.transcribe, "whisperkit-large-v3-turbo"): .seed(1.0 / 6),
    Key(.diarize, unkeyed): .seed(1.0 / 60),
    Key(.matchSpeakers, unkeyed): .seed(0.05),
    Key(.merge, unkeyed): .seed(0.1),
    Key(.cleanup, unkeyed): .seed(12),
    Key(.summarize, unkeyed): .seed(3),
    Key(.persist, unkeyed): .seed(0.005),
    Key(.deliver, unkeyed): .seed(0.5),
    Key(.retention, unkeyed): .seed(0.05),
  ])

  /// Whether a stage's rate is keyed by an engine or a model; every other
  /// stage has one rate under `unkeyed`, and a row with another key is
  /// ignored on load.
  public static func isKeyed(_ stage: PipelineStage) -> Bool {
    stage.rateKeying != .none
  }

  /// The seed key a key without a seed of its own falls back to.
  static func seedKey(_ stage: PipelineStage) -> String {
    stage.rateKeying == .speechEngine ? fallbackEngine : unkeyed
  }

  /// The learned rate for `stage` under `key`, else its seed, else the
  /// stage's fallback seed.
  public func rate(_ stage: PipelineStage, key: String) -> StageRate {
    entries[Key(stage, key)]
      ?? Self.seeds.entries[Key(stage, key)]
      ?? Self.seeds.entries[Key(stage, Self.seedKey(stage))]
      ?? .seed(0)
  }

  /// Sets a rate outright; `MeetingStore.stageRates()` uses it for the rows
  /// and `MeetingStore.record` folds a sample into its one row.
  public mutating func set(_ rate: StageRate, _ stage: PipelineStage, key: String) {
    entries[Key(stage, key)] = rate
  }
}

/// Pure arithmetic behind `progress` events: from a meeting's duration, its
/// lanes, a transcript token count and `StageRates`, the expected seconds
/// per stage, the expected remaining time from any point in the run and the
/// fraction at which the next event is expected. Until the transcript
/// exists the token count is a guess from the audio duration; the pipeline
/// replaces it at cleanup start.
public struct ProcessingEstimator: Sendable {
  /// Tokens of transcript one second of audio yields, before the transcript
  /// exists: around 140 words a minute at 1.5 to 2 tokens a word, labels
  /// and timestamps included.
  public static let guessedTokensPerAudioSecond = 4.0
  /// UTF-8 bytes per token for the transcript count. Deliberately the same
  /// rough rule as StenoLLM's budget for German and mixed text; the LLM
  /// rates are learned in these units, so the count only has to agree with
  /// itself.
  public static let bytesPerToken = 3.0

  /// The meeting's audio in seconds.
  public var duration: TimeInterval
  public var lanes: [AudioLane]
  /// Transcript tokens, guessed or counted.
  public var tokens: Int
  public var speechEngineID: String
  public var llmModel: String
  public var rates: StageRates

  /// `tokens` nil guesses from `duration`.
  public init(
    duration: TimeInterval,
    lanes: [AudioLane],
    tokens: Int? = nil,
    speechEngineID: String,
    llmModel: String,
    rates: StageRates
  ) {
    self.duration = duration
    self.lanes = lanes
    self.tokens = tokens ?? Self.guessedTokens(duration: duration)
    self.speechEngineID = speechEngineID
    self.llmModel = llmModel
    self.rates = rates
  }

  public static func guessedTokens(duration: TimeInterval) -> Int {
    Int((max(0, duration) * guessedTokensPerAudioSecond).rounded(.up))
  }

  /// The transcript's token count as the rates are learned in.
  public static func tokenCount(_ segments: [TranscriptSegment]) -> Int {
    let bytes = segments.reduce(0) { $0 + $1.text.utf8.count }
    return Int((Double(bytes) / bytesPerToken).rounded(.up))
  }

  /// The `cleanup` and `summarize` key for a settings value: the model, or
  /// `StageRates.noModel` while none is configured and the passthrough
  /// passes run.
  public static func llmModelKey(_ settings: Settings) -> String {
    guard let model = settings.llmModel, !model.isEmpty else { return StageRates.noModel }
    return model
  }

  /// Whether the pipeline learns a stage's rate; `PipelineStage.isLearned`.
  public static func learns(_ stage: PipelineStage) -> Bool {
    stage.isLearned
  }

  /// The rate key for `stage` in this run.
  public func key(_ stage: PipelineStage) -> String {
    switch stage.rateKeying {
    case .speechEngine: speechEngineID
    case .llmModel: llmModel
    case .none: StageRates.unkeyed
    }
  }

  public func rate(_ stage: PipelineStage) -> StageRate {
    rates.rate(stage, key: key(stage))
  }

  /// Units of the stage's cost driver for this meeting.
  public func units(_ stage: PipelineStage) -> Double {
    switch stage.costDriver {
    case .audioSecondsAllLanes: duration * Double(lanes.count)
    case .audioSecondsOneLane: duration
    case .thousandTokens: Double(tokens) / 1000
    case .flat: 1
    }
  }

  /// Expected seconds for the whole stage.
  public func expectedSeconds(_ stage: PipelineStage) -> Double {
    rate(stage).secondsPerUnit * units(stage)
  }

  /// Expected seconds until the next event: one lane inside `transcribe`,
  /// the whole stage otherwise.
  public func expectedStep(_ stage: PipelineStage) -> Double {
    guard stage == .transcribe, !lanes.isEmpty else { return expectedSeconds(stage) }
    return rate(stage).secondsPerUnit * duration
  }

  /// Expected seconds from the start of `stage` (its lane `lane`, zero
  /// based, inside `transcribe`) to the end of `stages`.
  public func expectedRemaining(
    from stage: PipelineStage, lane: Int = 0, in stages: [PipelineStage] = PipelineStage.allCases
  ) -> Double {
    let later = stages.drop { $0 != stage }.reduce(0) { $0 + expectedSeconds($1) }
    return max(0, later - Double(lane) * expectedStep(stage))
  }

  /// True while any learned stage in `stages` still runs on a seed.
  public func isSeeded(_ stages: [PipelineStage] = PipelineStage.allCases) -> Bool {
    stages.contains { $0.isLearned && rate($0).samples == 0 }
  }

  /// The honest numbers at an event: elapsed over elapsed plus remaining,
  /// the next event's fraction and the remaining time, before the run's
  /// monotonic clamp. When nothing has elapsed and nothing is expected to
  /// remain (every rate learned at zero) the fraction is 0 and the next
  /// event is expected at the end.
  public func progress(
    _ stage: PipelineStage, lane: Int = 0, elapsed: TimeInterval,
    in stages: [PipelineStage] = PipelineStage.allCases
  ) -> ProcessingProgress {
    let remaining = expectedRemaining(from: stage, lane: lane, in: stages)
    let total = elapsed + remaining
    let fraction = total > 0 ? elapsed / total : 0
    let next = total > 0 ? min(1, (elapsed + expectedStep(stage)) / total) : 1
    return ProcessingProgress(
      stage: stage,
      fraction: fraction,
      nextFraction: max(fraction, next),
      estimatedRemaining: .seconds(remaining),
      isEstimateSeeded: isSeeded(stages),
      lane: stage == .transcribe ? lane : 0,
      laneCount: stage == .transcribe ? max(1, lanes.count) : 1)
  }
}
