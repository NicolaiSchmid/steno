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
    #expect(Self.estimator().isSeeded())
  }

  @Test func remainingAtTranscribeStartIsTheSumOfTheLaterShares() {
    let estimator = Self.estimator()
    let later = PipelineStage.allCases.drop { $0 != .transcribe }
      .reduce(0) { $0 + estimator.expectedSeconds($1) }
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
  }

  @Test func oneSampleOfEveryStageMakesTheSecondEstimateExact() {
    // Durations that divide the units without rounding, so the equality
    // below is exact rather than within an ulp; 16 000 tokens for the same
    // reason.
    let durations: [PipelineStage: Double] = [
      .decode: 14.0625, .transcribe: 112.5, .diarize: 225, .matchSpeakers: 0.5, .merge: 0.25,
      .cleanup: 192, .summarize: 48, .persist: 2, .deliver: 1, .retention: 0.125,
    ]
    let first = Self.estimator(tokens: 16_000)
    var rates = StageRates.seeds
    for stage in PipelineStage.allCases {
      rates.record(
        StageSample(
          stage: stage, key: first.key(stage),
          secondsPerUnit: durations[stage]! / first.units(stage),
          recordedAt: SampleData.updatedAt))
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
    rates.record(
      StageSample(
        stage: .diarize, key: StageRates.unkeyed, secondsPerUnit: 2,
        recordedAt: SampleData.updatedAt))
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
      zero.record(
        StageSample(
          stage: stage, key: Self.estimator().key(stage), secondsPerUnit: 0,
          recordedAt: SampleData.updatedAt))
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
      llmModel: StageRates.fakeModel,
      rates: .seeds)
    #expect(fake.rate(.transcribe) == StageRates.seeds.rate(.transcribe, key: "parakeet-v3"))
    #expect(fake.rate(.transcribe).samples == 0)
    #expect(
      fake.rate(.cleanup) == StageRates.seeds.rate(.cleanup, key: StageRates.unkeyed),
      "a model without a seed of its own uses the stage's seed")
    #expect(fake.isSeeded())
    var learned = StageRates.seeds
    learned.record(
      StageSample(
        stage: .transcribe, key: "fake-engine", secondsPerUnit: 0.5,
        recordedAt: SampleData.updatedAt))
    let second = ProcessingEstimator(
      duration: 3600, lanes: [.mixed], speechEngineID: "fake-engine",
      llmModel: StageRates.fakeModel,
      rates: learned)
    #expect(second.rate(.transcribe) == StageRate(secondsPerUnit: 0.5, samples: 1))
    #expect(
      learned.rate(.transcribe, key: "parakeet-v3").samples == 0,
      "the fake's sample never touches the real engine's seed")
    #expect(ProcessingEstimator.llmModelKey(Settings()) == StageRates.fakeModel)
    #expect(ProcessingEstimator.llmModelKey(Settings(llmModel: "")) == StageRates.fakeModel)
    #expect(ProcessingEstimator.llmModelKey(Settings(llmModel: "qwen")) == "qwen")
    #expect(!ProcessingEstimator.learns(.decode) && ProcessingEstimator.learns(.transcribe))
    #expect(ProcessingEstimator.tokenCount(SampleData.segments()) > 0)
    #expect(ProcessingEstimator.tokenCount([]) == 0)
  }
}
