import Foundation

extension ProcessingPipeline {
  /// Runs the `MeetingSummarizer` for `meeting.templateID` and persists
  /// summary, tasks, decisions and speaker name suggestions with the
  /// meeting's title, language and summed usage in one transaction. An
  /// unknown template id fails the stage; nothing is substituted. A calendar
  /// title stays; any other title is replaced by the model's. The caller
  /// folds earlier usage (cleanup) into `meeting.llmUsage` first.
  ///
  /// Without a summarizer (no LLM endpoint) the stage posts its progress,
  /// leaves title, language and usage as given, and persists the meeting
  /// with `summary` nil and no tasks, decisions or name suggestions, so a
  /// second `process` run never keeps rows an earlier run wrote.
  func summarize(meeting: Meeting, segments: [TranscriptSegment], speakers: [Speaker])
    async throws -> Meeting
  {
    let store = self.store
    return try await run(.summarize, meetingID: meeting.id) {
      var updated = meeting
      guard let summarizer = dependencies.summarizer else {
        updated.summary = nil
        updated.updatedAt = self.now
        try await store.replaceSummary(updated, tasks: [], decisions: [], speakerNames: [])
        return updated
      }
      guard let template = SummaryTemplate.bundled(id: meeting.templateID) else {
        throw PipelineFailure(
          stage: .summarize, reason: "unknown summary template \(meeting.templateID)")
      }
      let participants = try await store.participants(meetingID: meeting.id)
      let people = try await store.persons()
      let output = try await summarizer.summarize(
        SummaryInput(
          meeting: meeting, segments: segments, speakers: speakers, participants: participants,
          knownPeople: people, template: template))

      updated.summary = output.summary
      updated.summary?.templateID = template.id
      if meeting.calendarEventID == nil, !output.title.isEmpty {
        updated.title = output.title
      }
      if let language = output.language { updated.language = language }
      updated.llmUsage = (meeting.llmUsage ?? .zero) + output.usage
      updated.updatedAt = self.now
      try await store.replaceSummary(
        updated, tasks: output.tasks, decisions: output.decisions,
        speakerNames: output.speakerNames)
      return updated
    }
  }
}
