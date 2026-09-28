import Foundation

/// Removes the audio of every asset whose `expiresAt` has passed: master,
/// sidecars and mixdown together with the sample clips of the meeting's
/// confirmed speakers, whose names no longer need a voice to check against.
/// Unconfirmed speakers keep their clips until confirmation or meeting
/// deletion, so an unnamed speaker still plays once the audio is gone. The
/// asset row keeps its URLs and loses `expiresAt` once every file is gone,
/// and the swept speakers lose `sampleClipURL`, so a sweep runs once per
/// expiry. A missing file is skipped, not an error. A file that cannot be
/// removed (permissions, read-only volume) leaves `expiresAt` set on its
/// asset and `sampleClipURL` on its speakers so the next sweep retries it,
/// and never stops the sweep from reaching the other assets; the errors are
/// returned together. The app runs this at launch and on
/// `MeetingEvent.retentionApplied`, which the retention stage does not post
/// while a delivery of the meeting is still pending or failed (the asset is
/// unstamped then). Assets of meetings still recording, queued or
/// processing are never due (`MeetingStore.expiredAssets`).
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

  /// The files actually removed, in asset order: each asset's own files,
  /// then its confirmed speakers' clips in cluster label order. Throws
  /// `Incomplete` after visiting every asset when any file resisted.
  @discardableResult
  public func run(now: Date) async throws -> [URL] {
    var removed: [URL] = []
    var failures: [(url: URL, error: any Error)] = []
    for asset in try await store.expiredAssets(now: now) {
      let confirmedWithClips = try await store.speakers(meetingID: asset.meetingID)
        .filter { $0.assignment.isConfirmed && $0.sampleClipURL != nil }
      let files = asset.expirableFiles + confirmedWithClips.compactMap(\.sampleClipURL)
      var clean = true
      for url in files where FileManager.default.fileExists(atPath: url.path) {
        do {
          try FileManager.default.removeItem(at: url)
          removed.append(url)
        } catch {
          clean = false
          failures.append((url, error))
        }
      }
      guard clean else { continue }
      try await store.clearSampleClips(
        meetingID: asset.meetingID, speakerIDs: confirmedWithClips.map(\.id))
      var swept = asset
      swept.expiresAt = nil
      try await store.save(swept)
    }
    if !failures.isEmpty { throw Incomplete(failures: failures) }
    return removed
  }

  /// Every asset whose master file is still on disk becomes `.keepForever`
  /// with `expiresAt` nil, in one write; Settings > Audio calls this when
  /// the rule changes to Forever, so the safe direction needs no per-meeting
  /// work. Assets whose master is gone are left as they are. Returns the
  /// number of assets kept.
  @discardableResult
  public func keepAll() async throws -> Int {
    let ids = try await store.assets()
      .filter { FileManager.default.fileExists(atPath: $0.url.path) }
      .map(\.id)
    try await store.keepForever(assetIDs: ids)
    return ids.count
  }
}
