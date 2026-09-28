import Foundation

extension ProcessingPipeline {
  /// Stamps `expiresAt` from the asset's retention: now for
  /// `.deleteAfterProcessing`, now plus the days for `.keepDays`, nil for
  /// `.keepForever`, then posts `retentionApplied`. `RetentionSweep` does
  /// the deleting; the app runs it on that event.
  ///
  /// Deletion waits for delivery: when any `Delivery` row of the meeting is
  /// not `.delivered` the asset is left unstamped and nothing is posted, not
  /// even the stage's `progress`, so the Obsidian audio copy can still be
  /// made by a later Re-export. `redeliver` and `rerunSummary` stamp that
  /// deferred asset once every delivery has succeeded
  /// (`stampDeferredRetention`). A meeting without destinations has no rows
  /// and is stamped at once.
  ///
  /// The row is read again before the write: a keep toggled or a
  /// `RetentionSweep.keepAll()` run while the meeting was processing has
  /// changed the retention since `process` loaded it, and that choice is
  /// what gets stamped, never the copy from the start of the run.
  func retention(asset: AudioAsset) async throws {
    let store = self.store
    let events = dependencies.events
    let meetingID = asset.meetingID
    let delivered = try await attributing(.retention) {
      try await store.deliveries(meetingID: meetingID).allDelivered
    }
    guard delivered else { return }
    try await run(.retention, meetingID: meetingID) {
      var updated = try await store.asset(id: asset.id) ?? asset
      updated.expiresAt = updated.retention.expiry(from: self.now)
      try await store.save(updated)
      await events.post(.retentionApplied(meetingID: meetingID))
    }
  }

  /// The deferred case after `deliver` ran again: an asset with a finite
  /// retention and no stamp gets one now if every delivery succeeded. An
  /// asset that is already stamped is never restamped, so a Re-export does
  /// not extend a 30 day expiry; a `.keepForever` asset (the per-meeting
  /// keep, or `RetentionSweep.keepAll()`) is never touched. Only a `.ready`
  /// meeting is stamped: a failed one needs its audio to be processed
  /// again, whatever its deliveries say. An asset whose master is already
  /// gone (swept, or removed by hand) is not stamped again either.
  func stampDeferredRetention(meetingID: UUID) async throws {
    guard let asset = try await store.asset(meetingID: meetingID),
      asset.retention != .keepForever, asset.expiresAt == nil,
      try await store.meeting(id: meetingID)?.state == .ready,
      FileManager.default.fileExists(atPath: asset.url.path)
    else { return }
    try await retention(asset: asset)
  }

  /// The per-meeting keep: `rule` replaces the asset's retention and clears
  /// its stamp, then the deferred-case rules decide whether a new stamp is
  /// written now (`stampDeferredRetention`: a finite rule on a `.ready`
  /// meeting whose every delivery succeeded). `.keepForever` never stamps.
  /// Safe while the meeting is processing: the stages read the row again
  /// before they write it.
  public func applyRetention(meetingID: UUID, rule: AudioRetention) async throws {
    guard var asset = try await store.asset(meetingID: meetingID) else {
      throw PipelineFailure(stage: .retention, reason: "meeting \(meetingID) has no audio asset")
    }
    asset.retention = rule
    asset.expiresAt = nil
    try await attributing(.retention) { try await store.save(asset) }
    try await stampDeferredRetention(meetingID: meetingID)
  }
}
