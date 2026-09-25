import Foundation

extension ProcessingPipeline {
  /// `SpeakerMemory.match` at `settings.speakerMatchThreshold` for every
  /// speaker with an embedding: a hit becomes `.suggested`, a miss stays
  /// `.unknown`. Nothing is confirmed here; that is the review sheet's job.
  func matchSpeakers(_ speakers: [Speaker], meetingID: UUID, settings: Settings) async throws
    -> [Speaker]
  {
    let memory = dependencies.speakerMemory
    return try await run(.matchSpeakers, meetingID: meetingID) {
      var matched: [Speaker] = []
      for var speaker in speakers {
        if let embedding = speaker.embedding,
          let match = try await memory.match(embedding, threshold: settings.speakerMatchThreshold)
        {
          speaker.assignment = .suggested(personID: match.person.id, similarity: match.similarity)
        } else {
          speaker.assignment = .unknown
        }
        matched.append(speaker)
      }
      return matched
    }
  }
}
