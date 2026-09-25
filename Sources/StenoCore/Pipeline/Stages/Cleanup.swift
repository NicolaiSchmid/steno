import Foundation

extension ProcessingPipeline {
  struct Cleaned: Sendable {
    var segments: [TranscriptSegment]
    var usage: LLMUsage
  }

  /// Runs the `TranscriptCleaner` and persists the cleaned `text`; `rawText`,
  /// ids, order and count are the merge stage's and must come back intact.
  func cleanup(meeting: Meeting, segments: [TranscriptSegment], speakers: [Speaker]) async throws
    -> Cleaned
  {
    let cleaner = dependencies.cleaner
    let store = self.store
    return try await run(.cleanup, meetingID: meeting.id) {
      let participants = try await store.participants(meetingID: meeting.id)
      let people = try await store.persons()
      let output = try await cleaner.clean(
        CleanupInput(
          segments: segments, language: meeting.language, participants: participants,
          speakers: speakers, knownPeople: people))
      guard output.segments.count == segments.count else {
        throw PipelineFailure(
          stage: .cleanup,
          reason: "cleaner returned \(output.segments.count) segments for \(segments.count)")
      }
      var cleaned: [TranscriptSegment] = []
      for (original, candidate) in zip(segments, output.segments) {
        guard candidate.id == original.id else {
          throw PipelineFailure(stage: .cleanup, reason: "cleaner reordered segment \(original.id)")
        }
        var segment = original
        segment.text = candidate.text
        cleaned.append(segment)
      }
      var updated = meeting
      updated.updatedAt = self.now
      try await store.replaceTranscript(updated, segments: cleaned, speakers: speakers)
      return Cleaned(segments: cleaned, usage: output.usage)
    }
  }
}
