import Foundation
import StenoCore

/// Pass 2: title, structured summary for the template, decisions, tasks and
/// speaker name suggestions. One call when the transcript fits the input
/// budget, else map (notes per chunk) and reduce (one call). A decode
/// failure goes once through the repair request, then fails the call.
/// Post-processing is `AnalysisDraft.summaryOutput(for:usage:minimumConfidence:)`.
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

  public func summarize(_ input: SummaryInput) async throws -> SummaryOutput {
    let builder = SummaryPromptBuilder(template: input.template, timeZone: timeZone)
    var singleShot = builder.buildSingleShot(input)
    let budget = budget(for: input, systemPrompt: singleShot.messages[0].content)
    let transcriptTokens = TranscriptChunker.estimateTokens(
      input.segments, language: input.meeting.language)

    let draft: AnalysisDraft
    let usage: LLMUsage
    if budget.fits(transcriptTokens) {
      singleShot.maxTokens = endpoint.summaryReservedOutputTokens
      (draft, usage) = try await complete(
        AnalysisDraft.self, singleShot, schema: builder.draftSchema)
    } else {
      (draft, usage) = try await mapReduce(
        input, builder: builder, budget: budget, transcriptTokens: transcriptTokens)
    }
    return draft.summaryOutput(for: input, usage: usage, minimumConfidence: minimumConfidence)
  }

  /// The budget both paths work within: the context less the reserved
  /// answer and the single-shot system prompt (the largest of the three)
  /// plus message framing. `systemPrompt` defaults to building it.
  func budget(for input: SummaryInput, systemPrompt: String? = nil) -> TokenBudget {
    let system =
      systemPrompt
      ?? SummaryPromptBuilder(template: input.template, timeZone: timeZone)
      .buildSingleShot(input).messages[0].content
    return TokenBudget(
      contextTokens: endpoint.contextTokens,
      reservedOutputTokens: endpoint.summaryReservedOutputTokens,
      promptOverheadTokens: TokenBudget.estimateTokens(system, language: "en")
        + LLMBudgetPolicy.promptFramingTokens)
  }

  /// Notes per chunk (`maxConcurrentRequests` at a time), then one reduce
  /// call over every chunk's notes. Two levels only. Each map call may
  /// spend the chunk's share of the input budget on its notes
  /// (`TokenBudget.mapNotesOutputTokens`), which is also what the up-front
  /// check reserves, so `transcriptTooLong` is thrown before the first call
  /// when the chunks are too many for that share; the post-map check only
  /// catches a model that ignored its ceiling and the prompt's length rule.
  func mapReduce(
    _ input: SummaryInput, builder: SummaryPromptBuilder, budget: TokenBudget,
    transcriptTokens: Int
  ) async throws -> (AnalysisDraft, LLMUsage) {
    let chunks = TranscriptChunker(budget: budget.inputBudget).chunk(
      input.segments, language: input.meeting.language)
    let notesTokens = budget.mapNotesOutputTokens(chunkCount: chunks.count)
    guard budget.fits(chunks.count * notesTokens) else {
      throw LLMError.transcriptTooLong(
        estimatedTokens: transcriptTokens, budget: budget.inputBudget)
    }
    let mapped = try await mapBounded(chunks, limit: endpoint.maxConcurrentRequests) {
      chunk -> (ChunkNotes, LLMUsage) in
      let request = builder.buildMap(
        input, chunk: chunk, of: chunks.count, notesTokens: notesTokens)
      var (notes, usage) = try await self.complete(
        ChunkNotes.self, request, schema: builder.notesSchema)
      notes.chunkIndex = chunk.index
      return (notes, usage)
    }
    var reduce = builder.buildReduce(input, notes: mapped.map(\.0))
    reduce.maxTokens = endpoint.summaryReservedOutputTokens
    let notesEstimate = TokenBudget.estimateTokens(reduce.messages[1].content, language: "en")
    guard budget.fits(notesEstimate) else {
      throw LLMError.transcriptTooLong(estimatedTokens: notesEstimate, budget: budget.inputBudget)
    }
    let (draft, reduceUsage) = try await complete(
      AnalysisDraft.self, reduce, schema: builder.draftSchema)
    return (draft, mapped.map(\.1).reduce(reduceUsage, +))
  }

  /// One request, decoded into `type`; an undecodable answer gets exactly
  /// one repair round. Usage sums both calls.
  func complete<T: Decodable>(
    _ type: T.Type, _ request: LLMRequest, schema: JSONSchema
  ) async throws -> (T, LLMUsage) {
    let first = try await model.complete(request)
    do {
      return (try StructuredOutputDecoder.decode(type, from: first), first.countedUsage)
    } catch LLMError.invalidJSON(let detail) {
      let repair = SummaryPromptBuilder.buildRepair(
        for: request, schema: schema, invalid: first.text, error: detail)
      let second = try await model.complete(repair)
      return (
        try StructuredOutputDecoder.decode(type, from: second),
        first.countedUsage + second.countedUsage
      )
    }
  }
}
