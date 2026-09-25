import Foundation
import StenoCore
import Testing

@testable import StenoLLM

/// Packing invariants of `TranscriptChunker` over many seeded transcripts
/// and chunker sizes, not just the committed 60-minute fixture: order and
/// completeness, sequential indices, the `maxTokens` bound (except for a
/// single oversized segment), the close rule (a speaker turn once the target
/// is reached, or the next segment would overflow), and the leading context.
@Suite struct ChunkerPropertyTests {
  struct Case: CustomStringConvertible, Sendable {
    var seed: UInt64
    var segmentCount: Int
    var targetTokens: Int
    var maxTokens: Int
    var contextSegments: Int
    var language: LanguageTag?

    var description: String {
      "seed \(seed), \(segmentCount) segments, target \(targetTokens), max \(maxTokens), context \(contextSegments), \(language?.rawValue ?? "untagged")"
    }
  }

  static let cases: [Case] = {
    var cases: [Case] = []
    let sizes: [(Int, Int, Int)] = [(50, 80, 3), (150, 220, 2), (400, 400, 0), (2_000, 3_000, 3)]
    let counts = [1, 2, 7, 60, 300]
    var seed: UInt64 = 1
    for (target, maximum, context) in sizes {
      for count in counts {
        cases.append(
          Case(
            seed: seed, segmentCount: count, targetTokens: target, maxTokens: maximum,
            contextSegments: context, language: seed.isMultiple(of: 2) ? "en" : "de"))
        seed += 1
      }
    }
    cases.append(
      Case(
        seed: 99, segmentCount: 120, targetTokens: 100, maxTokens: 150, contextSegments: 3,
        language: nil))
    return cases
  }()

  static func transcript(_ testCase: Case) -> [TranscriptSegment] {
    let meetingID = SampleData.uuid(400)
    return SyntheticTranscript.generate(
      meetingID: meetingID, seed: testCase.seed,
      speakers: [
        .init(id: SampleData.uuid(410), lane: .mic, role: .vendor, weight: 3),
        .init(id: SampleData.uuid(411), lane: .system, role: .customer, weight: 4),
        .init(id: SampleData.uuid(412), lane: .system, role: .colleague, weight: 1),
      ],
      segmentCount: testCase.segmentCount, segmentSeconds: 4)
  }

  static func check(
    _ chunks: [TranscriptChunk], against segments: [TranscriptSegment],
    chunker: TranscriptChunker, language: LanguageTag?, label: String
  ) {
    #expect(chunks.flatMap(\.segments) == segments, "\(label): concatenation")
    #expect(chunks.map(\.index) == Array(0..<chunks.count), "\(label): indices")
    #expect(chunks.allSatisfy { !$0.segments.isEmpty }, "\(label): no empty chunk")
    for chunk in chunks {
      #expect(
        chunk.estimatedTokens
          == TranscriptChunker.estimateTokens(chunk.segments, language: language),
        "\(label): chunk \(chunk.index) estimate")
      #expect(
        chunk.estimatedTokens <= chunker.maxTokens || chunk.segments.count == 1,
        "\(label): chunk \(chunk.index) has \(chunk.estimatedTokens) tokens in \(chunk.segments.count) segments"
      )
    }
    #expect(chunks.first?.leadingContext.isEmpty != false, "\(label): first chunk has no context")
    // Once a chunk has reached the target, it closes at the next speaker
    // change: no chunk contains a turn boundary past its target.
    for chunk in chunks {
      var tokens = 0
      for (previous, segment) in zip(chunk.segments, chunk.segments.dropFirst()) {
        tokens += TranscriptChunker.estimateTokens(previous, language: language)
        #expect(
          tokens < chunker.targetTokens || segment.speakerID == previous.speakerID,
          "\(label): chunk \(chunk.index) runs past a speaker turn at \(tokens) tokens")
      }
    }
    for (previous, chunk) in zip(chunks, chunks.dropFirst()) {
      #expect(
        chunk.leadingContext == Array(previous.segments.suffix(chunker.contextSegments)),
        "\(label): chunk \(chunk.index) context")
      guard let next = chunk.segments.first, let last = previous.segments.last else { continue }
      let nextTokens = TranscriptChunker.estimateTokens(next, language: language)
      let wouldOverflow = previous.estimatedTokens + nextTokens > chunker.maxTokens
      let turnAfterTarget =
        previous.estimatedTokens >= chunker.targetTokens && next.speakerID != last.speakerID
      #expect(
        wouldOverflow || turnAfterTarget,
        "\(label): chunk \(previous.index) closed at \(previous.estimatedTokens) tokens without a reason"
      )
    }
  }

  @Test(arguments: cases)
  func packingInvariantsHoldOnSeededTranscripts(testCase: Case) {
    let segments = Self.transcript(testCase)
    let chunker = TranscriptChunker(
      targetTokens: testCase.targetTokens, maxTokens: testCase.maxTokens,
      contextSegments: testCase.contextSegments)
    let chunks = chunker.chunk(segments, language: testCase.language)
    #expect(!chunks.isEmpty)
    Self.check(
      chunks, against: segments, chunker: chunker, language: testCase.language,
      label: testCase.description)
  }

  @Test func oversizedSegmentsAreAloneAndEverythingElseStaysBounded() {
    var segments = Self.transcript(
      Case(
        seed: 7, segmentCount: 40, targetTokens: 60, maxTokens: 90, contextSegments: 3,
        language: "de"))
    let long = Array(repeating: "wort", count: 500).joined(separator: " ")
    for index in [5, 6, 20] {
      segments[index].text = long
      segments[index].rawText = long
    }
    let chunker = TranscriptChunker(targetTokens: 60, maxTokens: 90)
    let chunks = chunker.chunk(segments, language: "de")
    Self.check(chunks, against: segments, chunker: chunker, language: "de", label: "oversized")
    let oversized = chunks.filter { $0.estimatedTokens > chunker.maxTokens }
    #expect(oversized.count == 3)
    #expect(oversized.allSatisfy { $0.segments.count == 1 })
    #expect(
      oversized.map { $0.segments[0].id } == [segments[5].id, segments[6].id, segments[20].id])
  }

  @Test func theBudgetInitialiserNeverProducesAChunkOverTheBudget() {
    for budget in [64, 300, 1_000, 5_000] {
      let chunker = TranscriptChunker(budget: budget)
      for seed in UInt64(1)...4 {
        let segments = Self.transcript(
          Case(
            seed: seed, segmentCount: 150, targetTokens: 0, maxTokens: 0, contextSegments: 3,
            language: "de"))
        let chunks = chunker.chunk(segments, language: "de")
        Self.check(
          chunks, against: segments, chunker: chunker, language: "de", label: "budget \(budget)")
        #expect(
          chunks.allSatisfy { $0.estimatedTokens <= budget || $0.segments.count == 1 },
          "budget \(budget), seed \(seed)")
      }
    }
  }
}
