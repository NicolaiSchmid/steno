import Foundation

/// A `LanguageModel` that answers from a queue of canned responses and
/// records every request. Throws `exhausted` when the queue runs dry.
public actor FakeLanguageModel: LanguageModel {
  public struct Exhausted: Error, Sendable, Equatable {}

  private var responses: [LLMResponse]
  public private(set) var requests: [LLMRequest] = []

  public init(responses: [LLMResponse]) {
    self.responses = responses
  }

  public func complete(_ request: LLMRequest) async throws -> LLMResponse {
    requests.append(request)
    guard !responses.isEmpty else { throw Exhausted() }
    return responses.removeFirst()
  }
}

/// A `TranscriptCleaner` that returns the segments untouched with a fixed
/// usage, so pipeline tests can assert on usage sums.
public struct PassthroughCleaner: TranscriptCleaner, Sendable {
  public var usage: LLMUsage
  public var failure: (any Error & Sendable)?
  public let calls = CallLog<Int>()

  public init(
    usage: LLMUsage = LLMUsage(promptTokens: 100, completionTokens: 50, requests: 1),
    failure: (any Error & Sendable)? = nil
  ) {
    self.usage = usage
    self.failure = failure
  }

  public func clean(_ input: CleanupInput) async throws -> CleanupOutput {
    await calls.record(input.segments.count)
    if let failure { throw failure }
    return CleanupOutput(segments: input.segments, failedChunks: [], usage: usage)
  }
}

/// A `MeetingSummarizer` that builds a deterministic `SummaryOutput` from the
/// template (one bullet per section quoting the first segment) or returns a
/// canned one; records inputs and can throw.
public struct FakeSummarizer: MeetingSummarizer, Sendable {
  public var canned: SummaryOutput?
  public var usage: LLMUsage
  public var failure: (any Error & Sendable)?
  public let calls = CallLog<String>()

  public init(
    canned: SummaryOutput? = nil,
    usage: LLMUsage = LLMUsage(promptTokens: 200, completionTokens: 100, requests: 1),
    failure: (any Error & Sendable)? = nil
  ) {
    self.canned = canned
    self.usage = usage
    self.failure = failure
  }

  public func summarize(_ input: SummaryInput) async throws -> SummaryOutput {
    await calls.record(input.template.id)
    if let failure { throw failure }
    if let canned { return canned }
    return Self.output(for: input, usage: usage)
  }

  public static func output(for input: SummaryInput, usage: LLMUsage) -> SummaryOutput {
    let firstText = input.segments.first?.text ?? "Nothing was said."
    let firstLabel = input.speakers.first?.clusterLabel ?? "Speaker 1"
    let sections = input.template.sections.map { section in
      SummarySection(
        id: section.id, heading: section.heading,
        bullets: [SummaryBullet(lead: firstLabel, text: firstText)])
    }
    return SummaryOutput(
      title: "Summary of \(input.meeting.title)",
      summary: SummaryDocument(
        templateID: input.template.id, language: input.meeting.language, sections: sections),
      decisions: ["Decision from \(firstLabel)."],
      tasks: [
        MeetingTask(
          id: MeetingStore.derivedID(input.meeting.id, salt: "fake-task-0"),
          meetingID: input.meeting.id, text: "Follow up on \(input.template.displayName).",
          assigneeName: firstLabel, priority: .normal)
      ],
      speakerNames: input.speakers.map { speaker in
        SpeakerNameSuggestion(
          speakerID: speaker.id, name: nil, confidence: 0,
          evidence: "The fake summarizer never guesses names.")
      },
      language: input.meeting.language,
      usage: usage
    )
  }
}
