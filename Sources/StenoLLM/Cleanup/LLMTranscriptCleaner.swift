import Foundation
import StenoCore

/// Pass 1: the transcript in chunks through the model, at most
/// `endpoint.maxConcurrentRequests` at a time. A chunk whose answer is refused, fails
/// validation (count, indices, emptied or reworded segments) or does not
/// decode is retried once with the reason appended, then kept as raw text
/// and listed in `failedChunks`. Network and HTTP failures propagate: the
/// pipeline keeps the raw transcript and marks the stage failed. `rawText`
/// is never touched; ids, order and count come back as they went in.
public struct LLMTranscriptCleaner: TranscriptCleaner, Sendable {
  public var model: any LanguageModel
  public var endpoint: LLMEndpoint
  public var chunker: TranscriptChunker

  public init(model: any LanguageModel, endpoint: LLMEndpoint, chunker: TranscriptChunker? = nil) {
    self.model = model
    self.endpoint = endpoint
    self.chunker = chunker ?? TranscriptChunker(budget: endpoint.cleanupChunkBudgetTokens)
  }

  public func clean(_ input: CleanupInput) async throws -> CleanupOutput {
    let chunks = chunker.chunk(input.segments, language: input.language)
    let glossary = input.glossary
    let labels = SpeakerLabels(speakers: input.speakers)
    let builder = CleanupPromptBuilder(maxOutputTokens: endpoint.maxOutputTokens)

    let results = try await mapBounded(chunks, limit: endpoint.maxConcurrentRequests) { chunk in
      try await self.cleanChunk(
        builder.build(chunk: chunk, language: input.language, glossary: glossary, labels: labels),
        chunk: chunk, builder: builder)
    }

    var segments = input.segments
    var offset = 0
    var failed: [Int] = []
    var usage = LLMUsage.zero
    for (chunk, (texts, chunkUsage)) in zip(chunks, results) {
      usage = usage + chunkUsage
      if let texts {
        for (position, text) in texts.enumerated() {
          segments[offset + position].text = text
        }
      } else {
        failed.append(chunk.index)
      }
      offset += chunk.segments.count
    }
    return CleanupOutput(segments: segments, failedChunks: failed, usage: usage)
  }

  /// One chunk: request, validate, one retry with the reason, else nil.
  func cleanChunk(
    _ request: LLMRequest, chunk: TranscriptChunk, builder: CleanupPromptBuilder
  ) async throws -> (texts: [String]?, usage: LLMUsage) {
    var request = request
    var usage = LLMUsage.zero
    for attempt in 0..<2 {
      let response: LLMResponse
      let rejection: String
      do {
        response = try await model.complete(request)
      } catch let error as LLMError where error.isAnswerProblem {
        // A refusal arrives as an error from the client, with no body to
        // validate; it costs a request all the same.
        usage = usage + LLMUsage(promptTokens: 0, completionTokens: 0, requests: 1)
        guard attempt == 0 else { break }
        request = builder.buildRetry(request, previousAnswer: "", error: error.description + ".")
        continue
      }
      usage = usage + response.countedUsage
      do {
        let draft = try StructuredOutputDecoder.decode(CleanupDraft.self, from: response)
        let problems = draft.problems(against: chunk)
        if problems.isEmpty { return (draft.orderedTexts, usage) }
        rejection = problems.joined(separator: " ")
      } catch let error as LLMError where error.isAnswerProblem {
        rejection = error.description + "."
      }
      guard attempt == 0 else { break }
      request = builder.buildRetry(request, previousAnswer: response.text, error: rejection)
    }
    return (nil, usage)
  }
}
