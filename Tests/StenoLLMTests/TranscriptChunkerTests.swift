import Foundation
import StenoCore
import Testing

@testable import StenoLLM

@Suite struct TranscriptChunkerTests {
  static let call = LLMFixtures.customerCall60min()

  @Test func sixtyMinuteFixtureYieldsSixToTwelveChunksThatConcatenateInOrder() {
    let segments = Self.call.segments
    let chunker = TranscriptChunker()
    let chunks = chunker.chunk(segments, language: "de")
    #expect(chunks.count >= 6 && chunks.count <= 12, "\(chunks.count) chunks")
    #expect(chunks.flatMap(\.segments) == segments)
    #expect(chunks.map(\.index) == Array(0..<chunks.count))
    for chunk in chunks {
      #expect(
        chunk.estimatedTokens == TranscriptChunker.estimateTokens(chunk.segments, language: "de"))
      #expect(
        chunk.estimatedTokens <= chunker.maxTokens || chunk.segments.count == 1,
        "chunk \(chunk.index) has \(chunk.estimatedTokens) tokens")
    }
    for chunk in chunks.dropLast() {
      #expect(chunk.estimatedTokens >= chunker.targetTokens, "chunk \(chunk.index) closed early")
    }
  }

  @Test func leadingContextIsTheTailOfThePreviousChunk() {
    let chunks = TranscriptChunker(contextSegments: 3).chunk(Self.call.segments, language: "de")
    #expect(chunks.first?.leadingContext == [])
    for (previous, chunk) in zip(chunks, chunks.dropFirst()) {
      #expect(chunk.leadingContext == Array(previous.segments.suffix(3)))
      #expect(chunk.leadingContext.count == 3)
    }
  }

  @Test func chunksCloseAtASpeakerChangeOnceTheTargetIsReached() {
    let chunks = TranscriptChunker().chunk(Self.call.segments, language: "de")
    for (previous, chunk) in zip(chunks, chunks.dropFirst()) where previous.estimatedTokens <= 3_000
    {
      let boundaryIsTurn = previous.segments.last?.speakerID != chunk.segments.first?.speakerID
      let previousWasFull =
        previous.estimatedTokens
        + (chunk.segments.first.map { TranscriptChunker.estimateTokens($0, language: "de") } ?? 0)
        > 3_000
      #expect(boundaryIsTurn || previousWasFull, "chunk \(chunk.index) boundary")
    }
  }

  @Test func anOversizedSegmentGetsItsOwnChunk() {
    let meetingID = SampleData.meetingID
    func segment(_ index: Int, words: Int, speaker: UUID) -> TranscriptSegment {
      let text = Array(repeating: "wort", count: words).joined(separator: " ")
      return TranscriptSegment(
        id: SampleData.uuid(300 + index), meetingID: meetingID, start: Double(index),
        end: Double(index + 1), speakerID: speaker, lane: .mixed, text: text, rawText: text)
    }
    let a = SampleData.speakerOneID
    let b = SampleData.speakerTwoID
    let segments = [
      segment(0, words: 10, speaker: a), segment(1, words: 10, speaker: b),
      segment(2, words: 2_000, speaker: a), segment(3, words: 10, speaker: b),
      segment(4, words: 10, speaker: a),
    ]
    let chunks = TranscriptChunker(targetTokens: 50, maxTokens: 80).chunk(segments, language: "de")
    #expect(
      chunks.map { $0.segments.map(\.id) } == [
        [segments[0].id, segments[1].id], [segments[2].id], [segments[3].id, segments[4].id],
      ])
    #expect(chunks[1].estimatedTokens > 80)
    #expect(chunks[1].leadingContext == [segments[0], segments[1]])
  }

  @Test func emptyInputAndBudgetInitialiser() {
    #expect(TranscriptChunker().chunk([], language: nil).isEmpty)
    let clamped = TranscriptChunker(budget: 500)
    #expect(clamped.targetTokens == 500)
    #expect(clamped.maxTokens == 500)
    let roomy = TranscriptChunker(budget: 10_000)
    #expect(roomy.targetTokens == 2_000)
    #expect(roomy.maxTokens == 3_000)
    let inverted = TranscriptChunker(targetTokens: 100, maxTokens: 50)
    #expect(inverted.maxTokens == 100)
  }

  @Test func fixtureIsAboutTwentyThousandTokens() {
    let tokens = TranscriptChunker.estimateTokens(Self.call.segments, language: "de")
    #expect(tokens > 12_000 && tokens < 26_000, "\(tokens)")
  }
}
