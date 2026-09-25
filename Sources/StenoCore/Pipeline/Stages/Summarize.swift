import Foundation

extension ProcessingPipeline {
  /// Runs the `MeetingSummarizer` for `templateID` (falling back to the
  /// first bundled template), persists summary, tasks and decisions, and returns
  /// the meeting with title, language and summed usage updated. A calendar
  /// title stays; any other title is replaced by the model's.
  func summarize(
    meeting: Meeting, segments: [TranscriptSegment], speakers: [Speaker], templateID: String,
    priorUsage: LLMUsage?
  ) async throws -> Meeting {
    let summarizer = dependencies.summarizer
    let store = self.store
    return try await run(.summarize, meetingID: meeting.id) {
      guard let template = SummaryTemplate.bundled(id: templateID) ?? SummaryTemplate.bundled.first
      else {
        throw PipelineFailure(stage: .summarize, reason: "no bundled summary template")
      }
      let participants = try await store.participants(meetingID: meeting.id)
      let people = try await store.persons()
      var input = meeting
      input.templateID = template.id
      let output = try await summarizer.summarize(
        SummaryInput(
          meeting: input, segments: segments, speakers: speakers, participants: participants,
          knownPeople: people, template: template))

      var updated = meeting
      updated.templateID = template.id
      updated.summary = output.summary
      updated.summary?.templateID = template.id
      if meeting.calendarEventID == nil, !output.title.isEmpty {
        updated.title = output.title
      }
      if let language = output.language { updated.language = language }
      updated.llmUsage = (priorUsage ?? meeting.llmUsage ?? .zero) + output.usage
      updated.updatedAt = self.now
      try await store.save(updated)
      try await store.replaceSummary(
        meetingID: meeting.id, output: output, templateID: template.id, now: self.now)
      return updated
    }
  }
}
