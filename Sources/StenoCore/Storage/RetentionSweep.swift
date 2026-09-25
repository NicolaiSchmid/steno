import Foundation

/// Removes the audio files of every asset whose `expiresAt` has passed:
/// master, sidecars and mixdown together, never sample clips. The row keeps
/// its URLs and loses `expiresAt`, so a sweep runs once per expiry. A missing
/// file is skipped, not an error. The app runs this at launch and after
/// every processed meeting.
public struct RetentionSweep: Sendable {
  public var store: MeetingStore
  /// `FileManager` is not `Sendable` in Apple's Foundation although
  /// `FileManager.default` is documented thread-safe; callers pass that one.
  public nonisolated(unsafe) var fileManager: FileManager

  public init(store: MeetingStore, fileManager: FileManager = .default) {
    self.store = store
    self.fileManager = fileManager
  }

  /// The files actually removed, in asset then file order.
  @discardableResult
  public func run(now: Date) async throws -> [URL] {
    var removed: [URL] = []
    for asset in try await store.expiredAssets(now: now) {
      for url in asset.expirableFiles where fileManager.fileExists(atPath: url.path) {
        try fileManager.removeItem(at: url)
        removed.append(url)
      }
      var swept = asset
      swept.expiresAt = nil
      try await store.save(swept)
    }
    return removed
  }
}
