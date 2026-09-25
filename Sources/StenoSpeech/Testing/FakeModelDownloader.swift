import Foundation
import StenoCore

/// A `ModelDownloading` that writes the asset's `requiredPaths` as markers
/// after reporting scripted progress: plain files empty, compiled bundles as
/// a directory holding an empty `coremldata.bin`, which is the shape
/// `ModelStore.isInstalled` accepts. `hold` lets a test keep the download
/// open until it releases it, so two concurrent `ensure` calls can be shown
/// to share one download. Records every asset it was asked for.
public struct FakeModelDownloader: ModelDownloading, Sendable {
  public var steps: [(fraction: Double, phase: String)]
  /// Thrown by the first download only, so a test can show the retry.
  public var failure: (any Error & Sendable)?
  /// Awaited between the progress steps and the marker files.
  public var hold: (@Sendable () async -> Void)?
  /// When false the downloader returns without writing anything, which
  /// `ModelStore` reports as `ModelDownloadError.incomplete`.
  public var writesMarkers: Bool
  public let downloads = CallLog<ModelAsset>()

  public init(
    steps: [(fraction: Double, phase: String)] = [(0.5, "downloading"), (1, "done")],
    failure: (any Error & Sendable)? = nil,
    hold: (@Sendable () async -> Void)? = nil,
    writesMarkers: Bool = true
  ) {
    self.steps = steps
    self.failure = failure
    self.hold = hold
    self.writesMarkers = writesMarkers
  }

  public func download(
    _ asset: ModelAsset, under root: URL,
    progress: @escaping @Sendable (Double, String) -> Void
  ) async throws {
    await downloads.record(asset)
    for step in steps { progress(step.fraction, step.phase) }
    await hold?()
    if let failure, await downloads.count == 1 { throw failure }
    guard writesMarkers else { return }
    for path in asset.requiredPaths {
      try Self.writeMarker(at: root.appendingPathComponent(path))
    }
  }

  /// Replaces whatever is at `url` with a complete marker, as the real
  /// downloaders replace a half-written file.
  static func writeMarker(at url: URL) throws {
    let fileManager = FileManager.default
    if fileManager.fileExists(atPath: url.path) { try fileManager.removeItem(at: url) }
    try fileManager.createDirectory(
      at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    if url.pathExtension == "mlmodelc" {
      try fileManager.createDirectory(at: url, withIntermediateDirectories: true)
      try Data().write(to: url.appendingPathComponent("coremldata.bin"))
    } else {
      try Data().write(to: url)
    }
  }
}
