import Foundation

extension ProcessingPipeline {
  struct Cleaned: Sendable {
    var segments: [TranscriptSegment]
    /// Nil when the pass was skipped.
    var usage: LLMUsage?
  }

  /// Runs the `TranscriptCleaner` and persists the cleaned `text`; `rawText`,
  /// ids, order and count are the merge stage's and must come back intact.
  /// The transcript's token count replaces the run's guess from the audio
  /// duration before the stage's event is posted. Without a cleaner (no LLM
  /// endpoint) the stage posts its progress and hands the merged segments on
  /// untouched with nil usage; the merge stage already persisted them, so
  /// nothing is written. A voice the user marked "Not a person" is neither
  /// sent nor cleaned (`withoutDismissed`); its segments keep their text.
  func cleanup(meeting: Meeting, segments: [TranscriptSegment], speakers: [Speaker]) async throws
    -> Cleaned
  {
    let store = self.store
    revise(tokens: ProcessingEstimator.tokenCount(segments), meetingID: meeting.id)
    return try await run(.cleanup, meetingID: meeting.id) {
      guard let cleaner = dependencies.cleaner else {
        return Cleaned(segments: segments, usage: nil)
      }
      let participants = try await store.participants(meetingID: meeting.id)
      let people = try await store.persons()
      let (sent, voices) = try await Self.withoutDismissed(
        segments, speakers, meetingID: meeting.id, store: store)
      let output = try await cleaner.clean(
        CleanupInput(
          segments: sent, language: meeting.language, participants: participants,
          speakers: voices, knownPeople: people))
      guard output.segments.count == sent.count else {
        throw PipelineFailure(
          stage: .cleanup,
          reason: "cleaner returned \(output.segments.count) segments for \(sent.count)")
      }
      var texts: [UUID: String] = [:]
      for (original, candidate) in zip(sent, output.segments) {
        guard candidate.id == original.id else {
          throw PipelineFailure(stage: .cleanup, reason: "cleaner reordered segment \(original.id)")
        }
        texts[original.id] = candidate.text
      }
      let cleaned = segments.map { original in
        var segment = original
        if let text = texts[original.id] { segment.text = text }
        return segment
      }
      var updated = meeting
      updated.updatedAt = self.now
      try await store.replaceTranscript(updated, segments: cleaned, speakers: speakers)
      return Cleaned(segments: cleaned, usage: output.usage)
    }
  }

  /// Without the voices the user marked "Not a person" (as stored now, so a
  /// dismissal kept across processing again counts): their segments and
  /// their rows. The cleanup and the summary see only people. Rust:
  /// `without_dismissed` in `steno-pipeline`.
  static func withoutDismissed(
    _ segments: [TranscriptSegment], _ speakers: [Speaker], meetingID: UUID, store: MeetingStore
  ) async throws -> ([TranscriptSegment], [Speaker]) {
    let dismissed = Set(
      try await store.speakers(meetingID: meetingID).filter(\.dismissed).map(\.id)
        + speakers.filter(\.dismissed).map(\.id))
    guard !dismissed.isEmpty else { return (segments, speakers) }
    return (
      segments.filter { segment in segment.speakerID.map { !dismissed.contains($0) } ?? true },
      speakers.filter { !dismissed.contains($0.id) }
    )
  }
}
