import Foundation
import StenoCore
import Synchronization

/// Installs, checks and removes model assets under one directory
/// (`~/Library/Application Support/Steno/Models` unless `Settings.modelsDirectory`
/// says otherwise). Downloads go through `ModelDownloading`; concurrent
/// `ensure` calls for one asset share a single download and each sees its
/// progress.
public actor ModelStore {
  public nonisolated let directory: URL
  private let downloader: any ModelDownloading
  private var inFlight: [ModelAsset: DownloadJob] = [:]

  /// Extensions of the staging files both frameworks write while a file of
  /// a bundle is still coming in (FluidAudio `*.partial`, WhisperKit
  /// `*.incomplete`).
  static let stagingExtensions: Set<String> = ["partial", "incomplete"]

  /// `<support directory>/Models`, following `HOME` like `StenoPaths`.
  public static func defaultDirectory() -> URL {
    StenoPaths.defaultSupportDirectory.appendingPathComponent("Models", isDirectory: true)
  }

  /// `directory` nil means `defaultDirectory()`; the app passes
  /// `Settings.modelsDirectory`.
  public init(directory: URL? = nil, downloader: any ModelDownloading = LiveModelDownloader()) {
    self.directory = (directory ?? Self.defaultDirectory()).standardizedFileURL
    self.downloader = downloader
  }

  /// The asset's own directory (`ModelAsset.relativePath` under the root).
  public nonisolated func directory(for asset: ModelAsset) -> URL {
    asset.directory(under: directory)
  }

  /// The folder handed to the framework for this asset: FluidAudio's
  /// `ModelHub` parent, WhisperKit's `downloadBase`.
  public nonisolated func frameworkRoot(for asset: ModelAsset) -> URL {
    asset.frameworkRoot(under: directory)
  }

  /// True when every `ModelAsset.requiredPaths` entry is complete: plain
  /// files exist, compiled bundles have their root `coremldata.bin` and no
  /// staging file inside. Both frameworks write each file of a bundle
  /// straight into place, so a process killed mid-bundle leaves a directory
  /// that exists and cannot load; FluidAudio's own cache check refuses the
  /// same shape. A bundle that is complete but corrupt still surfaces only
  /// when the framework loads it.
  public nonisolated func isInstalled(_ asset: ModelAsset) -> Bool {
    missingFiles(of: asset).isEmpty
  }

  public nonisolated func installedAssets() -> [ModelAsset] {
    ModelAsset.allCases.filter(isInstalled)
  }

  /// Bytes on disk of an installed asset, nil when absent.
  public nonisolated func installedSize(of asset: ModelAsset) -> Int64? {
    guard isInstalled(asset) else { return nil }
    let root = directory(for: asset)
    guard
      let files = FileManager.default.enumerator(
        at: root, includingPropertiesForKeys: [.fileSizeKey, .isRegularFileKey])
    else { return nil }
    var total: Int64 = 0
    for case let url as URL in files {
      let values = try? url.resourceValues(forKeys: [.fileSizeKey, .isRegularFileKey])
      if values?.isRegularFile == true { total += Int64(values?.fileSize ?? 0) }
    }
    return total
  }

  /// Installs the asset if needed. The stream ends when the asset is on disk
  /// and throws the downloader's error otherwise. An installed asset yields
  /// one `installed` event and finishes.
  public func ensure(_ asset: ModelAsset) -> AsyncThrowingStream<ModelDownloadProgress, any Error> {
    if isInstalled(asset) {
      return AsyncThrowingStream { continuation in
        continuation.yield(
          ModelDownloadProgress(asset: asset, fractionCompleted: 1, phase: "installed"))
        continuation.finish()
      }
    }
    if let job = inFlight[asset] {
      return job.subscribe()
    }
    let job = DownloadJob()
    inFlight[asset] = job
    let stream = job.subscribe()
    let root = directory
    let downloader = self.downloader
    Task {
      do {
        try FileManager.default.createDirectory(
          at: asset.directory(under: root), withIntermediateDirectories: true)
        try await downloader.download(asset, under: root) { fraction, phase in
          job.publish(
            ModelDownloadProgress(asset: asset, fractionCompleted: fraction, phase: phase))
        }
        let missing = self.missingFiles(of: asset)
        guard missing.isEmpty else {
          throw ModelDownloadError.incomplete(asset, missing: missing)
        }
        job.publish(ModelDownloadProgress(asset: asset, fractionCompleted: 1, phase: "installed"))
        await self.finish(asset, job: job, result: .success(()))
      } catch {
        await self.finish(asset, job: job, result: .failure(error))
      }
    }
    return stream
  }

  /// `ensure` drained: returns when installed, throws otherwise.
  public func ensureInstalled(_ asset: ModelAsset) async throws {
    for try await _ in ensure(asset) {}
  }

  /// Deletes the asset directory. Removing an asset mid-download is refused.
  public func remove(_ asset: ModelAsset) throws {
    guard inFlight[asset] == nil else { throw ModelStoreError.downloadInProgress(asset) }
    let target = directory(for: asset)
    if FileManager.default.fileExists(atPath: target.path) {
      try FileManager.default.removeItem(at: target)
    }
  }

  private func finish(_ asset: ModelAsset, job: DownloadJob, result: Result<Void, any Error>) {
    inFlight[asset] = nil
    job.finish(result)
  }

  /// The `requiredPaths` entries that are absent or not load-ready, relative
  /// to the models root.
  private nonisolated func missingFiles(of asset: ModelAsset) -> [String] {
    asset.requiredPaths.filter { !Self.isComplete(directory.appendingPathComponent($0)) }
  }

  /// A plain file is complete when it exists. A compiled bundle
  /// (`.mlmodelc`) is complete when it is a directory with its root
  /// `coremldata.bin` and nothing inside it still carries a staging
  /// extension.
  static func isComplete(_ url: URL) -> Bool {
    let fileManager = FileManager.default
    guard fileManager.fileExists(atPath: url.path) else { return false }
    guard url.pathExtension == "mlmodelc" else { return true }
    guard fileManager.fileExists(atPath: url.appendingPathComponent("coremldata.bin").path),
      let contents = fileManager.enumerator(at: url, includingPropertiesForKeys: nil)
    else { return false }
    for case let item as URL in contents where stagingExtensions.contains(item.pathExtension) {
      return false
    }
    return true
  }
}

public enum ModelStoreError: Error, Sendable, Equatable, CustomStringConvertible {
  case downloadInProgress(ModelAsset)

  public var description: String {
    switch self {
    case .downloadInProgress(let asset): "\(asset.rawValue) is being downloaded"
    }
  }
}

/// One download shared by every `ensure` stream opened while it runs. Late
/// subscribers first receive the latest progress so a progress bar never
/// starts from zero for a download that is half done.
final class DownloadJob: Sendable {
  typealias Continuation = AsyncThrowingStream<ModelDownloadProgress, any Error>.Continuation

  private struct State {
    var subscribers: [UUID: Continuation] = [:]
    var latest: ModelDownloadProgress?
    var outcome: Result<Void, any Error>?
  }

  private let state = Mutex(State())

  func subscribe() -> AsyncThrowingStream<ModelDownloadProgress, any Error> {
    AsyncThrowingStream { continuation in
      let id = UUID()
      let replay: (ModelDownloadProgress?, Result<Void, any Error>?) = state.withLock { state in
        if state.outcome == nil { state.subscribers[id] = continuation }
        return (state.latest, state.outcome)
      }
      if let latest = replay.0 { continuation.yield(latest) }
      switch replay.1 {
      case .success: continuation.finish()
      case .failure(let error): continuation.finish(throwing: error)
      case nil:
        continuation.onTermination = { _ in self.unsubscribe(id) }
      }
    }
  }

  private func unsubscribe(_ id: UUID) {
    state.withLock { $0.subscribers[id] = nil }
  }

  func publish(_ progress: ModelDownloadProgress) {
    let subscribers = state.withLock { state in
      state.latest = progress
      return Array(state.subscribers.values)
    }
    for continuation in subscribers { continuation.yield(progress) }
  }

  func finish(_ result: Result<Void, any Error>) {
    let subscribers = state.withLock { state in
      state.outcome = result
      let subscribers = Array(state.subscribers.values)
      state.subscribers.removeAll()
      return subscribers
    }
    for continuation in subscribers {
      switch result {
      case .success: continuation.finish()
      case .failure(let error): continuation.finish(throwing: error)
      }
    }
  }
}
