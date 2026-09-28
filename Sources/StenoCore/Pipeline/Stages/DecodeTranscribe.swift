import Foundation

/// Elects a meeting's language from tagged segments by summed duration.
public enum LanguageElection {
  /// The language with the largest summed segment duration; nil when no
  /// segment is tagged. Ties break on the tag so the result is stable.
  public static func elect(_ segments: [RawSegment]) -> LanguageTag? {
    var totals: [LanguageTag: TimeInterval] = [:]
    for segment in segments {
      guard let language = segment.language else { continue }
      totals[language, default: 0] += segment.duration
    }
    return
      totals
      .sorted { lhs, rhs in
        if lhs.value != rhs.value { return lhs.value > rhs.value }
        return lhs.key.rawValue < rhs.key.rawValue
      }
      .first?.key
  }
}

extension ProcessingPipeline {
  struct Transcription: Sendable {
    var lanes: [AudioLane: [RawSegment]]
    var language: LanguageTag?
  }

  /// Per lane: decode, then transcribe with the previous lane's dominant
  /// language as the hint. The buffer goes out of scope before the next lane
  /// is decoded. `progress` is posted once for `decode`, on the first lane,
  /// and once per lane for `transcribe`; the engine was prepared by
  /// `process` before the run's first event.
  func decodeAndTranscribe(asset: AudioAsset, meetingID: UUID) async throws -> Transcription {
    let decoder = dependencies.decoder
    let engine = dependencies.speechEngine
    var lanes: [AudioLane: [RawSegment]] = [:]
    var hint: LanguageTag?
    for (index, lane) in Self.orderedLanes(asset.lanes).enumerated() {
      let buffer: AudioBuffer16k
      if index == 0 {
        buffer = try await run(.decode, meetingID: meetingID) {
          try await decoder.decode(asset, lane: lane)
        }
      } else {
        buffer = try await attributing(.decode) { try await decoder.decode(asset, lane: lane) }
      }
      let laneHint = hint
      let segments = try await run(.transcribe, lane: index, meetingID: meetingID) {
        try await engine.transcribe(buffer, hint: laneHint?.language)
      }
      lanes[lane] = segments
      hint = LanguageElection.elect(segments) ?? hint
    }
    let language = LanguageElection.elect(lanes.values.flatMap { $0 })
    return Transcription(lanes: lanes, language: language)
  }

  /// `.mic` before `.system` before `.mixed`, so "me" sets the hint.
  static func orderedLanes(_ lanes: [AudioLane]) -> [AudioLane] {
    AudioLane.allCases.filter { lanes.contains($0) }
  }
}
