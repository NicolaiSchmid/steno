import Foundation
import StenoCore
import Testing

@testable import StenoLLM

/// The cleanup contract from the model's side: merged, duplicated,
/// reordered, shortened, expanded and emptied answers are rejected with a
/// prompt-ready reason, the one-word allowance and the 0.7 to 1.3 ratio are
/// exact at their boundaries, a retry that comes back right is applied, and
/// several failing chunks are all listed while the rest is cleaned.
@Suite struct CleanupValidationTests {
  static let standup = LLMFixtures.denglishStandup()
  static let chunk = TranscriptChunker().chunk(standup.segments, language: "de")[0]

  static func echo(_ chunk: TranscriptChunk) -> CleanupDraft {
    CleanupDraft(
      segments: chunk.segments.enumerated().map { .init(index: $0.offset, text: $0.element.text) })
  }

  static func words(_ count: Int) -> String {
    Array(repeating: "wort", count: count).joined(separator: " ")
  }

  @Test func mergedAnswersAreRejectedByCountAndByDuplicateIndex() {
    let chunk = Self.chunk
    let count = chunk.segments.count
    var merged = Self.echo(chunk)
    merged.segments[0].text += " " + merged.segments[1].text
    merged.segments.remove(at: 1)
    #expect(
      merged.problems(against: chunk) == [
        "Expected \(count) segments, got \(count - 1).",
        "Indices must be 0 to \(count - 1), each exactly once; got "
          + ([0] + Array(2..<count)).map(String.init).joined(separator: ", ") + ".",
      ])
    // Renumbering the merged answer hides nothing: the count still fails.
    for index in merged.segments.indices { merged.segments[index].index = index }
    #expect(
      merged.problems(against: chunk).first == "Expected \(count) segments, got \(count - 1).")
    #expect(merged.problems(against: chunk).count == 2)

    var duplicated = Self.echo(chunk)
    duplicated.segments[1].index = 0
    let problems = duplicated.problems(against: chunk)
    #expect(problems.count == 1)
    #expect(
      problems.first?.hasPrefix("Indices must be 0 to \(count - 1), each exactly once;") == true)

    var added = Self.echo(chunk)
    added.segments.append(.init(index: count, text: "extra"))
    #expect(added.problems(against: chunk).count == 2, "count and indices both fail")

    var negative = Self.echo(chunk)
    negative.segments[0].index = -1
    #expect(negative.problems(against: chunk).first?.hasPrefix("Indices must be") == true)
  }

  @Test func reorderedAnswersAreAcceptedAndReadBackInSegmentOrder() {
    let chunk = Self.chunk
    var shuffled = Self.echo(chunk)
    shuffled.segments.reverse()
    #expect(shuffled.problems(against: chunk) == [])
    #expect(shuffled.orderedTexts == chunk.segments.map(\.text))
  }

  @Test func wordRatioBoundariesAreExact() {
    let meetingID = SampleData.meetingID
    func chunk(words: Int) -> TranscriptChunk {
      let text = Self.words(words)
      let segment = TranscriptSegment(
        id: SampleData.uuid(500), meetingID: meetingID, start: 0, end: 1,
        speakerID: SampleData.speakerOneID, lane: .mixed, text: text, rawText: text)
      return TranscriptChunk(index: 0, segments: [segment], leadingContext: [], estimatedTokens: 1)
    }
    func problems(original: Int, cleaned: Int) -> [String] {
      CleanupDraft(segments: [.init(index: 0, text: Self.words(cleaned))])
        .problems(against: chunk(words: original))
    }
    // Ratio 0.7 and 1.3 are inside the range; one word past them is outside.
    #expect(problems(original: 10, cleaned: 7) == [])
    #expect(problems(original: 10, cleaned: 13) == [])
    #expect(
      problems(original: 10, cleaned: 6) == [
        "Segment 0 changed from 10 to 6 words; keep the wording, only fix spelling, casing and punctuation."
      ])
    #expect(problems(original: 10, cleaned: 14).count == 1)
    // A difference of one word passes whatever the ratio says ("Git Hub").
    #expect(problems(original: 2, cleaned: 1) == [])
    #expect(problems(original: 1, cleaned: 2) == [])
    #expect(
      problems(original: 3, cleaned: 1) == [
        "Segment 0 changed from 3 to 1 words; keep the wording, only fix spelling, casing and punctuation."
      ])
    // Emptied is its own message; an empty original accepts anything.
    #expect(problems(original: 5, cleaned: 0) == ["Segment 0 came back empty."])
    #expect(
      CleanupDraft(segments: [.init(index: 0, text: "   \n")]).problems(against: chunk(words: 5))
        == ["Segment 0 came back empty."])
    #expect(problems(original: 0, cleaned: 4) == [])
    #expect(CleanupDraft.wordCount("  a\tb\nc  ") == 3)
  }

  @Test func severalRewordedSegmentsAreAllReported() {
    let chunk = Self.chunk
    var draft = Self.echo(chunk)
    draft.segments[1].text = "Kurz."
    draft.segments[4].text = ""
    let problems = draft.problems(against: chunk)
    #expect(problems.count == 2)
    #expect(problems[0].hasPrefix("Segment 1 changed from"))
    #expect(problems[1] == "Segment 4 came back empty.")
  }

  @Test func aRetryThatComesBackRightIsAppliedAndNotAFailedChunk() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    let chunker = TranscriptChunker(targetTokens: 150, maxTokens: 220)
    let input = CleanupInput(export: Self.standup)
    let chunks = chunker.chunk(input.segments, language: input.language)
    try #require(chunks.count >= 3)
    let victim = try #require(chunks[1].segments.first?.text)
    // First answer for the victim chunk drops a segment; the retry is perfect
    // and capitalises every segment so the applied text is recognisable.
    server.respond { request in
      guard let firstUser = request.chat?.messages.first(where: { $0.role == "user" })?.content
      else { return nil }
      let segments = Scripts.parseSegments(firstUser)
      let isVictim = segments.first?.text == victim
      let isRetry = request.purpose == "cleanup-retry"
      let answer = (isVictim && !isRetry) ? Array(segments.dropLast()) : segments
      return Scripts.json(
        CleanupDraft(
          segments: answer.map {
            .init(index: $0.index, text: $0.text.prefix(1).uppercased() + $0.text.dropFirst())
          }))
    }
    let cleaner = CleanupTests.cleaner(server, chunker: chunker)
    let output = try await cleaner.clean(input)
    #expect(output.failedChunks == [])
    #expect(output.segments.allSatisfy { $0.text.first?.isUppercase == true })
    #expect(output.segments.map(\.rawText) == input.segments.map(\.rawText))
    let retries = server.requests.filter { $0.purpose == "cleanup-retry" }
    #expect(retries.count == 1)
    #expect(server.requests.count == chunks.count + 1)
    #expect(output.usage.requests == chunks.count + 1)
    let victimOffset = chunks[0].segments.count
    #expect(output.segments[victimOffset].text.hasPrefix(victim.prefix(1).uppercased()))
  }

  @Test func severalFailingChunksAreListedInOrderWhileTheRestIsCleaned() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    let chunker = TranscriptChunker(targetTokens: 150, maxTokens: 220)
    let input = CleanupInput(export: Self.standup)
    let chunks = chunker.chunk(input.segments, language: input.language)
    try #require(chunks.count >= 3)
    let firstTexts = Set(
      [chunks[0], chunks[chunks.count - 1]].compactMap { $0.segments.first?.text })
    server.respond { request in
      guard let firstUser = request.chat?.messages.first(where: { $0.role == "user" })?.content
      else { return nil }
      let segments = Scripts.parseSegments(firstUser)
      if let first = segments.first?.text, firstTexts.contains(first) {
        // Halves every segment: a word-ratio failure both times.
        return Scripts.json(
          CleanupDraft(
            segments: segments.map { .init(index: $0.index, text: Self.words(1)) }))
      }
      return Scripts.json(
        CleanupDraft(
          segments: segments.map {
            .init(index: $0.index, text: $0.text.prefix(1).uppercased() + $0.text.dropFirst())
          }))
    }
    let output = try await CleanupTests.cleaner(server, chunker: chunker).clean(input)
    #expect(output.failedChunks == [0, chunks.count - 1])
    #expect(output.segments.count == input.segments.count)
    var offset = 0
    for chunk in chunks {
      let texts = output.segments[offset..<offset + chunk.segments.count].map(\.text)
      if output.failedChunks.contains(chunk.index) {
        #expect(texts == chunk.segments.map(\.text), "chunk \(chunk.index) kept raw")
      } else {
        #expect(texts.allSatisfy { $0.first?.isUppercase == true }, "chunk \(chunk.index) cleaned")
      }
      offset += chunk.segments.count
    }
    #expect(server.requests.count == chunks.count + 2)
    let retry = try #require(server.requests.first { $0.purpose == "cleanup-retry" }?.chat)
    #expect(retry.messages.last?.content.contains("keep the wording") == true)
  }

  @Test func aRefusalFallsBackToRawAfterOneRetry() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    server.enqueue(Scripts.refusal("I will not edit this."), Scripts.refusal("Still no."))
    let output = try await CleanupTests.cleaner(server).clean(CleanupInput(export: Self.standup))
    #expect(output.failedChunks == [0])
    #expect(output.segments == Self.standup.segments)
    #expect(server.requests.count == 2)
    let retry = try #require(server.requests.last?.chat)
    try #require(retry.messages.count == 4, "the retry carries the rejected answer and the reason")
    #expect(retry.messages[2].role == "assistant")
    #expect(retry.messages[3].content.contains("the model refused: I will not edit this."))
    #expect(output.usage.requests == 2, "a refusal still cost a request")
  }
}
