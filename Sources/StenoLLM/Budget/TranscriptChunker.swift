import Foundation
import StenoCore

/// A run of consecutive segments that fits one request, with the tail of
/// the previous chunk as read-only context.
public struct TranscriptChunk: Sendable, Equatable {
  public var index: Int
  public var segments: [TranscriptSegment]
  public var leadingContext: [TranscriptSegment]
  /// Estimated tokens of `segments` as transcript lines, context excluded.
  public var estimatedTokens: Int
}

/// Splits a transcript on segment boundaries. A chunk grows to at least
/// `targetTokens`, then closes at the next speaker change, or at the segment
/// that would push it past `maxTokens`. A single segment larger than
/// `maxTokens` gets a chunk of its own. Order is preserved; concatenating
/// every chunk's `segments` yields the input.
public struct TranscriptChunker: Sendable, Equatable {
  public var targetTokens: Int
  public var maxTokens: Int
  public var contextSegments: Int

  /// Tokens for the `[n] Speaker 1: ` framing of one transcript line.
  public static let lineOverheadTokens = 6

  public init(targetTokens: Int = 2_000, maxTokens: Int = 3_000, contextSegments: Int = 3) {
    self.targetTokens = max(1, targetTokens)
    self.maxTokens = max(self.targetTokens, maxTokens)
    self.contextSegments = max(0, contextSegments)
  }

  /// A chunker whose chunks never exceed `budget`, keeping the defaults
  /// where the budget allows.
  public init(budget: Int, contextSegments: Int = 3) {
    self.init(
      targetTokens: min(2_000, budget), maxTokens: min(3_000, budget),
      contextSegments: contextSegments)
  }

  public static func estimateTokens(_ segment: TranscriptSegment, language: LanguageTag?) -> Int {
    TokenBudget.estimateTokens(segment.text, language: language) + lineOverheadTokens
  }

  public static func estimateTokens(_ segments: [TranscriptSegment], language: LanguageTag?) -> Int
  {
    segments.reduce(0) { $0 + estimateTokens($1, language: language) }
  }

  public func chunk(_ segments: [TranscriptSegment], language: LanguageTag?) -> [TranscriptChunk] {
    var chunks: [TranscriptChunk] = []
    var current: [TranscriptSegment] = []
    var currentTokens = 0

    func close() {
      guard !current.isEmpty else { return }
      let previous = chunks.last?.segments ?? []
      chunks.append(
        TranscriptChunk(
          index: chunks.count, segments: current,
          leadingContext: Array(previous.suffix(contextSegments)),
          estimatedTokens: currentTokens))
      current = []
      currentTokens = 0
    }

    for segment in segments {
      let tokens = Self.estimateTokens(segment, language: language)
      if !current.isEmpty {
        let exceedsMax = currentTokens + tokens > maxTokens
        let turnAfterTarget =
          currentTokens >= targetTokens && segment.speakerID != current.last?.speakerID
        if exceedsMax || turnAfterTarget { close() }
      }
      current.append(segment)
      currentTokens += tokens
    }
    close()
    return chunks
  }
}
