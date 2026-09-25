import Foundation

/// Removes the audio files of every asset whose `expiresAt` has passed:
/// master, sidecars and mixdown together, never sample clips. The row keeps
/// its URLs and loses `expiresAt` once every file is gone, so a sweep runs
/// once per expiry. A missing file is skipped, not an error. A file that
/// cannot be removed (permissions, read-only volume) leaves `expiresAt` set
/// on its asset so the next sweep retries it, and never stops the sweep
/// from reaching the other assets; the errors are returned together. The
/// app runs this at launch and after every processed meeting.
public struct RetentionSweep: Sendable {
  public let store: MeetingStore

  /// Every file the sweep could not remove, with the reason.
  public struct Incomplete: Error, CustomStringConvertible {
    public var failures: [(url: URL, error: any Error)]
    public var description: String {
      "retention sweep could not remove "
        + failures.map { "\($0.url.path) (\($0.error))" }.joined(separator: ", ")
    }
  }

  public init(store: MeetingStore) {
    self.store = store
  }

  /// The files actually removed, in asset then file order. Throws
  /// `Incomplete` after visiting every asset when any file resisted.
  @discardableResult
  public func run(now: Date) async throws -> [URL] {
    var removed: [URL] = []
    var failures: [(url: URL, error: any Error)] = []
    for asset in try await store.expiredAssets(now: now) {
      var clean = true
      for url in asset.expirableFiles where FileManager.default.fileExists(atPath: url.path) {
        do {
          try FileManager.default.removeItem(at: url)
          removed.append(url)
        } catch {
          clean = false
          failures.append((url, error))
        }
      }
      guard clean else { continue }
      var swept = asset
      swept.expiresAt = nil
      try await store.save(swept)
    }
    if !failures.isEmpty { throw Incomplete(failures: failures) }
    return removed
  }
}
