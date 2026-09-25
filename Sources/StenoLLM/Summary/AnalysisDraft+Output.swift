import Foundation
import StenoCore

// "The model returned X, what do we make of it": the summary pass's
// post-processing as pure functions over the draft, mirroring
// `CleanupDraft.problems(against:)` for pass 1. `LLMMeetingSummarizer` only
// talks to the model.

extension AnalysisDraft {
  /// The draft as `SummaryOutput`: sections in template order (unknown ids
  /// dropped, a section the model split merged, empty optional ones dropped,
  /// empty required ones kept with the template heading), a blank title
  /// replaced by the meeting's, decisions deduplicated, tasks with derived
  /// ids, strict `yyyy-MM-dd` due dates, assignees resolved against the
  /// participants, speaker labels mapped to ids, name suggestions below
  /// `minimumConfidence` dropped. The document's `language` is the meeting's,
  /// else English (what the renderer needs); the output's `language` is the
  /// meeting's tag as elected, nil included, so the pipeline never stores
  /// English for a meeting the engine left untagged.
  public func summaryOutput(for input: SummaryInput, usage: LLMUsage, minimumConfidence: Double)
    -> SummaryOutput
  {
    let labels = SpeakerLabels(speakers: input.speakers)
    let title = Self.trimmed(title).isEmpty ? input.meeting.title : Self.trimmed(title)
    return SummaryOutput(
      title: title,
      summary: SummaryDocument(
        templateID: input.template.id,
        language: OutputLanguage.resolve(meeting: input.meeting.language),
        sections: Self.sections(from: self, template: input.template)),
      decisions: Self.unique(decisions.map(Self.trimmed).filter { !$0.isEmpty }),
      tasks: tasks.enumerated().compactMap { offset, task in
        Self.makeTask(task, index: offset, input: input, labels: labels)
      },
      speakerNames: Self.suggestions(
        speakerNames, labels: labels, speakers: input.speakers, minimum: minimumConfidence),
      language: input.meeting.language,
      usage: usage)
  }

  /// Template order; unknown ids dropped; a section the model split in two
  /// is merged (bullets in order, the first non-blank heading); empty
  /// optional sections dropped, empty required ones kept; a missing or blank
  /// heading falls back to the template's.
  static func sections(from draft: AnalysisDraft, template: SummaryTemplate) -> [SummarySection] {
    template.sections.compactMap { section in
      let drafted = draft.sections.filter { $0.id == section.id }
      let bullets = drafted.flatMap(\.bullets).compactMap { bullet -> SummaryBullet? in
        var lead = trimmed(bullet.lead)
        if lead.hasSuffix(":") { lead.removeLast() }
        let text = trimmed(bullet.text)
        guard !text.isEmpty || !lead.isEmpty else { return nil }
        return SummaryBullet(lead: lead, text: text)
      }
      guard !bullets.isEmpty || section.required else { return nil }
      let heading = drafted.map { trimmed($0.heading) }.first { !$0.isEmpty } ?? section.heading
      return SummarySection(id: section.id, heading: heading, bullets: bullets)
    }
  }

  static func makeTask(_ task: DraftTask, index: Int, input: SummaryInput, labels: SpeakerLabels)
    -> MeetingTask?
  {
    let text = trimmed(task.text)
    guard !text.isEmpty else { return nil }
    let assignee = resolveAssignee(task.assignee, input: input, labels: labels)
    return MeetingTask(
      id: UUID(derivedFrom: input.meeting.id, salt: "task-\(index)"),
      meetingID: input.meeting.id,
      text: text,
      assigneePersonID: assignee?.personID,
      assigneeName: assignee?.name,
      priority: task.priority,
      dueDate: parseDueDate(task.dueDate))
  }

  /// A participant by full name, then by unique first name, then a speaker
  /// label (its confirmed or suggested person), then a known person; else
  /// the name as written with no person.
  static func resolveAssignee(_ raw: String?, input: SummaryInput, labels: SpeakerLabels)
    -> (name: String, personID: UUID?)?
  {
    guard let raw, !trimmed(raw).isEmpty else { return nil }
    let name = trimmed(raw)
    let lowered = name.lowercased()
    if let participant = input.participants.first(where: { $0.displayName.lowercased() == lowered })
    {
      return (participant.displayName, participant.personID)
    }
    let byFirstName = input.participants.filter {
      $0.displayName.lowercased().split(separator: " ").first.map(String.init) == lowered
    }
    if byFirstName.count == 1, let participant = byFirstName.first {
      return (participant.displayName, participant.personID)
    }
    if let speakerID = labels.speakerID(forLabel: name),
      let speaker = input.speakers.first(where: { $0.id == speakerID })
    {
      if let personID = speaker.personID,
        let person = input.knownPeople.first(where: { $0.id == personID })
      {
        return (person.displayName, person.id)
      }
      return (speaker.clusterLabel, nil)
    }
    if let person = input.knownPeople.first(where: { $0.displayName.lowercased() == lowered }) {
      return (person.displayName, person.id)
    }
    return (name, nil)
  }

  /// `YYYY-MM-DD` at midnight UTC, else nil; anything looser is dropped.
  static func parseDueDate(_ raw: String?) -> Date? {
    guard let raw else { return nil }
    let text = trimmed(raw)
    guard text.count == 10 else { return nil }
    let formatter = DateFormatter()
    formatter.locale = Locale(identifier: "en_US_POSIX")
    formatter.timeZone = .gmt
    formatter.dateFormat = "yyyy-MM-dd"
    formatter.isLenient = false
    guard let date = formatter.date(from: text), formatter.string(from: date) == text else {
      return nil
    }
    return date
  }

  /// Known labels only, a name present, confidence at or above `minimum`
  /// and clamped to 0...1, one suggestion per speaker (the strongest), in
  /// speaker order.
  static func suggestions(
    _ drafts: [DraftSpeakerName], labels: SpeakerLabels, speakers: [Speaker], minimum: Double
  ) -> [SpeakerNameSuggestion] {
    var best: [UUID: SpeakerNameSuggestion] = [:]
    for draft in drafts {
      guard let speakerID = labels.speakerID(forLabel: draft.speakerLabel),
        let name = draft.name.map(trimmed), !name.isEmpty
      else { continue }
      let confidence = min(max(draft.confidence, 0), 1)
      guard confidence >= minimum else { continue }
      let suggestion = SpeakerNameSuggestion(
        speakerID: speakerID, name: name, confidence: confidence, evidence: trimmed(draft.evidence))
      if let existing = best[speakerID], existing.confidence >= confidence { continue }
      best[speakerID] = suggestion
    }
    return speakers.compactMap { best[$0.id] }
  }

  static func trimmed(_ text: String) -> String {
    text.trimmingCharacters(in: .whitespacesAndNewlines)
  }

  static func unique(_ strings: [String]) -> [String] {
    var seen: Set<String> = []
    return strings.filter { seen.insert($0.lowercased()).inserted }
  }
}
