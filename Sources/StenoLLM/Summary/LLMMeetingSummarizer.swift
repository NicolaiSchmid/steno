import Foundation
import StenoCore

/// Pass 2: title, structured summary for the template, decisions, tasks and
/// speaker name suggestions. One call when the transcript fits the input
/// budget, else map (notes per chunk) and reduce (one call). A decode
/// failure goes once through the repair request, then fails the call.
/// Post-processing drops sections the template does not know and empty
/// optional ones, validates due dates strictly, matches assignees to
/// participants, maps speaker labels to ids and filters weak name guesses.
/// No Markdown is produced here; StenoCore renders the `SummaryDocument`.
public struct LLMMeetingSummarizer: MeetingSummarizer, Sendable {
  public var model: any LanguageModel
  public var endpoint: LLMEndpoint
  /// The zone the meeting date is written in for the model.
  public var timeZone: TimeZone
  /// Suggestions below this confidence are dropped.
  public var minimumConfidence: Double

  public init(
    model: any LanguageModel, endpoint: LLMEndpoint, timeZone: TimeZone = .current,
    minimumConfidence: Double = 0.3
  ) {
    self.model = model
    self.endpoint = endpoint
    self.timeZone = timeZone
    self.minimumConfidence = minimumConfidence
  }

  /// Tokens kept for the answer: the endpoint's ceiling, at most a quarter
  /// of the context.
  public var reservedOutputTokens: Int {
    max(256, min(endpoint.maxOutputTokens, endpoint.contextTokens / 4))
  }

  public func summarize(_ input: SummaryInput) async throws -> SummaryOutput {
    let builder = SummaryPromptBuilder(template: input.template, timeZone: timeZone)
    let language = OutputLanguage.resolve(meeting: input.meeting.language)
    let singleShot = builder.buildSingleShot(input, segments: input.segments)
    let budget = TokenBudget(
      endpoint: endpoint, reservedOutputTokens: reservedOutputTokens,
      promptOverheadTokens: TokenBudget.estimateTokens(
        singleShot.messages[0].content, language: "en") + 64)
    let transcriptTokens = TranscriptChunker.estimateTokens(
      input.segments, language: input.meeting.language)

    let draft: AnalysisDraft
    let usage: LLMUsage
    if budget.fits(transcriptTokens) {
      var request = singleShot
      request.maxTokens = reservedOutputTokens
      (draft, usage) = try await complete(AnalysisDraft.self, request, builder: builder)
    } else {
      (draft, usage) = try await mapReduce(
        input, builder: builder, budget: budget, transcriptTokens: transcriptTokens)
    }
    return Self.output(from: draft, input: input, language: language, usage: usage)
  }

  /// Estimated tokens one chunk's notes take in the reduce prompt; decides
  /// up front whether two levels are enough.
  public static let notesTokensPerChunk = 200

  /// Notes per chunk (`maxConcurrentRequests` at a time), then one reduce
  /// call over every chunk's notes. Two levels only: when the notes alone
  /// would not fit the budget, `transcriptTooLong` is thrown before the
  /// first call; when the real notes turn out too long, after the map.
  func mapReduce(
    _ input: SummaryInput, builder: SummaryPromptBuilder, budget: TokenBudget,
    transcriptTokens: Int
  ) async throws -> (AnalysisDraft, LLMUsage) {
    let chunks = TranscriptChunker(budget: budget.inputBudget).chunk(
      input.segments, language: input.meeting.language)
    guard budget.fits(chunks.count * Self.notesTokensPerChunk) else {
      throw LLMError.transcriptTooLong(
        estimatedTokens: transcriptTokens, budget: budget.inputBudget)
    }
    let notesTokens = min(reservedOutputTokens, 1_500)
    let mapped = try await mapBounded(chunks, limit: max(1, endpoint.maxConcurrentRequests)) {
      chunk -> (ChunkNotes, LLMUsage) in
      var request = builder.buildMap(input, chunk: chunk, of: chunks.count)
      request.maxTokens = notesTokens
      var (notes, usage) = try await self.complete(
        ChunkNotes.self, request, builder: builder, schema: builder.notesSchema,
        schemaName: "chunk_notes")
      notes.chunkIndex = chunk.index
      return (notes, usage)
    }
    let notes = mapped.map(\.0)
    var usage = mapped.map(\.1).reduce(LLMUsage.zero, +)
    var reduce = builder.buildReduce(input, notes: notes)
    reduce.maxTokens = reservedOutputTokens
    let notesEstimate = TokenBudget.estimateTokens(reduce.messages[1].content, language: "en")
    guard budget.fits(notesEstimate) else {
      throw LLMError.transcriptTooLong(estimatedTokens: notesEstimate, budget: budget.inputBudget)
    }
    let (draft, reduceUsage) = try await complete(AnalysisDraft.self, reduce, builder: builder)
    usage = usage + reduceUsage
    return (draft, usage)
  }

  // MARK: Calls

  /// One request, decoded into `type`; an undecodable answer gets exactly
  /// one repair round. Usage sums both calls.
  func complete<T: Decodable & Sendable>(
    _ type: T.Type, _ request: LLMRequest, builder: SummaryPromptBuilder,
    schema: JSONSchema? = nil, schemaName: String = "meeting_analysis"
  ) async throws -> (T, LLMUsage) {
    let decoder = StructuredOutputDecoder()
    let first = try await model.complete(request)
    var usage = Self.usage(of: first)
    do {
      return (try decoder.decode(type, from: first), usage)
    } catch let error as LLMError {
      guard case .invalidJSON(let detail) = error else { throw error }
      let repair = builder.buildRepair(
        invalid: first.text, error: detail, schema: schema, name: schemaName,
        purpose: request.purpose + "-repair")
      var repairRequest = repair
      repairRequest.maxTokens = request.maxTokens
      let second = try await model.complete(repairRequest)
      usage = usage + Self.usage(of: second)
      return (try decoder.decode(type, from: second), usage)
    }
  }

  static func usage(of response: LLMResponse) -> LLMUsage {
    response.usage ?? LLMUsage(promptTokens: 0, completionTokens: 0, requests: 1)
  }

  // MARK: Post-processing

  static func output(
    from draft: AnalysisDraft, input: SummaryInput, language: LanguageTag, usage: LLMUsage
  ) -> SummaryOutput {
    let labels = SpeakerLabels(speakers: input.speakers)
    let title = trimmed(draft.title).isEmpty ? input.meeting.title : trimmed(draft.title)
    return SummaryOutput(
      title: title,
      summary: SummaryDocument(
        templateID: input.template.id, language: language,
        sections: sections(from: draft, template: input.template)),
      decisions: unique(draft.decisions.map(trimmed).filter { !$0.isEmpty }),
      tasks: draft.tasks.enumerated().compactMap { offset, task in
        makeTask(task, index: offset, input: input, labels: labels)
      },
      speakerNames: suggestions(
        draft.speakerNames, labels: labels, speakers: input.speakers, minimum: 0.3),
      language: language,
      usage: usage)
  }

  /// Template order; unknown ids dropped; empty optional sections dropped,
  /// empty required ones kept; a missing or blank heading falls back to the
  /// template's.
  static func sections(from draft: AnalysisDraft, template: SummaryTemplate) -> [SummarySection] {
    template.sections.compactMap { section in
      let drafted = draft.sections.first { $0.id == section.id }
      let bullets = (drafted?.bullets ?? []).compactMap { bullet -> SummaryBullet? in
        var lead = trimmed(bullet.lead)
        if lead.hasSuffix(":") { lead.removeLast() }
        let text = trimmed(bullet.text)
        guard !text.isEmpty || !lead.isEmpty else { return nil }
        return SummaryBullet(lead: lead, text: text)
      }
      guard !bullets.isEmpty || section.required else { return nil }
      let heading = trimmed(drafted?.heading ?? "")
      return SummarySection(
        id: section.id, heading: heading.isEmpty ? section.heading : heading, bullets: bullets)
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
      priority: task.priority.taskPriority,
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
    formatter.timeZone = TimeZone(identifier: "UTC")
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
