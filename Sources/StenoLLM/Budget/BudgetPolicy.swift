import Foundation

/// The numbers a small context window makes someone tune, in one place and
/// with units in every name. `LLMEndpoint` derives its budgets from them so
/// the app can show "cleanup chunks of N tokens, summary answers of M" next
/// to the context setting.
public enum LLMBudgetPolicy {
  /// Cleanup: half the context for the chunk, the other half for its echo,
  /// less this reserve for the system prompt and framing.
  public static let cleanupContextReserveTokens = 512
  /// The smallest chunk budget worth sending.
  public static let cleanupChunkFloorTokens = 256

  /// The cleanup answer repeats the chunk as JSON: about this much framing
  /// per segment (`{"index": n, "text": ""}` and separators)...
  public static let cleanupFramingTokensPerSegment = 12
  /// ...plus this much for the envelope, then a third of headroom, never
  /// under the floor.
  public static let cleanupAnswerFixedTokens = 64
  public static let cleanupAnswerHeadroomNumerator = 4
  public static let cleanupAnswerHeadroomDenominator = 3
  public static let cleanupAnswerFloorTokens = 256

  /// Summary: the answer gets at most this fraction of the context (a
  /// quarter), capped by the endpoint's `maxOutputTokens`, at least the floor.
  public static let summaryOutputContextDivisor = 4
  public static let summaryOutputFloorTokens = 256

  /// Map: one chunk's notes may take its share of the input budget, at most
  /// the ceiling, at least the floor (below it the chunk cannot be carried
  /// and the pass refuses before the first call).
  public static let mapNotesCeilingTokens = 1_500
  public static let mapNotesFloorTokens = 256
  /// One notes point, a sentence plus its JSON framing, costs about this
  /// many tokens; the map prompt's length rule follows from the ceiling.
  public static let tokensPerNotesPoint = 60
  public static let mapNotesMinimumPoints = 3

  /// Message framing beyond the system prompt text (role markers, the user
  /// message's header line), counted against the input budget.
  public static let promptFramingTokens = 64
}

extension LLMEndpoint {
  /// Tokens one cleanup chunk may hold: half the context less the reserve.
  public var cleanupChunkBudgetTokens: Int {
    max(
      LLMBudgetPolicy.cleanupChunkFloorTokens,
      contextTokens / 2 - LLMBudgetPolicy.cleanupContextReserveTokens)
  }

  /// Tokens kept for a summary answer: the endpoint's ceiling, at most a
  /// quarter of the context.
  public var summaryReservedOutputTokens: Int {
    max(
      LLMBudgetPolicy.summaryOutputFloorTokens,
      min(maxOutputTokens, contextTokens / LLMBudgetPolicy.summaryOutputContextDivisor))
  }
}
