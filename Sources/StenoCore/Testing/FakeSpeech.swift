import Foundation

/// Records what a fake was asked to do, from any task. Fakes expose one
/// named for what it records (`transcriptions`, `summaries`, `dispatches`).
public actor CallLog<Entry: Sendable> {
  public private(set) var entries: [Entry] = []

  public init() {}

  public func record(_ entry: Entry) {
    entries.append(entry)
  }

  public var count: Int { entries.count }
}

/// A `SpeechEngine` that emits one segment per `segmentSeconds` of audio,
/// tagged with `language`, text `"<prefix> segment <n>"`. Deterministic and
/// configurable; records every hint it was given.
public struct FakeSpeechEngine: SpeechEngine, Sendable {
  public struct TranscribeCall: Sendable, Equatable {
    public var duration: TimeInterval
    public var hint: Locale.Language?
  }

  public let id: String
  public let supportedLanguages: Set<Locale.Language>
  public var segmentSeconds: TimeInterval
  public var language: LanguageTag?
  public var textPrefix: String
  public var wordTimings: Bool
  public var failure: (any Error & Sendable)?
  public let transcriptions = CallLog<TranscribeCall>()
  public let preparations = CallLog<Bool>()

  public init(
    id: String = "fake-engine",
    segmentSeconds: TimeInterval = 1,
    language: LanguageTag? = "de",
    textPrefix: String = "fake",
    wordTimings: Bool = false,
    failure: (any Error & Sendable)? = nil
  ) {
    self.id = id
    self.supportedLanguages = [LanguageTag("de").language, LanguageTag("en").language]
    self.segmentSeconds = segmentSeconds
    self.language = language
    self.textPrefix = textPrefix
    self.wordTimings = wordTimings
    self.failure = failure
  }

  public func prepare() async throws {
    await preparations.record(true)
  }

  public func transcribe(_ audio: AudioBuffer16k, hint: Locale.Language?) async throws
    -> [RawSegment]
  {
    await transcriptions.record(TranscribeCall(duration: audio.duration, hint: hint))
    if let failure { throw failure }
    return Self.segments(
      duration: audio.duration, segmentSeconds: segmentSeconds, language: language,
      textPrefix: textPrefix, wordTimings: wordTimings)
  }

  public static func segments(
    duration: TimeInterval, segmentSeconds: TimeInterval, language: LanguageTag?,
    textPrefix: String, wordTimings: Bool = false
  ) -> [RawSegment] {
    guard duration > 0, segmentSeconds > 0 else { return [] }
    let count = Int((duration / segmentSeconds).rounded(.up))
    return (0..<count).map { index in
      let start = Double(index) * segmentSeconds
      let end = min(duration, start + segmentSeconds)
      let text = "\(textPrefix) segment \(index + 1)"
      return RawSegment(
        start: start, end: end, text: text, language: language,
        wordTimings: wordTimings
          ? text.split(separator: " ").enumerated().map { offset, word in
            let step = (end - start) / 3
            return WordTiming(
              word: String(word), start: start + Double(offset) * step,
              end: start + Double(offset + 1) * step)
          } : nil)
    }
  }
}

/// A `Diarizer` that hands out `clusterCount` speakers round-robin over
/// `turnSeconds` turns, with unit embeddings along successive axes and a
/// sample clip range of at most ten seconds from the cluster's first turn.
public struct FakeDiarizer: Diarizer, Sendable {
  public var clusterCount: Int
  public var turnSeconds: TimeInterval
  public var result: (@Sendable (AudioBuffer16k) -> DiarizationResult)?
  public var failure: (any Error & Sendable)?
  /// Runs before every `diarize`; tests use it to observe state mid-pipeline.
  public var onDiarize: (@Sendable () async -> Void)?
  public let diarizations = CallLog<TimeInterval>()

  public init(
    clusterCount: Int = 2, turnSeconds: TimeInterval = 1.5, failure: (any Error & Sendable)? = nil
  ) {
    self.clusterCount = clusterCount
    self.turnSeconds = turnSeconds
    self.failure = failure
  }

  /// A diarizer returning exactly `result` for every buffer.
  public init(result: @escaping @Sendable (AudioBuffer16k) -> DiarizationResult) {
    self.clusterCount = 0
    self.turnSeconds = 0
    self.result = result
  }

  public func prepare() async throws {}

  public func diarize(_ audio: AudioBuffer16k) async throws -> DiarizationResult {
    await diarizations.record(audio.duration)
    await onDiarize?()
    if let failure { throw failure }
    if let result { return result(audio) }
    return Self.roundRobin(
      duration: audio.duration, clusterCount: clusterCount, turnSeconds: turnSeconds)
  }

  public static func roundRobin(
    duration: TimeInterval, clusterCount: Int, turnSeconds: TimeInterval
  )
    -> DiarizationResult
  {
    guard clusterCount > 0, turnSeconds > 0, duration > 0 else {
      return DiarizationResult(clusters: [])
    }
    var ranges = [[ClosedRange<TimeInterval>]](repeating: [], count: clusterCount)
    var start = 0.0
    var index = 0
    while start < duration {
      let end = min(duration, start + turnSeconds)
      ranges[index % clusterCount].append(start...end)
      start += turnSeconds
      index += 1
    }
    return DiarizationResult(
      clusters: ranges.enumerated().compactMap { offset, turns in
        guard let first = turns.first else { return nil }
        let clip = first.lowerBound...min(first.upperBound, first.lowerBound + 10)
        return SpeakerCluster(
          label: "Speaker \(offset + 1)",
          ranges: turns,
          embedding: SampleData.embedding(axis: offset),
          clusterConfidence: Float(0.9 - 0.1 * Double(offset)),
          sampleClipRange: clip)
      })
  }
}
