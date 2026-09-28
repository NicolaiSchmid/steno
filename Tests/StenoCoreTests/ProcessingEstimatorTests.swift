import Foundation
import Testing

@testable import StenoCore

/// Pure arithmetic: a 3600 s two-lane meeting on the seeds, no pipeline.
@Suite struct ProcessingEstimatorTests {
  /// `SpeechEngineID` lives in StenoSpeech, out of core's reach, so the ids
  /// are spelled out here; `StageRateSeedTests` in StenoSpeechTests walks
  /// `allCases`.
  static let engineIDs = [
    "parakeet-v3", "parakeet-ultra", "parakeet-de", "whisperkit-large-v3-turbo",
  ]

  static func estimator(rates: StageRates = .seeds, tokens: Int? = nil) -> ProcessingEstimator {
    ProcessingEstimator(
      duration: 3600, lanes: [.mic, .system], tokens: tokens, speechEngineID: "parakeet-v3",
      llmModel: "local-model", rates: rates)
  }

  @Test func everyStageAndEveryEngineHasAPositiveSeed() {
    for stage in PipelineStage.allCases {
      let seed = StageRates.seeds.rate(stage, key: StageRates.seedKey(stage))
      #expect(seed.secondsPerUnit > 0, "\(stage)")
      #expect(seed.samples == 0, "\(stage)")
    }
    for id in Self.engineIDs {
      #expect(
        StageRates.seeds.entries[StageRates.Key(.transcribe, id)] != nil, Comment(rawValue: id))
    }
    for (key, rate) in StageRates.seeds.entries {
      #expect(rate.secondsPerUnit > 0, "\(key)")
      #expect(rate.samples == 0, "\(key)")
    }
    for stage in PipelineStage.allCases where stage.rateKeying == .none {
      #expect(
        StageRates.seeds.entries[StageRates.Key(stage, StageRates.unkeyed)] != nil,
        "an unkeyed stage is seeded under the empty key: \(stage)")
      #expect(!StageRates.isKeyed(stage))
    }
    #expect(Self.estimator().isSeeded())
  }

  @Test func remainingAtTranscribeStartIsTheSumOfTheLaterShares() {
    let estimator = Self.estimator()
    // Spelled out rather than folded over `allCases`, so the sum is not the
    // implementation's own expression.
    let later =
      estimator.expectedSeconds(.transcribe) + estimator.expectedSeconds(.diarize)
      + estimator.expectedSeconds(.matchSpeakers) + estimator.expectedSeconds(.merge)
      + estimator.expectedSeconds(.cleanup) + estimator.expectedSeconds(.summarize)
      + estimator.expectedSeconds(.persist) + estimator.expectedSeconds(.deliver)
      + estimator.expectedSeconds(.retention)
    #expect(estimator.expectedRemaining(from: .transcribe) == later)
    #expect(
      estimator.expectedRemaining(from: .transcribe, lane: 1)
        == later - estimator.expectedStep(.transcribe), "lane two starts at the lane boundary")
    #expect(
      estimator.expectedRemaining(from: .decode)
        == PipelineStage.allCases.reduce(0) { $0 + estimator.expectedSeconds($1) })
    #expect(estimator.expectedRemaining(from: .retention) == estimator.expectedSeconds(.retention))
    #expect(estimator.expectedStep(.transcribe) * 2 == estimator.expectedSeconds(.transcribe))
    #expect(estimator.expectedSeconds(.transcribe) == 7200.0 / 90, "two lanes at RTFx 90")
    #expect(estimator.expectedSeconds(.diarize) == 3600.0 / 60, "one lane at RTFx 60")
    #expect(estimator.tokens == 14_400, "four tokens per audio second until the transcript exists")
    #expect(estimator.units(.cleanup) == 14.4)
    #expect(estimator.units(.merge) == 1)
    #expect(estimator.units(.persist) == 7200, "persist encodes every lane, so per audio second")
    #expect(estimator.expectedSeconds(.persist) == 36, "0.005 s per audio second over two lanes")
  }

  @Test func oneSampleOfEveryStageMakesTheSecondEstimateExact() {
    // Durations that divide the units without rounding, so the equality
    // below is exact rather than within an ulp; 16 000 tokens for the same
    // reason.
    let durations: [PipelineStage: Double] = [
      .decode: 14.0625, .transcribe: 112.5, .diarize: 225, .matchSpeakers: 0.5, .merge: 0.25,
      .cleanup: 192, .summarize: 48, .persist: 1.7578125, .deliver: 1, .retention: 0.125,
    ]
    let first = Self.estimator(tokens: 16_000)
    var rates = StageRates.seeds
    for stage in PipelineStage.allCases {
      rates.set(
        first.rate(stage).absorbing(durations[stage]! / first.units(stage)), stage,
        key: first.key(stage))
    }
    let second = Self.estimator(rates: rates, tokens: 16_000)
    for stage in PipelineStage.allCases {
      #expect(second.expectedSeconds(stage) == durations[stage]!, "\(stage)")
      #expect(second.rate(stage).samples == 1, "\(stage)")
    }
    #expect(!second.isSeeded())
    #expect(second.expectedRemaining(from: .decode) == durations.values.reduce(0, +))
    #expect(second.key(.transcribe) == "parakeet-v3")
    #expect(second.key(.cleanup) == "local-model" && second.key(.summarize) == "local-model")
    #expect(second.key(.diarize) == StageRates.unkeyed)
  }

  @Test func theFirstSampleReplacesTheSeedAndLaterOnesAverage() {
    let seed = StageRates.seeds.rate(.diarize, key: StageRates.unkeyed)
    let first = seed.absorbing(2)
    #expect(first == StageRate(secondsPerUnit: 2, samples: 1))
    let second = first.absorbing(4)
    #expect(second.samples == 2)
    #expect(abs(second.secondsPerUnit - 2.6) < 1e-12, "0.3 * 4 + 0.7 * 2")
    var rates = StageRates.seeds
    rates.set(seed.absorbing(2), .diarize, key: StageRates.unkeyed)
    #expect(rates.rate(.diarize, key: StageRates.unkeyed) == first)
    #expect(
      rates.rate(.merge, key: StageRates.unkeyed).samples == 0, "other stages keep their seed")
  }

  @Test func fractionNeverDropsWhenTheTokenCountIsRevised() {
    for factor in [10.0, 0.1] {
      var run = ProcessingRun(
        estimator: Self.estimator(), stages: PipelineStage.allCases,
        stopwatch: Stopwatch(ManualClock()))
      var elapsed = 0.0
      var previous: ProcessingProgress?
      var merge: ProcessingProgress?
      var cleanup: ProcessingProgress?
      for stage in PipelineStage.allCases {
        if stage == .cleanup {
          run.estimator.tokens = Int(Double(run.estimator.tokens) * factor)
        }
        let lanes = stage == .transcribe ? run.estimator.lanes.count : 1
        for lane in 0..<lanes {
          // Every step lands on schedule, so the honest fraction alone would
          // drop at cleanup when the token count grows tenfold.
          let event = run.progress(stage, lane: lane, elapsed: .seconds(elapsed))
          #expect(event.stage == stage)
          #expect(event.fraction >= (previous?.fraction ?? 0), "\(stage) x\(factor)")
          #expect(event.nextFraction >= event.fraction, "\(stage) x\(factor)")
          #expect(event.nextFraction <= 1, "\(stage) x\(factor)")
          #expect(event.isEstimateSeeded)
          if stage == .merge { merge = event }
          if stage == .cleanup { cleanup = event }
          previous = event
          elapsed += run.estimator.expectedStep(stage)
        }
      }
      #expect(cleanup!.fraction >= merge!.fraction, "x\(factor)")
      #expect(cleanup!.fraction >= merge!.nextFraction, "the bar rests where merge said it would")
      #expect(previous?.stage == .retention)
      #expect(previous?.nextFraction == 1, "retention ends the run")
      #expect(previous?.estimatedRemaining == .seconds(run.estimator.expectedSeconds(.retention)))
      if factor > 1 {
        #expect(
          cleanup!.estimatedRemaining > merge!.estimatedRemaining,
          "ten times the tokens grows the remaining time even though the fraction held")
      } else {
        #expect(cleanup!.estimatedRemaining < merge!.estimatedRemaining)
      }
    }
  }

  /// A stage that finishes faster than its rate predicted lifts the next
  /// event's fraction to the boundary the bar already reached, but the next
  /// window keeps its honest length: `expectedTimeToNextEvent` stays the
  /// step of the stage that is starting, so the bar neither freezes nor
  /// reads "a bit longer than usual" while the run is ahead.
  @Test func anEarlyStageLiftsTheBarWithoutShorteningTheNextWindow() {
    let estimator = Self.estimator()
    var run = ProcessingRun(
      estimator: estimator, stages: PipelineStage.allCases, stopwatch: Stopwatch(ManualClock()))
    let decode = run.progress(.decode, elapsed: .zero)
    // Decode ran three times faster than seeded.
    let early = estimator.expectedStep(.decode) / 3
    let transcribe = run.progress(.transcribe, lane: 0, elapsed: .seconds(early))
    #expect(transcribe.fraction == decode.nextFraction, "lifted to the boundary the bar reached")
    #expect(transcribe.nextFraction > transcribe.fraction)
    #expect(
      abs(
        transcribe.expectedTimeToNextEvent / .seconds(1) - estimator.expectedStep(.transcribe))
        < 1e-9, "the honest step survives the clamp")
    #expect(
      transcribe.estimatedRemaining == .seconds(estimator.expectedRemaining(from: .transcribe)))

    // Every stage a third of its expected time: every window keeps its
    // honest step and every stage with work ahead of it has a window.
    var elapsed = 0.0
    run = ProcessingRun(
      estimator: estimator, stages: PipelineStage.allCases, stopwatch: Stopwatch(ManualClock()))
    for stage in PipelineStage.allCases {
      let lanes = stage == .transcribe ? estimator.lanes.count : 1
      for lane in 0..<lanes {
        let event = run.progress(stage, lane: lane, elapsed: .seconds(elapsed))
        if estimator.expectedSeconds(stage) > 0 {
          #expect(event.nextFraction > event.fraction, "\(stage) lane \(lane)")
        }
        let window = event.expectedTimeToNextEvent / .seconds(1)
        let step = min(estimator.expectedStep(stage), event.estimatedRemaining / .seconds(1))
        #expect(abs(window - step) < 1e-9, "\(stage) lane \(lane): \(window) vs \(step)")
        #expect(event.lane == (stage == .transcribe ? lane : 0))
        #expect(event.laneCount == (stage == .transcribe ? 2 : 1))
        elapsed += estimator.expectedStep(stage) / 3
      }
    }
  }

  @Test func honestFractionsFollowTheClock() {
    let estimator = Self.estimator()
    let total = estimator.expectedRemaining(from: .decode)
    let start = estimator.progress(.decode, elapsed: 0)
    #expect(start.fraction == 0)
    #expect(start.nextFraction == estimator.expectedSeconds(.decode) / total)
    #expect(start.estimatedRemaining == .seconds(total))
    // Transcribe took twice as long as planned: the fraction moves with it.
    let planned = estimator.expectedSeconds(.decode) + estimator.expectedSeconds(.transcribe)
    let late = estimator.progress(.diarize, elapsed: planned * 2)
    let remaining = estimator.expectedRemaining(from: .diarize)
    #expect(late.fraction == planned * 2 / (planned * 2 + remaining))
    #expect(late.estimatedRemaining == .seconds(remaining))
    #expect(late.expectedTimeToNextEvent > .zero)
  }

  @Test func nothingLeftAndNothingElapsedReadsAsTheStart() {
    var zero = StageRates.seeds
    for stage in PipelineStage.allCases {
      zero.set(StageRate(secondsPerUnit: 0, samples: 1), stage, key: Self.estimator().key(stage))
    }
    let estimator = Self.estimator(rates: zero)
    let start = estimator.progress(.summarize, elapsed: 0, in: [.summarize, .deliver])
    #expect(start.fraction == 0)
    #expect(start.nextFraction == 1)
    #expect(start.estimatedRemaining == .zero)
    #expect(start.expectedTimeToNextEvent == .zero)
    let later = estimator.progress(.deliver, elapsed: 2, in: [.summarize, .deliver])
    #expect(later.fraction == 1, "time passed and nothing is expected to remain")
  }

  @Test func expectedTimeToNextEventIsTheShareOfTheRemaining() {
    let progress = ProcessingProgress(
      stage: .cleanup, fraction: 0.2, nextFraction: 0.4, estimatedRemaining: .seconds(80),
      isEstimateSeeded: false)
    #expect(progress.expectedTimeToNextEvent == .seconds(20), "(0.4 - 0.2) / 0.8 of 80 s")
    let done = ProcessingProgress(
      stage: .retention, fraction: 1, nextFraction: 1, estimatedRemaining: .zero,
      isEstimateSeeded: false)
    #expect(done.expectedTimeToNextEvent == .zero)
    let last = ProcessingProgress(
      stage: .retention, fraction: 0.9, nextFraction: 1, estimatedRemaining: .seconds(3),
      isEstimateSeeded: true)
    #expect(last.expectedTimeToNextEvent == .seconds(3), "the last step is all that remains")
  }

  @Test func anUnknownEngineUsesTheParakeetSeedWithZeroSamples() {
    let fake = ProcessingEstimator(
      duration: 3600, lanes: [.mixed], speechEngineID: "fake-engine",
      llmModel: StageRates.noModel,
      rates: .seeds)
    #expect(fake.rate(.transcribe) == StageRates.seeds.rate(.transcribe, key: "parakeet-v3"))
    #expect(fake.rate(.transcribe).samples == 0)
    #expect(
      fake.rate(.cleanup) == StageRates.seeds.rate(.cleanup, key: StageRates.unkeyed),
      "a model without a seed of its own uses the stage's seed")
    #expect(fake.isSeeded())
    var learned = StageRates.seeds
    learned.set(
      learned.rate(.transcribe, key: "fake-engine").absorbing(0.5), .transcribe, key: "fake-engine")
    let second = ProcessingEstimator(
      duration: 3600, lanes: [.mixed], speechEngineID: "fake-engine",
      llmModel: StageRates.noModel,
      rates: learned)
    #expect(second.rate(.transcribe) == StageRate(secondsPerUnit: 0.5, samples: 1))
    #expect(
      learned.rate(.transcribe, key: "parakeet-v3").samples == 0,
      "the fake's sample never touches the real engine's seed")
    #expect(ProcessingEstimator.llmModelKey(Settings()) == StageRates.noModel)
    #expect(ProcessingEstimator.llmModelKey(Settings(llmModel: "")) == StageRates.noModel)
    #expect(ProcessingEstimator.llmModelKey(Settings(llmModel: "qwen")) == "qwen")
    #expect(!ProcessingEstimator.learns(.decode) && ProcessingEstimator.learns(.transcribe))
    #expect(ProcessingEstimator.tokenCount(SampleData.segments()) > 0)
    #expect(ProcessingEstimator.tokenCount([]) == 0)
  }
}
