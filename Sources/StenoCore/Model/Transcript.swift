import Foundation

/// Which capture lane a segment or file belongs to.
public enum AudioLane: String, Codable, Sendable, Equatable, Hashable, CaseIterable,
  CodingKeyRepresentable
{
  /// The Mac microphone in a call: "me".
  case mic
  /// The process tap in a call: "them".
  case system
  /// One room lane, fully diarized; no segment is auto-assigned to "me".
  case mixed
}

/// One line of the transcript after merge and cleanup. `rawText` keeps the STT
/// output; `text` is the cleaned version shown and exported.
public struct TranscriptSegment: Codable, Sendable, Equatable, Hashable, Identifiable {
  public var id: UUID
  public var meetingID: UUID
  public var start: TimeInterval
  public var end: TimeInterval
  public var speakerID: UUID?
  public var lane: AudioLane
  public var text: String
  public var rawText: String

  public init(
    id: UUID,
    meetingID: UUID,
    start: TimeInterval,
    end: TimeInterval,
    speakerID: UUID? = nil,
    lane: AudioLane,
    text: String,
    rawText: String
  ) {
    self.id = id
    self.meetingID = meetingID
    self.start = start
    self.end = end
    self.speakerID = speakerID
    self.lane = lane
    self.text = text
    self.rawText = rawText
  }

  public var duration: TimeInterval { max(0, end - start) }
}

/// Mono Float32 audio at 16 kHz, the only format speech engines and diarizers
/// accept. Decoded one lane at a time so at most one buffer is alive.
public struct AudioBuffer16k: Sendable, Equatable {
  public static let sampleRate: Double = 16_000

  public var samples: [Float]

  public init(samples: [Float]) {
    self.samples = samples
  }

  public var duration: TimeInterval {
    Double(samples.count) / Self.sampleRate
  }

  /// The samples covering `range`, clamped to the buffer.
  public func slice(_ range: ClosedRange<TimeInterval>) -> AudioBuffer16k {
    let lower = max(0, Int((range.lowerBound * Self.sampleRate).rounded(.down)))
    let upper = min(samples.count, Int((range.upperBound * Self.sampleRate).rounded(.up)))
    guard lower < upper else { return AudioBuffer16k(samples: []) }
    return AudioBuffer16k(samples: Array(samples[lower..<upper]))
  }
}

/// What a `SpeechEngine` returns: untimed by speaker, tagged with the detected
/// language when the engine knows it.
public struct RawSegment: Codable, Sendable, Equatable, Hashable {
  public var start: TimeInterval
  public var end: TimeInterval
  public var text: String
  @LanguageTag public var language: Locale.Language?
  public var wordTimings: [WordTiming]?

  public init(
    start: TimeInterval,
    end: TimeInterval,
    text: String,
    language: Locale.Language? = nil,
    wordTimings: [WordTiming]? = nil
  ) {
    self.start = start
    self.end = end
    self.text = text
    self.language = language
    self.wordTimings = wordTimings
  }

  public var duration: TimeInterval { max(0, end - start) }
}

public struct WordTiming: Codable, Sendable, Equatable, Hashable {
  public var word: String
  public var start: TimeInterval
  public var end: TimeInterval

  public init(word: String, start: TimeInterval, end: TimeInterval) {
    self.word = word
    self.start = start
    self.end = end
  }
}

/// What a `Diarizer` returns for one lane.
public struct DiarizationResult: Codable, Sendable, Equatable, Hashable {
  public var clusters: [SpeakerCluster]

  public init(clusters: [SpeakerCluster]) {
    self.clusters = clusters
  }
}

/// One diarization cluster before it becomes a `Speaker` row.
public struct SpeakerCluster: Codable, Sendable, Equatable, Hashable {
  /// The diarizer's label, "Speaker 1" onwards in order of first speech.
  public var label: String
  public var ranges: [ClosedRange<TimeInterval>]
  public var embedding: Embedding?
  public var clusterConfidence: Float
  /// The diarizer-chosen clip for manual naming, at most ten seconds.
  public var sampleClipRange: ClosedRange<TimeInterval>?

  public init(
    label: String,
    ranges: [ClosedRange<TimeInterval>],
    embedding: Embedding? = nil,
    clusterConfidence: Float,
    sampleClipRange: ClosedRange<TimeInterval>? = nil
  ) {
    self.label = label
    self.ranges = ranges
    self.embedding = embedding
    self.clusterConfidence = clusterConfidence
    self.sampleClipRange = sampleClipRange
  }
}
