import Foundation

/// Elects a meeting's language from tagged segments by summed duration.
public enum LanguageElection {
  /// The language with the largest summed segment duration; nil when no
  /// segment is tagged. Ties break on the tag so the result is stable.
  public static func elect(_ segments: [RawSegment]) -> Locale.Language? {
    var totals: [String: (language: Locale.Language, duration: TimeInterval)] = [:]
    for segment in segments {
      guard let language = segment.language else { continue }
      let key = language.stenoIdentifier
      totals[key, default: (language, 0)].duration += segment.duration
    }
    return
      totals
      .sorted { lhs, rhs in
        if lhs.value.duration != rhs.value.duration {
          return lhs.value.duration > rhs.value.duration
        }
        return lhs.key < rhs.key
      }
      .first?.value.language
  }
}

extension ProcessingPipeline {
  struct Transcription: Sendable {
    var lanes: [AudioLane: [RawSegment]]
    var language: Locale.Language?
  }

  /// Per lane: decode, then transcribe with the previous lane's dominant
  /// language as the hint. The buffer goes out of scope before the next lane
  /// is decoded. `progress` is posted once for `decode` and once for
  /// `transcribe`, on the first lane.
  func decodeAndTranscribe(asset: AudioAsset, meetingID: UUID) async throws -> Transcription {
    let engine = dependencies.speechEngine
    try await run(.decode, meetingID: meetingID, post: true) {
      try await engine.prepare()
    }
    var lanes: [AudioLane: [RawSegment]] = [:]
    var hint: Locale.Language?
    for (index, lane) in Self.orderedLanes(asset.lanes).enumerated() {
      let segments = try await transcribeLane(
        lane, asset: asset, meetingID: meetingID, hint: hint, first: index == 0)
      lanes[lane] = segments
      hint = LanguageElection.elect(segments) ?? hint
    }
    let language = LanguageElection.elect(lanes.values.flatMap { $0 })
    return Transcription(lanes: lanes, language: language)
  }

  private func transcribeLane(
    _ lane: AudioLane, asset: AudioAsset, meetingID: UUID, hint: Locale.Language?, first: Bool
  ) async throws -> [RawSegment] {
    let decoder = dependencies.decoder
    let engine = dependencies.speechEngine
    let buffer = try await run(.decode, meetingID: meetingID, post: false) {
      try await decoder.decode(asset, lane: lane)
    }
    return try await run(.transcribe, meetingID: meetingID, post: first) {
      try await engine.transcribe(buffer, hint: hint)
    }
  }

  /// `.mic` before `.system` before `.mixed`, so "me" sets the hint.
  static func orderedLanes(_ lanes: [AudioLane]) -> [AudioLane] {
    AudioLane.allCases.filter { lanes.contains($0) }
  }
}
