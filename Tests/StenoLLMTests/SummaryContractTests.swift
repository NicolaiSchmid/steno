import Foundation
import StenoCore
import Testing

@testable import StenoLLM

/// The summary contract beyond the default template: every bundled
/// template round-trips its section ids through prompt, schema and
/// post-processing; an unknown priority or a missing key is a decode
/// failure that goes once through repair with the path named; the meeting
/// date line follows the summarizer's time zone; dates and priorities in
/// the draft are the model's strings until Steno validates them.
@Suite struct SummaryContractTests {
  static let standup = LLMFixtures.denglishStandup()
  static let utc = TimeZone(identifier: "UTC")!

  /// A draft that fills every section of `template` in reverse order, with
  /// one extra section the template does not know.
  static func fullDraft(for template: SummaryTemplate) -> AnalysisDraft {
    var sections = template.sections.reversed().map { section in
      AnalysisDraft.Section(
        id: section.id, heading: "Überschrift \(section.id)",
        bullets: [.init(lead: "Punkt", text: "Speaker 1 sagt etwas zu \(section.id).")])
    }
    sections.append(
      .init(id: "not-a-section", heading: "Nope", bullets: [.init(lead: "x", text: "y")]))
    return AnalysisDraft(
      title: "Titel: Untertitel", language: "de", sections: sections,
      decisions: ["Entschieden."],
      tasks: [DraftTask(text: "Tun", assignee: "Mara", priority: .normal, dueDate: "2026-10-01")],
      speakerNames: [])
  }

  @Test(arguments: SummaryTemplate.bundledIDs)
  func everyTemplateRoundTripsItsSectionIDs(templateID: String) async throws {
    let template = try #require(SummaryTemplate.bundled(id: templateID))
    let input = SummaryInput(export: Self.standup, template: template)
    let builder = SummaryPromptBuilder(template: template, timeZone: Self.utc)
    let ids = template.sections.map(\.id)

    // Prompt: the section list names exactly the template's ids in order.
    let system = builder.buildSingleShot(input).messages[0].content
    let listed = system.split(separator: "\n").compactMap { line -> String? in
      guard line.hasPrefix("- "), let dash = line.range(of: " — \"") else { return nil }
      return String(line[line.index(line.startIndex, offsetBy: 2)..<dash.lowerBound])
    }
    #expect(listed == ids, "\(templateID)")
    for section in template.sections {
      #expect(system.contains(section.instructions), "\(templateID)/\(section.id) instructions")
    }
    // Schema: the id enum is the same list, for single shot and reduce.
    let idEnum = builder.draftSchema.jsonValue["properties"]?["sections"]?["items"]?["properties"]?[
      "id"]?["enum"]
    #expect(idEnum == .array(ids.map { .string($0) }), "\(templateID)")
    #expect(
      builder.draftSchema.promptText.contains(ids.map { "\"\($0)\"" }.joined(separator: " | ")))
    #expect(
      builder.buildReduce(input, notes: []).messages[0].content.contains(
        ids.map { "\"\($0)\"" }.joined(separator: " | ")))

    // Post-processing: template order restored, the unknown id dropped, the
    // model's headings kept, and the document renders with every heading.
    let server = try StubChatServer()
    defer { server.stop() }
    server.enqueue(Scripts.json(Self.fullDraft(for: template)))
    let output = try await SummaryTests.summarizer(server).summarize(input)
    #expect(output.summary.templateID == templateID)
    #expect(output.summary.sections.map(\.id) == ids, "\(templateID)")
    #expect(output.summary.sections.map(\.heading) == ids.map { "Überschrift \($0)" })
    #expect(output.title == "Titel: Untertitel")
    #expect(output.tasks.first?.assigneePersonID == LLMFixtures.personMaraID)
    var export = Self.standup
    export.meeting.summary = output.summary
    let markdown = SummaryMarkdown.render(export)
    for id in ids {
      #expect(markdown.contains("## Überschrift \(id)\n"), "\(templateID)/\(id) rendered")
    }
    #expect(!markdown.contains("Nope"))
  }

  @Test func requiredSectionsPerTemplateSurviveAnEmptyDraft() throws {
    for template in SummaryTemplate.bundled {
      let input = SummaryInput(export: Self.standup, template: template)
      let empty = AnalysisDraft(
        title: "", language: "de", sections: [], decisions: [], tasks: [], speakerNames: [])
      let output = LLMMeetingSummarizer.output(from: empty, input: input, usage: .zero)
      #expect(
        output.summary.sections.map(\.id) == template.sections.filter(\.required).map(\.id),
        "\(template.id)")
      #expect(output.summary.sections.allSatisfy { $0.bullets.isEmpty })
      #expect(
        output.summary.sections.map(\.heading)
          == template.sections.filter(\.required).map(\.heading), "\(template.id)")
    }
  }

  @Test func anUnknownPriorityIsADecodeFailureRepairedOnceWithThePath() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    let canned = try SummaryTests.canned("summary-default-standup")
    let broken = canned.replacingOccurrences(
      of: "\"priority\": \"high\"", with: "\"priority\": \"urgent\"")
    try #require(broken != canned)
    server.enqueue(Scripts.completion(broken), Scripts.completion(canned))
    let output = try await SummaryTests.summarizer(server).summarize(SummaryTests.defaultInput())
    #expect(server.requests.map(\.purpose) == ["summary", "summary-repair"])
    let repair = try #require(server.requests.last?.chat)
    #expect(repair.messages[1].content.contains("Validation error: "))
    #expect(repair.messages[1].content.contains("tasks.[0].priority"))
    #expect(repair.messages[1].content.contains("\"priority\": \"urgent\""))
    #expect(repair.responseFormat?.jsonSchema?.name == "meeting_analysis")
    #expect(output.tasks.first?.priority == .high)
    #expect(output.usage.requests == 2)
  }

  @Test func aMissingKeyOrWrongTypeNamesThePath() {
    func decode(_ text: String) -> LLMError? {
      #expect(throws: LLMError.self) {
        try StructuredOutputDecoder.decode(
          AnalysisDraft.self, from: LLMResponse(text: text, finishReason: .stop))
      }
    }
    #expect(
      decode(
        "{\"title\":\"t\",\"language\":\"de\",\"sections\":[],\"decisions\":[],\"tasks\":[]}")
        == .invalidJSON("missing key speakerNames at root"))
    let wrong = decode(
      "{\"title\":\"t\",\"language\":\"de\",\"sections\":[{\"id\":\"executive-summary\",\"heading\":\"h\",\"bullets\":\"none\"}],\"decisions\":[],\"tasks\":[],\"speakerNames\":[]}"
    )
    guard case .invalidJSON(let detail) = wrong else {
      Issue.record("expected invalidJSON")
      return
    }
    #expect(detail.hasPrefix("expected Array<Any> at sections.[0].bullets"), "\(detail)")
    let confidence = decode(
      "{\"title\":\"t\",\"language\":\"de\",\"sections\":[],\"decisions\":[],\"tasks\":[],\"speakerNames\":[{\"speakerLabel\":\"Speaker 1\",\"name\":null,\"confidence\":\"high\",\"evidence\":\"\"}]}"
    )
    guard case .invalidJSON(let confidenceDetail) = confidence else {
      Issue.record("expected invalidJSON")
      return
    }
    #expect(confidenceDetail.contains("speakerNames.[0].confidence"))
  }

  @Test func nullableFieldsDecodeAsNil() throws {
    let json = """
      {"title":"t","language":"de","sections":[],"decisions":[],
       "tasks":[{"text":"x","assignee":null,"priority":"low","dueDate":null}],
       "speakerNames":[{"speakerLabel":"Speaker 1","name":null,"confidence":0.1,"evidence":"q"}]}
      """
    let draft = try StructuredOutputDecoder.decode(
      AnalysisDraft.self, from: LLMResponse(text: json, finishReason: .stop))
    #expect(draft.tasks[0].assignee == nil)
    #expect(draft.tasks[0].dueDate == nil)
    #expect(draft.speakerNames[0].name == nil)
    let output = LLMMeetingSummarizer.output(
      from: draft, input: SummaryTests.defaultInput(), usage: .zero)
    #expect(output.tasks[0].assigneeName == nil)
    #expect(output.tasks[0].dueDate == nil)
    #expect(output.speakerNames.isEmpty, "a suggestion without a name is dropped")
  }

  @Test func theMeetingDateLineFollowsTheSummarizerTimeZone() {
    // The fixture starts at 09:00 UTC on Thursday 2026-09-24.
    let input = SummaryTests.defaultInput()
    #expect(input.meeting.startedAt == Date(timeIntervalSince1970: 1_790_240_400))
    func dateLine(_ zone: String) -> String? {
      let builder = SummaryPromptBuilder(
        template: input.template, timeZone: TimeZone(identifier: zone)!)
      return builder.buildSingleShot(input).messages[0].content
        .split(separator: "\n").first { $0.hasPrefix("Meeting date: ") }.map(String.init)
    }
    #expect(dateLine("UTC")?.hasPrefix("Meeting date: 2026-09-24 (Thursday).") == true)
    #expect(dateLine("Europe/Berlin")?.hasPrefix("Meeting date: 2026-09-24 (Thursday).") == true)
    #expect(
      dateLine("Pacific/Honolulu")?.hasPrefix("Meeting date: 2026-09-23 (Wednesday).") == true)
    #expect(
      dateLine("Pacific/Kiritimati")?.hasPrefix("Meeting date: 2026-09-24 (Thursday).") == true)
    // Map and reduce carry the same line.
    let builder = SummaryPromptBuilder(template: input.template, timeZone: Self.utc)
    let chunk = TranscriptChunker().chunk(input.segments, language: "de")[0]
    #expect(
      builder.buildMap(input, chunk: chunk, of: 1).messages[0].content.contains(
        "Meeting date: 2026-09-24 (Thursday)."))
    #expect(
      builder.buildReduce(input, notes: []).messages[0].content.contains(
        "Meeting date: 2026-09-24 (Thursday)."))
  }

  @Test func theSummarizerNeverSendsTheRawTextOrTheAPIKeyAndOnlyText() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    server.enqueue(Scripts.completion(try SummaryTests.canned("summary-default-standup")))
    var export = Self.standup
    for index in export.segments.indices { export.segments[index].rawText = "RAW-\(index)" }
    let input = SummaryInput(export: export, template: SummaryTemplate.bundled(id: "default"))
    _ = try await SummaryTests.summarizer(server).summarize(input)
    let data = try #require(server.requests.first?.body)
    let body = String(decoding: data, as: UTF8.self)
    #expect(!body.contains("RAW-"), "the summary reads the cleaned text, never rawText")
    #expect(!body.contains(export.speakers[0].id.uuidString), "speaker ids stay on the device")
    #expect(body.contains("Speaker 1: okay lass uns anfangen."))
  }
}
