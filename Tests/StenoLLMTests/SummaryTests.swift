import Foundation
import StenoCore
import Testing

@testable import StenoLLM

@Suite struct SummaryTests {
  static let standup = LLMFixtures.denglishStandup()
  static let utc = TimeZone(identifier: "UTC")!

  static func summarizer(
    _ server: StubChatServer, configure: (inout LLMEndpoint) -> Void = { _ in }
  ) -> LLMMeetingSummarizer {
    var endpoint = LLMEndpoint(baseURL: server.baseURL, model: "stub-model")
    configure(&endpoint)
    let client = OpenAICompatibleClient(endpoint: endpoint, apiKey: nil, retry: .none)
    return LLMMeetingSummarizer(model: client, endpoint: endpoint, timeZone: utc)
  }

  static func canned(_ name: String) throws -> String {
    try String(contentsOf: Fixtures.url("llm/responses/\(name).json"), encoding: .utf8)
  }

  static func defaultInput() -> SummaryInput {
    SummaryInput(export: standup, template: SummaryTemplate.bundled(id: "default"))
  }

  @Test func singleShotBuildsTheSummaryOutputFromTheDraft() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    let usage = LLMUsage(promptTokens: 1_500, completionTokens: 400, requests: 1)
    server.enqueue(Scripts.completion(try Self.canned("summary-default-standup"), usage: usage))
    let input = Self.defaultInput()
    let output = try await Self.summarizer(server).summarize(input)

    #expect(server.requests.count == 1)
    #expect(server.requests.first?.purpose == "summary")
    #expect(server.requests.first?.chat?.temperature == 0.2)
    #expect(server.requests.first?.chat?.responseFormat?.jsonSchema?.name == "meeting_analysis")
    #expect(output.usage == usage)
    #expect(output.title == "Daily Standup: Onboarding, CI und Obsidian-Export")
    #expect(output.language == "de")
    #expect(output.summary.templateID == "default")
    #expect(output.summary.language == "de")

    // Sections: template order, unknown `wrap-up` dropped, empty optional
    // `open-questions` dropped, translated heading kept.
    #expect(output.summary.sections.map(\.id) == ["executive-summary", "full-summary"])
    #expect(output.summary.sections.first?.id == "executive-summary")
    #expect(output.summary.sections[1].heading == "Vollständige Zusammenfassung")
    #expect(output.summary.sections.flatMap(\.bullets).allSatisfy { !$0.lead.isEmpty })
    #expect(output.summary.sections[0].bullets[0].text.contains("Speaker 2"))

    // Rendering with Speaker 2 confirmed as Nicolai bolds the name.
    var export = Self.standup
    export.meeting.summary = output.summary
    export.speakers[1].assignment = .confirmed(personID: SampleData.personNicolaiID)
    let markdown = SummaryMarkdown.render(export)
    #expect(markdown.contains("**Nicolai** übernimmt den Export nach Obsidian"))
    #expect(!markdown.contains("Speaker 2"))
    #expect(markdown.hasPrefix("## Executive Summary\n"))

    #expect(output.decisions.count == 2, "duplicate decisions collapse")
    #expect(output.tasks.count == 4, "the blank task is dropped")
    #expect(output.tasks[0].dueDate == nil, "\"next Friday\" is not a date")
    #expect(output.tasks[0].priority == .high)
    #expect(output.tasks[0].assigneeName == "Speaker 2", "an unconfirmed speaker keeps its label")
    #expect(output.tasks[0].assigneePersonID == nil)
    #expect(output.tasks[1].assigneeName == "Jérôme")
    #expect(output.tasks[1].assigneePersonID == SampleData.personJeromeID)
    #expect(
      output.tasks[1].dueDate == SampleData.startedAt.addingTimeInterval(-9 * 3_600), "midnight UTC"
    )
    #expect(output.tasks[2].assigneeName == "Jérôme", "case-insensitive")
    #expect(output.tasks[2].dueDate == Date(timeIntervalSince1970: 1_790_726_400))
    #expect(output.tasks[3].priority == .low)
    #expect(output.tasks[3].assigneePersonID == LLMFixtures.personMaraID)
    #expect(output.tasks.map(\.meetingID).allSatisfy { $0 == input.meeting.id })
    #expect(Set(output.tasks.map(\.id)).count == 4)
    #expect(output.tasks[0].id == UUID(derivedFrom: input.meeting.id, salt: "task-0"))

    #expect(output.speakerNames.count == 1)
    #expect(output.speakerNames.first?.speakerID == LLMFixtures.standupSpeakerTwoID)
    #expect(output.speakerNames.first?.name == "Nicolai")
    #expect(output.speakerNames.first?.confidence == 0.9)
  }

  @Test func anInvalidAnswerIsRepairedOnceThenFails() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    server.enqueue(contentsOf: [
      Scripts.completion("{\"title\": \"x\", \"sections\": [}"),
      Scripts.fenced(
        try WireJSON.decode(
          AnalysisDraft.self, from: Data(try Self.canned("summary-default-standup").utf8))),
    ])
    let output = try await Self.summarizer(server).summarize(Self.defaultInput())
    #expect(output.summary.sections.count == 2)
    #expect(server.requests.map(\.purpose) == ["summary", "summary-repair"])
    let repair = try #require(server.requests.last?.chat)
    #expect(repair.messages[1].content.contains("Validation error: malformed JSON"))
    #expect(repair.messages[1].content.contains("{\"title\": \"x\", \"sections\": [}"))
    #expect(output.usage.requests == 2)

    server.enqueue(contentsOf: [Scripts.completion("nope"), Scripts.completion("still nope")])
    let error = await #expect(throws: LLMError.self) {
      try await Self.summarizer(server).summarize(Self.defaultInput())
    }
    guard case .invalidJSON = error else {
      Issue.record("expected invalidJSON, got \(String(describing: error))")
      return
    }
    #expect(server.requests.count == 4)
  }

  @Test func truncatedAnswerFailsWithoutRepair() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    server.enqueue(Scripts.truncated("{\"title\": \"cut"))
    let error = await #expect(throws: LLMError.self) {
      try await Self.summarizer(server).summarize(Self.defaultInput())
    }
    #expect(error == .truncated)
    #expect(server.requests.count == 1)
  }

  @Test func requiredSectionsSurviveEmptyAndHeadingsFallBackToTheTemplate() {
    let draft = AnalysisDraft(
      title: "  ", language: "de",
      sections: [
        .init(
          id: "open-questions", heading: " ",
          bullets: [.init(lead: "Zeitplan:", text: " Oktober? ")]),
        .init(id: "executive-summary", heading: "", bullets: [.init(lead: "", text: "")]),
      ],
      decisions: [" a ", "A", ""], tasks: [], speakerNames: [])
    let input = Self.defaultInput()
    let output = LLMMeetingSummarizer.output(
      from: draft, input: input, language: "de", usage: .zero)
    #expect(output.title == input.meeting.title, "a blank title keeps the meeting's")
    #expect(output.summary.sections.map(\.id) == ["executive-summary", "open-questions"])
    #expect(output.summary.sections[0].bullets.isEmpty)
    #expect(output.summary.sections[0].heading == "Executive Summary")
    #expect(output.summary.sections[1].heading == "Open Questions")
    #expect(
      output.summary.sections[1].bullets == [SummaryBullet(lead: "Zeitplan", text: "Oktober?")])
    #expect(output.decisions == ["a"])
  }

  @Test func assigneeResolutionOrder() {
    let input = SummaryInput(export: LLMFixtures.customerCall60min())
    let labels = SpeakerLabels(speakers: input.speakers)
    func resolve(_ name: String?) -> (name: String, personID: UUID?)? {
      LLMMeetingSummarizer.resolveAssignee(name, input: input, labels: labels)
    }
    #expect(resolve(nil) == nil)
    #expect(resolve("  ") == nil)
    #expect(resolve("petra vogel")?.name == "Petra Vogel")
    #expect(resolve("Petra")?.name == "Petra Vogel", "unique first name")
    #expect(resolve("Tom")?.name == "Tom Berger")
    #expect(resolve("Nicolai")?.personID == SampleData.personNicolaiID)
    #expect(resolve("me")?.name == "Nicolai", "the Me speaker is a confirmed person")
    #expect(resolve("Speaker 1")?.name == "Speaker 1", "unresolved speaker keeps its label")
    #expect(resolve("Speaker 1")?.personID == nil)
    #expect(
      resolve("Jérôme")?.personID == SampleData.personJeromeID, "known person, not a participant")
    #expect(resolve("Somebody Else")?.name == "Somebody Else")
    #expect(resolve("Somebody Else")?.personID == nil)
  }

  @Test func dueDatesAreStrict() {
    #expect(
      LLMMeetingSummarizer.parseDueDate("2026-09-26") == Date(timeIntervalSince1970: 1_790_380_800))
    #expect(LLMMeetingSummarizer.parseDueDate(" 2026-09-26 ") != nil)
    #expect(LLMMeetingSummarizer.parseDueDate("next Friday") == nil)
    #expect(LLMMeetingSummarizer.parseDueDate("2026-9-26") == nil)
    #expect(LLMMeetingSummarizer.parseDueDate("26.09.2026") == nil)
    #expect(LLMMeetingSummarizer.parseDueDate("2026-13-01") == nil)
    #expect(LLMMeetingSummarizer.parseDueDate("2026-02-30") == nil)
    #expect(LLMMeetingSummarizer.parseDueDate(nil) == nil)
    #expect(LLMMeetingSummarizer.parseDueDate("") == nil)
  }

  @Test func speakerSuggestionsAreFilteredClampedAndOnePerSpeaker() {
    let speakers = Self.standup.speakers
    let labels = SpeakerLabels(speakers: speakers)
    let drafts = [
      DraftSpeakerName(
        speakerLabel: "speaker 3", name: "Jérôme", confidence: 1.7, evidence: " quote "),
      DraftSpeakerName(speakerLabel: "Speaker 2", name: "Nico", confidence: 0.4, evidence: "a"),
      DraftSpeakerName(speakerLabel: "Speaker 2", name: "Nicolai", confidence: 0.8, evidence: "b"),
      DraftSpeakerName(speakerLabel: "Speaker 1", name: "Mara", confidence: 0.29, evidence: "c"),
      DraftSpeakerName(speakerLabel: "Speaker 1", name: "", confidence: 0.9, evidence: "d"),
      DraftSpeakerName(speakerLabel: "Speaker 7", name: "Ghost", confidence: 0.9, evidence: "e"),
    ]
    let result = LLMMeetingSummarizer.suggestions(
      drafts, labels: labels, speakers: speakers, minimum: 0.3)
    #expect(
      result == [
        SpeakerNameSuggestion(
          speakerID: LLMFixtures.standupSpeakerTwoID, name: "Nicolai", confidence: 0.8,
          evidence: "b"),
        SpeakerNameSuggestion(
          speakerID: LLMFixtures.standupSpeakerThreeID, name: "Jérôme", confidence: 1,
          evidence: "quote"),
      ])
  }

  @Test(arguments: SummaryTemplate.bundledIDs)
  func singleShotPromptMatchesItsGolden(templateID: String) throws {
    let input = SummaryInput(
      export: Self.standup, template: SummaryTemplate.bundled(id: templateID))
    let request = SummaryPromptBuilder(template: input.template, timeZone: Self.utc)
      .buildSingleShot(input, segments: input.segments)
    try Snapshot.assert(
      PromptSnapshotTests.render(request), matches: "llm/prompts/summary-single-\(templateID).txt")
    let system = request.messages[0].content
    #expect(system.contains("Output language: German."))
    #expect(system.contains("Meeting date: 2026-09-24 (Thursday)."))
    #expect(
      system.contains(
        "- Speaker 2 (unknown; suggest a name only with evidence from the transcript)"))
    #expect(system.contains("- Speaker 3 = Jérôme"))
    #expect(system.contains("- Speaker 1 = probably Mara (unconfirmed)"))
    #expect(system.contains("- Nicolai (the user, \"me\")"))
    #expect(
      request.messages[1].content.hasPrefix("Transcript:\nSpeaker 1: okay lass uns anfangen."))
  }

  @Test func anUntaggedMeetingIsSummarisedInEnglish() throws {
    var export = Self.standup
    export.meeting.language = nil
    let input = SummaryInput(export: export, template: SummaryTemplate.bundled(id: "default"))
    let request = SummaryPromptBuilder(template: input.template, timeZone: Self.utc)
      .buildSingleShot(input, segments: input.segments)
    try Snapshot.assert(
      PromptSnapshotTests.render(request), matches: "llm/prompts/summary-single-default-en.txt")
    #expect(request.messages[0].content.contains("Output language: English."))
  }
}
