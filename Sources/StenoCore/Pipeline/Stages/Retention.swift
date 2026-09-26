import Foundation

extension ProcessingPipeline {
  /// Stamps `expiresAt` from the asset's retention: now for
  /// `.deleteAfterProcessing`, now plus the days for `.keepDays`, nil for
  /// `.keepForever`, then posts `retentionApplied`. `RetentionSweep` does
  /// the deleting; the app runs it on that event.
  func retention(asset: AudioAsset) async throws {
    let store = self.store
    let events = dependencies.events
    try await run(.retention, meetingID: asset.meetingID) {
      var updated = asset
      updated.expiresAt = asset.retention.expiry(from: self.now)
      try await store.save(updated)
      await events.post(.retentionApplied(meetingID: asset.meetingID))
    }
  }
}
