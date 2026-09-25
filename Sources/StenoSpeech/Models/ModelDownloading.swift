import Foundation
import StenoCore

/// The seam between `ModelStore` and the two frameworks' download code, so
/// the store is unit-tested without network. `directory` is the asset's own
/// directory (`ModelStore.directory(for:)`); the downloader fills it so that
/// every `ModelAsset.requiredFiles` entry exists when it returns.
public protocol ModelDownloading: Sendable {
  func download(
    _ asset: ModelAsset, into directory: URL,
    progress: @escaping @Sendable (Double, String) -> Void
  ) async throws
}

public enum ModelDownloadError: Error, Sendable, Equatable, CustomStringConvertible {
  /// This build has neither FluidAudio nor WhisperKit (Linux); nothing can be
  /// downloaded.
  case unsupportedPlatform(ModelAsset)
  /// The downloader returned but `requiredFiles` are still missing.
  case incomplete(ModelAsset, missing: [String])

  public var description: String {
    switch self {
    case .unsupportedPlatform(let asset):
      "\(asset.rawValue) cannot be downloaded on this platform"
    case .incomplete(let asset, let missing):
      "\(asset.rawValue) download finished but \(missing.joined(separator: ", ")) is missing"
    }
  }
}

#if canImport(FluidAudio) && canImport(WhisperKit)
  /// Downloads through FluidAudio's `ModelHub` and WhisperKit's Hub client.
  /// FluidAudio downloads are serialised because the German fine-tune is
  /// fetched through the process-wide `ModelRegistry.repoOverrides`
  /// redirection, which must not be visible to a concurrent v3 download.
  public struct LiveModelDownloader: ModelDownloading {
    public init() {}

    public func download(
      _ asset: ModelAsset, into directory: URL,
      progress: @escaping @Sendable (Double, String) -> Void
    ) async throws {
      switch asset {
      case .parakeetV3, .parakeetUltra, .parakeetDE, .offlineDiarizer:
        try await FluidAudioDownloads.shared.download(asset, into: directory, progress: progress)
      case .whisperLargeV3Turbo:
        try await WhisperKitDownloads.download(asset, into: directory, progress: progress)
      }
    }
  }
#else
  /// Linux builds have no model frameworks; every download fails with
  /// `ModelDownloadError.unsupportedPlatform`.
  public struct LiveModelDownloader: ModelDownloading {
    public init() {}

    public func download(
      _ asset: ModelAsset, into directory: URL,
      progress: @escaping @Sendable (Double, String) -> Void
    ) async throws {
      throw ModelDownloadError.unsupportedPlatform(asset)
    }
  }
#endif
