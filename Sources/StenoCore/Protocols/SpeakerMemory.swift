/// Known voices across meetings: pure math over `MeetingStore.persons()`.
/// A person's voice (`Person.embedding` and `sampleCount`) is written by the
/// store: `MeetingStore.confirm` and the merges recompute it from the
/// person's confirmed speakers (`refreshVoice`). This protocol only reads it.
public protocol SpeakerMemory: Sendable {
  /// Candidates ranked by cosine similarity, best first; feeds `match` and
  /// the speaker picker.
  func candidates(for embedding: Embedding, limit: Int) async throws -> [SpeakerMatch]
}

extension SpeakerMemory {
  /// The best candidate at or above `threshold` that is at least `margin`
  /// above the runner-up; nil otherwise.
  public func match(_ embedding: Embedding, threshold: Float, margin: Float = 0.05) async throws
    -> SpeakerMatch?
  {
    let ranked = try await candidates(for: embedding, limit: 2)
    guard let best = ranked.first, best.similarity >= threshold else { return nil }
    if ranked.count > 1, best.similarity - ranked[1].similarity < margin { return nil }
    return best
  }
}
