/// Known voices across meetings: pure math over `MeetingStore.persons()`.
public protocol SpeakerMemory: Sendable {
  /// Candidates ranked by cosine similarity, best first; feeds the review
  /// sheet.
  func candidates(for embedding: Embedding, limit: Int) async throws -> [SpeakerMatch]
  /// Folds the embedding into the person's running mean with a capped sample
  /// count, and saves the person.
  func enroll(_ embedding: Embedding, as person: Person) async throws
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
