import Foundation

extension ProcessingPipeline {
  /// Stamps `expiresAt` from the asset's retention: now for
  /// `.deleteAfterProcessing`, now plus the days for `.keepDays`, nil for
  /// `.keepForever`, then posts `retentionApplied`. `RetentionSweep` does
  /// the deleting; the app runs it on that event.
  ///
  /// Deletion waits for delivery: when any `Delivery` row of the meeting is
  /// not `.delivered` the asset is left unstamped and nothing is posted, so
  /// the Obsidian audio copy can still be made by a later Re-export.
  /// `redeliver` and `rerunSummary` stamp that deferred asset once every
  /// delivery has succeeded (`stampDeferredRetention`). A meeting without
  /// destinations has no rows and is stamped at once.
  func retention(asset: AudioAsset) async throws {
    let store = self.store
    let events = dependencies.events
    try await run(.retention, meetingID: asset.meetingID) {
      let deliveries = try await store.deliveries(meetingID: asset.meetingID)
      guard deliveries.allSatisfy({ $0.status == .delivered }) else { return }
      var updated = asset
      updated.expiresAt = asset.retention.expiry(from: self.now)
      try await store.save(updated)
      await events.post(.retentionApplied(meetingID: asset.meetingID))
    }
  }

  /// The deferred case after `deliver` ran again: an asset with a finite
  /// retention and no stamp gets one now if every delivery succeeded. An
  /// asset that is already stamped is never restamped, so a Re-export does
  /// not extend a 30 day expiry; a `.keepForever` asset (the per-meeting
  /// keep, or `RetentionSweep.keepAll()`) is never touched.
  func stampDeferredRetention(meetingID: UUID) async throws {
    guard let asset = try await store.asset(meetingID: meetingID),
      asset.retention != .keepForever, asset.expiresAt == nil
    else { return }
    try await retention(asset: asset)
  }
}
