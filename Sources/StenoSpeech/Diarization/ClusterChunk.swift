import Foundation

/// One embedding window of the offline diarizer after clustering: which
/// speaker it was assigned to, when, and its 256-dim WeSpeaker vector.
/// `quality` is not reported per chunk by the framework;
/// `DiarizationMapping.result` fills it from the turn the chunk overlaps
/// most. The framework result is mapped into these first so every mapping
/// test can build them by hand.
struct ClusterChunk: Sendable, Equatable {
  var speakerLabel: String
  var start: TimeInterval
  var end: TimeInterval
  var embedding: [Float]
  var quality: Float = 1

  var duration: TimeInterval { max(0, end - start) }
  var range: ClosedRange<TimeInterval> { start...max(start, end) }
}

/// One "who spoke when" turn from the diarizer, before ranges are merged.
struct SpeakerTurn: Sendable, Equatable {
  var speakerLabel: String
  var start: TimeInterval
  var end: TimeInterval
  var quality: Float

  var duration: TimeInterval { max(0, end - start) }
}
