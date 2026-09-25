import Foundation
import StenoCore

// A `MeetingExport` (`meeting.json`) carries everything both passes need,
// so the CLI and the fixtures feed the cleaner and summarizer from one file.

extension CleanupInput {
  public init(export: MeetingExport) {
    self.init(
      segments: export.segments,
      language: export.meeting.language,
      participants: export.participants,
      speakers: export.speakers,
      knownPeople: export.persons)
  }
}

extension SummaryInput {
  /// `template` defaults to the meeting's template, then to `default`.
  public init(export: MeetingExport, template: SummaryTemplate? = nil) {
    self.init(
      meeting: export.meeting,
      segments: export.segments,
      speakers: export.speakers,
      participants: export.participants,
      knownPeople: export.persons,
      template: template
        ?? SummaryTemplate.bundled(id: export.meeting.templateID)
        ?? SummaryTemplate.bundled[0])
  }
}
