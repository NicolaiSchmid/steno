import Foundation

extension ProcessingPipeline {
  /// Runs the `MeetingSummarizer` for `meeting.templateID` and persists
  /// summary, tasks, decisions and speaker name suggestions with the
  /// meeting's title, language and summed usage in one transaction. An unknown template id fails the stage;
  /// nothing is substituted. A calendar title stays; any other title is
  /// replaced by the model's. The caller folds earlier usage (cleanup) into
  /// `meeting.llmUsage` first.
  func summarize(meeting: Meeting, segments: [TranscriptSegment], speakers: [Speaker])
    async throws -> Meeting
  {
    let summarizer = dependencies.summarizer
    let store = self.store
    return try await run(.summarize, meetingID: meeting.id) {
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

      var updated = meeting
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
