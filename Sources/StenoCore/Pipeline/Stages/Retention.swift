import Foundation

extension ProcessingPipeline {
  /// Stamps `expiresAt` from the asset's retention: now for
  /// `.deleteAfterProcessing`, now plus the days for `.keepDays`, nil for
  /// `.keepForever`. `RetentionSweep` does the deleting.
  func retention(asset: AudioAsset) async throws {
    let store = self.store
    try await run(.retention, meetingID: asset.meetingID) {
      var updated = asset
      updated.expiresAt = asset.retention.expiry(from: self.now)
      try await store.save(updated)
    }
  }
}
