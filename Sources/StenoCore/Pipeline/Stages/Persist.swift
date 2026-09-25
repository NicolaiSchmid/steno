import Foundation

extension ProcessingPipeline {
  /// Writes the mixdown for every asset that is not already AAC, marks the
  /// meeting `.ready`, and posts `speakersNeedReview` when any speaker is
  /// not `.confirmed`.
  func persist(meeting: Meeting, asset: AudioAsset, settings: Settings) async throws -> AudioAsset {
    let decoder = dependencies.decoder
    let store = self.store
    let events = dependencies.events
    return try await run(.persist, meetingID: meeting.id) {
      var updated = asset
      if asset.format != .m4aAAC {
        let folder = Self.meetingFolder(meeting.id, settings: settings)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let mixdown = folder.appendingPathComponent("audio.m4a")
        try await decoder.mixdown(asset, to: mixdown)
        updated.mixdownURL = mixdown
      }
      try await store.save(updated)
      try await store.setState(.ready, meetingID: meeting.id, now: self.now)
      let unconfirmed = try await store.speakers(meetingID: meeting.id)
        .filter { !$0.assignment.isConfirmed }
        .map(\.id)
      if !unconfirmed.isEmpty {
        await events.post(.speakersNeedReview(meetingID: meeting.id, speakerIDs: unconfirmed))
      }
      return updated
    }
  }
}
