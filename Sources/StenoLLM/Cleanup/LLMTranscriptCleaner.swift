import Foundation
import StenoCore

/// Pass 1: the transcript in chunks through the model, at most
/// `endpoint.maxConcurrentRequests` at a time. A chunk whose answer fails
/// validation (count, indices, emptied or reworded segments) or does not
/// decode is retried once with the reason appended, then kept as raw text
/// and listed in `failedChunks`. Network and HTTP failures propagate: the
/// pipeline keeps the raw transcript and marks the stage failed. `rawText`
/// is never touched; ids, order and count come back as they went in.
public struct LLMTranscriptCleaner: TranscriptCleaner, Sendable {
  public var model: any LanguageModel
  public var endpoint: LLMEndpoint
  public var chunker: TranscriptChunker
  public var validator: CleanupValidator

  public init(
    model: any LanguageModel, endpoint: LLMEndpoint, chunker: TranscriptChunker? = nil,
    validator: CleanupValidator = CleanupValidator()
  ) {
    self.model = model
    self.endpoint = endpoint
    // Half the context for the chunk, the other half for its echo.
    self.chunker = chunker ?? TranscriptChunker(budget: max(256, endpoint.contextTokens / 2 - 512))
    self.validator = validator
  }

  struct ChunkResult: Sendable {
    var texts: [String]?
    var usage: LLMUsage
  }

  public func clean(_ input: CleanupInput) async throws -> CleanupOutput {
    let chunks = chunker.chunk(input.segments, language: input.language)
    guard !chunks.isEmpty else {
      return CleanupOutput(segments: input.segments, failedChunks: [], usage: .zero)
    }
    let glossary = Glossary(input: input)
    let labels = SpeakerLabels(speakers: input.speakers)
    let builder = CleanupPromptBuilder(maxOutputTokens: endpoint.maxOutputTokens)
    let limit = max(1, endpoint.maxConcurrentRequests)

    var results = [Int: ChunkResult]()
    try await withThrowingTaskGroup(of: (Int, ChunkResult).self) { group in
      var pending = chunks.makeIterator()
      func addNext() {
        guard let chunk = pending.next() else { return }
        group.addTask {
          let result = try await self.cleanChunk(
            chunk, language: input.language, glossary: glossary, labels: labels, builder: builder)
          return (chunk.index, result)
        }
      }
      for _ in 0..<limit { addNext() }
      while let (index, result) = try await group.next() {
        results[index] = result
        addNext()
      }
    }

    var segments = input.segments
    var offset = 0
    var failed: [Int] = []
    var usage = LLMUsage.zero
    for chunk in chunks {
      let result = results[chunk.index]
      usage = usage + (result?.usage ?? .zero)
      if let texts = result?.texts, texts.count == chunk.segments.count {
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
    _ chunk: TranscriptChunk, language: LanguageTag?, glossary: Glossary, labels: SpeakerLabels,
    builder: CleanupPromptBuilder
  ) async throws -> ChunkResult {
    var usage = LLMUsage.zero
    var request = builder.build(
      chunk: chunk, language: language, glossary: glossary, labels: labels)
    for attempt in 0..<2 {
      let response = try await model.complete(request)
      usage =
        usage + (response.usage ?? LLMUsage(promptTokens: 0, completionTokens: 0, requests: 1))
      let rejection: String
      do {
        let draft = try StructuredOutputDecoder().decode(CleanupDraft.self, from: response)
        return ChunkResult(texts: try validator.validate(draft, against: chunk), usage: usage)
      } catch let error as CleanupValidator.Rejection {
        rejection = error.description
      } catch let error as LLMError where Self.isAnswerProblem(error) {
        rejection = error.description + "."
      }
      guard attempt == 0 else { break }
      request = builder.buildRetry(request, previousAnswer: response.text, error: rejection)
    }
    return ChunkResult(texts: nil, usage: usage)
  }

  /// Failures of the answer, worth one retry; everything else propagates.
  static func isAnswerProblem(_ error: LLMError) -> Bool {
    switch error {
    case .invalidJSON, .truncated, .refused: true
    default: false
    }
  }
}
