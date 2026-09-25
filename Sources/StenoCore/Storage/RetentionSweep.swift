import Foundation

/// Removes the audio files of every asset whose `expiresAt` has passed:
/// master, sidecars and mixdown together, never sample clips. The row keeps
/// its URLs and loses `expiresAt`, so a sweep runs once per expiry. A missing
/// file is skipped, not an error. The app runs this at launch and after
/// every processed meeting.
public struct RetentionSweep: Sendable {
  public var store: MeetingStore

  public init(store: MeetingStore) {
    self.store = store
  }

  /// The files actually removed, in asset then file order.
  @discardableResult
  public func run(now: Date) async throws -> [URL] {
    var removed: [URL] = []
    for asset in try await store.expiredAssets(now: now) {
      for url in asset.expirableFiles where FileManager.default.fileExists(atPath: url.path) {
        try FileManager.default.removeItem(at: url)
        removed.append(url)
      }
      var swept = asset
      swept.expiresAt = nil
      try await store.save(swept)
    }
    return removed
  }
}
