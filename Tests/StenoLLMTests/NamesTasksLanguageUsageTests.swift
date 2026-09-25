import Foundation
import StenoCore
import Testing

@testable import StenoLLM

/// Step 9 of the plan: speaker labels map to ids, weak suggestions are
/// dropped, the meeting language drives the prompt and the output, and
/// usage is the sum of every call the server saw.
@Suite struct NamesTasksLanguageUsageTests {
  static let standup = LLMFixtures.denglishStandup()
  static let utc = TimeZone(identifier: "UTC")!

  static func draft(speakerNames: [DraftSpeakerName] = [], tasks: [DraftTask] = [])
    -> AnalysisDraft
  {
    AnalysisDraft(
      title: "T",
      sections: [
        .init(
          id: "executive-summary", heading: "Executive Summary",
          bullets: [.init(lead: "Lead", text: "Speaker 2 sagt etwas.")])
      ],
      decisions: [], tasks: tasks, speakerNames: speakerNames)
  }

  static func input(language: LanguageTag?) -> SummaryInput {
    var export = standup
    export.meeting.language = language
    return SummaryInput(export: export, template: SummaryTemplate.bundled(id: "default"))
  }

  @Test func speakerTwoMapsToItsUUIDAndWeakSuggestionsAreDropped() {
    let names = [
      DraftSpeakerName(speakerLabel: "Speaker 2", name: "Nicolai", confidence: 0.85, evidence: "q"),
      DraftSpeakerName(speakerLabel: "SPEAKER 1", name: "Mara", confidence: 0.29, evidence: "q"),
      DraftSpeakerName(speakerLabel: "Speaker 3", name: "Jérôme", confidence: 0.3, evidence: "q"),
    ]
    let output = Self.draft(speakerNames: names).summaryOutput(
      for: Self.input(language: "de"), usage: .zero, minimumConfidence: 0.3)
    #expect(
      output.speakerNames.map(\.speakerID) == [
        LLMFixtures.standupSpeakerTwoID, LLMFixtures.standupSpeakerThreeID,
      ])
    #expect(output.speakerNames.map(\.name) == ["Nicolai", "Jérôme"])

    let strict = Self.draft(speakerNames: names).summaryOutput(
      for: Self.input(language: "de"), usage: .zero, minimumConfidence: 0.5)
    #expect(strict.speakerNames.map(\.name) == ["Nicolai"])
  }

  @Test func suggestionsNeverRenameASpeaker() {
    let input = Self.input(language: "de")
    let output = Self.draft(speakerNames: [
      DraftSpeakerName(speakerLabel: "Speaker 2", name: "Nicolai", confidence: 1, evidence: "q")
    ]).summaryOutput(for: input, usage: .zero, minimumConfidence: 0.3)
    #expect(output.summary.sections[0].bullets[0].text == "Speaker 2 sagt etwas.")
    #expect(input.speakers[1].assignment == .unknown, "the input is a value; nothing was renamed")
    #expect(output.speakerNames[0].evidence == "q")
  }

  @Test func tasksCarryOwnerPriorityAndDate() {
    let tasks = [
      DraftTask(
        text: "Angebot schicken", assignee: "Nicolai", priority: .high, dueDate: "2026-10-02"),
      DraftTask(text: "Retro planen", assignee: nil, priority: .low, dueDate: nil),
      DraftTask(
        text: "Doku schreiben", assignee: "Speaker 3", priority: .normal, dueDate: "Freitag"),
    ]
    let output = Self.draft(tasks: tasks).summaryOutput(
      for: Self.input(language: "de"), usage: .zero, minimumConfidence: 0.3)
    #expect(output.tasks.map(\.priority) == [.high, .low, .normal])
    #expect(output.tasks[0].assigneePersonID == SampleData.personNicolaiID)
    #expect(output.tasks[0].dueDate == Date(timeIntervalSince1970: 1_790_899_200))
    #expect(output.tasks[1].assigneeName == nil)
    #expect(output.tasks[1].assigneePersonID == nil)
    #expect(output.tasks[2].assigneeName == "Jérôme", "a confirmed speaker resolves to its person")
    #expect(output.tasks[2].assigneePersonID == SampleData.personJeromeID)
    #expect(output.tasks[2].dueDate == nil)
    #expect(output.tasks.allSatisfy { !$0.done })
  }

  @Test func meetingLanguageDrivesPromptAndOutput() throws {
    let german = Self.input(language: "de")
    let english = Self.input(language: nil)
    let swiss = Self.input(language: "de-CH")
    for (input, name, tag) in [
      (german, "German", "de"), (english, "English", "en"), (swiss, "German", "de-CH"),
    ] as [(SummaryInput, String, LanguageTag)] {
      let builder = SummaryPromptBuilder(template: input.template, timeZone: Self.utc)
      let single = builder.buildSingleShot(input)
      #expect(single.messages[0].content.contains("Output language: \(name)."))
      let map = builder.buildMap(
        input,
        chunk: TranscriptChunker().chunk(input.segments, language: input.meeting.language)[0],
        of: 1, notesTokens: 1_500)
      #expect(map.messages[0].content.contains("Output language: \(name)."))
      let cleanup = CleanupPromptBuilder().build(
        chunk: TranscriptChunker().chunk(input.segments, language: input.meeting.language)[0],
        language: input.meeting.language,
        glossary: CleanupInput(export: Self.standup).glossary,
        labels: SpeakerLabels(speakers: input.speakers))
      #expect(cleanup.messages[0].content.contains("Meeting language: \(name)."))
      let output = Self.draft().summaryOutput(for: input, usage: .zero, minimumConfidence: 0.3)
      #expect(output.summary.language == tag, "the renderer always gets a language")
      #expect(
        output.language == input.meeting.language,
        "the pipeline stores the tag as elected; an untagged meeting stays untagged, not English")
    }
    #expect(
      Self.draft().summaryOutput(for: english, usage: .zero, minimumConfidence: 0.3).language == nil
    )
  }

  @Test func usageIsTheSumOfEveryCallIncludingRepairsAndMissingUsage() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    let good = try String(
      contentsOf: Fixtures.url("llm/responses/summary-default-standup.json"), encoding: .utf8)
    server.enqueue(
      Scripts.completion(
        "broken {", usage: LLMUsage(promptTokens: 700, completionTokens: 3, requests: 1)),
      Scripts.completion(good, usage: nil),
    )
    let endpoint = LLMEndpoint(baseURL: server.baseURL, model: "stub-model")
    let client = OpenAICompatibleClient(endpoint: endpoint, apiKey: nil, retry: .none)
    let summarizer = LLMMeetingSummarizer(model: client, endpoint: endpoint, timeZone: Self.utc)
    let output = try await summarizer.summarize(Self.input(language: "de"))
    #expect(
      output.usage == LLMUsage(promptTokens: 700, completionTokens: 3, requests: 2),
      "a body without usage still counts as a request")

    let cleanupServer = try StubChatServer()
    defer { cleanupServer.stop() }
    cleanupServer.respond(
      with: Scripts.cleanupEcho(
        usage: LLMUsage(promptTokens: 40, completionTokens: 20, requests: 1)))
    let cleanupEndpoint = LLMEndpoint(baseURL: cleanupServer.baseURL, model: "stub-model")
    let cleaner = LLMTranscriptCleaner(
      model: OpenAICompatibleClient(endpoint: cleanupEndpoint, apiKey: nil, retry: .none),
      endpoint: cleanupEndpoint, chunker: TranscriptChunker(targetTokens: 150, maxTokens: 220))
    let cleaned = try await cleaner.clean(CleanupInput(export: Self.standup))
    let calls = cleanupServer.requests.count
    #expect(
      cleaned.usage
        == LLMUsage(promptTokens: 40 * calls, completionTokens: 20 * calls, requests: calls))
    let meetingTotal = cleaned.usage + output.usage
    #expect(meetingTotal.requests == calls + 2)
    #expect(meetingTotal.promptTokens == 40 * calls + 700)
  }
}
