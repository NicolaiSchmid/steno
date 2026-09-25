import Foundation
import StenoCore

/// A `ModelDownloading` that writes the asset's `requiredFiles` as empty
/// markers after reporting scripted progress. `hold` lets a test keep the
/// download open until it releases it, so two concurrent `ensure` calls can
/// be shown to share one download. Records every asset it was asked for.
public struct FakeModelDownloader: ModelDownloading, Sendable {
  public var steps: [(fraction: Double, phase: String)]
  /// Thrown by the first `failureCount` downloads.
  public var failure: (any Error & Sendable)?
  public var failureCount: Int
  /// Awaited between the progress steps and the marker files.
  public var hold: (@Sendable () async -> Void)?
  /// When false the downloader returns without writing anything, which
  /// `ModelStore` reports as `ModelDownloadError.incomplete`.
  public var writesMarkers: Bool
  public let downloads = CallLog<ModelAsset>()

  public init(
    steps: [(fraction: Double, phase: String)] = [(0.5, "downloading"), (1, "done")],
    failure: (any Error & Sendable)? = nil,
    failureCount: Int = .max,
    hold: (@Sendable () async -> Void)? = nil,
    writesMarkers: Bool = true
  ) {
    self.steps = steps
    self.failure = failure
    self.failureCount = failureCount
    self.hold = hold
    self.writesMarkers = writesMarkers
  }

  public func download(
    _ asset: ModelAsset, into directory: URL,
    progress: @escaping @Sendable (Double, String) -> Void
  ) async throws {
    await downloads.record(asset)
    for step in steps { progress(step.fraction, step.phase) }
    await hold?()
    if let failure, await downloads.count <= failureCount { throw failure }
    guard writesMarkers else { return }
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    for name in asset.requiredFiles {
      try Data().write(to: directory.appendingPathComponent(name))
    }
  }
}
