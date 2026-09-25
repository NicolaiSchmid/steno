import Foundation
import StenoCore

/// Builds the analysis requests for one `SummaryTemplate`: the single-shot
/// request over the whole transcript, the map request per chunk, the reduce
/// request over the chunk notes, and the repair request after a decode
/// failure. English prompts; the model writes in the meeting's language.
/// Every string here is pinned by a golden in `Tests/Fixtures/llm/prompts/`.
public struct SummaryPromptBuilder: Sendable {
  public var template: SummaryTemplate
  /// The zone the meeting date is written in; the user's on a Mac, UTC in
  /// goldens.
  public var timeZone: TimeZone
  /// Temperature of every analysis request.
  public static let temperature = 0.2

  public init(template: SummaryTemplate, timeZone: TimeZone = .current) {
    self.template = template
    self.timeZone = timeZone
  }

  // MARK: Template

  /// The template as prompt text: name, context, one line per section with
  /// id, heading, whether it is required, and its instructions.
  public func templateBlocks() -> String {
    var lines = [
      "Template: \(template.displayName)", template.context, "",
      "Sections, in this order, with exactly these ids:",
    ]
    for section in template.sections {
      let requirement =
        section.required ? "required" : "optional, omit it when the meeting has nothing for it"
      lines.append(
        "- \(section.id) — \"\(section.heading)\" (\(requirement)): \(section.instructions)")
    }
    return lines.joined(separator: "\n")
  }

  // MARK: Schemas

  static let taskSchema = JSONSchema.object([
    "text": .string(description: "the commitment, as one sentence"),
    "assignee": .string(description: "known name or speaker label verbatim").nullable,
    "priority": .string(enum: ["low", "normal", "high"]),
    "dueDate": .string(description: "YYYY-MM-DD, resolved against the meeting date").nullable,
  ])

  static let speakerNameSchema = JSONSchema.object([
    "speakerLabel": .string(description: "the label as it appears in the transcript"),
    "name": .string(description: "the person's name").nullable,
    "confidence": .number(description: "0 to 1"),
    "evidence": .string(description: "one short quote from the transcript"),
  ])

  /// The single-shot and reduce answer, section ids as an enum.
  public var draftSchema: JSONSchema {
    .object([
      "title": .string(description: "under 80 characters, \"Topic: Subtopic\" when natural"),
      "language": .string(description: "BCP-47 tag of the language you wrote in"),
      "sections": .array(
        of: .object([
          "id": .string(enum: template.sections.map(\.id)),
          "heading": .string(description: "the section heading in the output language"),
          "bullets": .array(of: .object(["lead": .string(), "text": .string()])),
        ])),
      "decisions": .array(of: .string()),
      "tasks": .array(of: Self.taskSchema),
      "speakerNames": .array(of: Self.speakerNameSchema),
    ])
  }

  /// The map answer for one chunk.
  public var notesSchema: JSONSchema {
    .object([
      "chunkIndex": .integer(),
      "topics": .array(
        of: .object([
          "topic": .string(),
          "points": .array(of: .string(description: "one full sentence, names included")),
        ])),
      "decisions": .array(of: .string()),
      "taskCandidates": .array(of: Self.taskSchema),
      "speakerCues": .array(of: Self.speakerNameSchema),
    ])
  }

  // MARK: Requests

  /// One call over the whole cleaned transcript.
  public func buildSingleShot(_ input: SummaryInput, segments: [TranscriptSegment]) -> LLMRequest {
    let labels = SpeakerLabels(speakers: input.speakers)
    let system = [
      "You are Steno's meeting analyst. You read the transcript of one meeting and return one JSON object and nothing else: no prose before or after it.",
      "", meetingBlock(input), "", templateBlocks(), "", Self.analysisRules, "",
      "Return exactly this JSON shape:", draftSchema.promptText,
    ].joined(separator: "\n")
    let user = "Transcript:\n" + TranscriptLines.renderPlain(segments, labels: labels)
    return request(
      system: system, user: user, schema: draftSchema, name: "meeting_analysis", purpose: "summary")
  }

  /// Notes for one chunk of a long transcript.
  public func buildMap(_ input: SummaryInput, chunk: TranscriptChunk, of total: Int) -> LLMRequest {
    let labels = SpeakerLabels(speakers: input.speakers)
    let system = [
      "You are Steno's meeting analyst. You read part \(chunk.index + 1) of \(total) of one meeting's transcript and return structured notes as one JSON object and nothing else.",
      "", meetingBlock(input), "", "Template: \(template.displayName)", template.context, "",
      Self.notesRules, "", "Return exactly this JSON shape, with chunkIndex \(chunk.index):",
      notesSchema.promptText,
    ].joined(separator: "\n")
    var user = "Part \(chunk.index + 1) of \(total)."
    if !chunk.leadingContext.isEmpty {
      user +=
        "\n\nEnd of the previous part, for context only:\n"
        + TranscriptLines.renderPlain(chunk.leadingContext, labels: labels)
    }
    user += "\n\nTranscript:\n" + TranscriptLines.renderPlain(chunk.segments, labels: labels)
    return request(
      system: system, user: user, schema: notesSchema, name: "chunk_notes", purpose: "summary-map")
  }

  /// One call merging every chunk's notes into the final analysis.
  public func buildReduce(_ input: SummaryInput, notes: [ChunkNotes]) -> LLMRequest {
    let system = [
      "You are Steno's meeting analyst. You receive structured notes taken from the consecutive parts of one meeting's transcript, in order. Merge them into the final analysis and return one JSON object and nothing else.",
      "", meetingBlock(input), "", templateBlocks(), "", Self.analysisRules, "",
      "Notes rules: the notes are your only source; merge duplicate decisions and tasks, keep every distinct one, and prefer later notes when a plan changed. Speaker cues with the same label agree or the higher confidence wins.",
      "", "Return exactly this JSON shape:", draftSchema.promptText,
    ].joined(separator: "\n")
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
    let json = (try? encoder.encode(notes)).map { String(decoding: $0, as: UTF8.self) } ?? "[]"
    let user = "Notes from \(notes.count) parts, in order (JSON):\n" + json
    return request(
      system: system, user: user, schema: draftSchema, name: "meeting_analysis",
      purpose: "summary-reduce")
  }

  /// The one repair round after a decode failure: the invalid output, the
  /// error, the shape, same response format.
  public func buildRepair(
    invalid: String, error: String, schema: JSONSchema? = nil, name: String = "meeting_analysis",
    purpose: String = "summary-repair"
  ) -> LLMRequest {
    let schema = schema ?? draftSchema
    let system = [
      "You fix a JSON answer that failed validation. Return only the corrected JSON object, nothing else. Keep the content; change only what the error requires.",
      "", "The JSON must have exactly this shape:", schema.promptText,
    ].joined(separator: "\n")
    let user = "Validation error: \(error)\n\nInvalid answer:\n\(invalid)"
    return request(system: system, user: user, schema: schema, name: name, purpose: purpose)
  }

  // MARK: Blocks

  /// Output language, meeting date, participants and speaker labels.
  func meetingBlock(_ input: SummaryInput) -> String {
    let language = OutputLanguage.resolve(meeting: input.meeting.language)
    let languageName = OutputLanguage.promptName(language)
    var lines = [
      "Output language: \(languageName). Write the title, every heading, bullet, decision and task in \(languageName); keep product names, code and terms the speakers used in another language as spoken. Translate the section headings given below into \(languageName).",
      "Meeting date: \(Self.formatDate(input.meeting.startedAt, timeZone: timeZone)). Resolve relative dates such as \"next Friday\" against it and write dates as YYYY-MM-DD.",
    ]
    lines.append("Participants:")
    for participant in input.participants {
      lines.append(
        "- \(participant.displayName)\(participant.role == .me ? " (the user, \"me\")" : "")")
    }
    if input.participants.isEmpty { lines.append("- unknown") }
    lines.append("Speaker labels in the transcript:")
    for speaker in input.speakers {
      lines.append("- \(speaker.clusterLabel)\(Self.knownName(speaker, people: input.knownPeople))")
    }
    return lines.joined(separator: "\n")
  }

  static func knownName(_ speaker: Speaker, people: [Person]) -> String {
    guard let personID = speaker.personID,
      let person = people.first(where: { $0.id == personID })
    else { return " (unknown; suggest a name only with evidence from the transcript)" }
    switch speaker.assignment {
    case .confirmed: return " = \(person.displayName)"
    case .suggested: return " = probably \(person.displayName) (unconfirmed)"
    case .unknown: return ""
    }
  }

  static let analysisRules = """
    Rules:
    - Each bullet is a short lead phrase plus one to three sentences of text. The lead names the topic (or the person, where the template says so); the text carries the substance.
    - Name people by their known name where one is listed above, otherwise by their speaker label verbatim (for example "Speaker 2"), so Steno can replace it with the right name later. Never guess a name inside a bullet.
    - Decisions are things the participants agreed on, not proposals or ideas. One sentence each.
    - Tasks are explicit commitments with an owner ("I will send the offer by Friday"). Priority is "high" only when urgency was said, "low" only when it was called optional, otherwise "normal". dueDate is an absolute date or null.
    - speakerNames: suggest a name for a speaker label only with evidence from the transcript (addressed by name, a self-introduction) or by elimination against the participant list; confidence between 0 and 1; evidence is one short quote. Leave name null when there is no evidence.
    - The title is under 80 characters, "Topic: Subtopic" when that reads naturally, in the output language.
    - Only what was said. Never invent facts, names, numbers or dates.
    """

  static let notesRules = """
    Rules:
    - Group what was said into topics in the order they came up. Each point is one full sentence in the output language that names who said or asked for what, using known names where listed above and the speaker label verbatim otherwise.
    - decisions are things agreed in this part, not proposals. taskCandidates are explicit commitments with an owner as said in this part; dueDate is an absolute date or null.
    - speakerCues: evidence in this part for who a speaker label is (addressed by name, a self-introduction), with a short quote; name null when there is none.
    - Only what was said in this part. Never invent anything.
    """

  static func formatDate(_ date: Date, timeZone: TimeZone) -> String {
    let formatter = DateFormatter()
    formatter.locale = Locale(identifier: "en_US_POSIX")
    formatter.timeZone = timeZone
    formatter.dateFormat = "yyyy-MM-dd (EEEE)"
    return formatter.string(from: date)
  }

  private func request(
    system: String, user: String, schema: JSONSchema, name: String, purpose: String
  ) -> LLMRequest {
    LLMRequest(
      messages: [
        LLMMessage(role: .system, content: system), LLMMessage(role: .user, content: user),
      ],
      responseFormat: .jsonSchema(name: name, schema: schema.jsonValue, strict: true),
      temperature: Self.temperature,
      maxTokens: nil,
      purpose: purpose)
  }
}
