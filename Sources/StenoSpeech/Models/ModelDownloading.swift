import Foundation
import StenoCore

/// The seam between `ModelStore` and the two frameworks' download code, so
/// the store is unit-tested without network. `root` is the models root
/// (`ModelStore.directory`); the downloader fills the asset's layout under it
/// so that every `ModelAsset.requiredPaths` entry is complete when it returns.
public protocol ModelDownloading: Sendable {
  func download(
    _ asset: ModelAsset, under root: URL,
    progress: @escaping @Sendable (Double, String) -> Void
  ) async throws
}

public enum ModelDownloadError: Error, Sendable, Equatable, CustomStringConvertible {
  /// This build has neither FluidAudio nor WhisperKit (Linux); nothing can be
  /// downloaded.
  case unsupportedPlatform(ModelAsset)
  /// The downloader returned but `requiredPaths` are still missing.
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

/// Runs jobs strictly one after another, in submission order, whatever
/// their outcome. An actor alone cannot promise this for work it awaits (it
/// is re-entrant across the suspension), and `LiveModelDownloader` needs it:
/// the German fine-tune is fetched through a process-wide repository
/// redirect that no other download may observe.
actor DownloadSerializer {
  private var last: Task<Void, Never>?

  func run<T: Sendable>(_ body: @escaping @Sendable () async throws -> T) async throws -> T {
    let previous = last
    let job = Task {
      await previous?.value
      return try await body()
    }
    last = Task { _ = try? await job.value }
    return try await job.value
  }
}

/// One entry of a process-wide string table set for the duration of a job
/// and restored afterwards, whether the job returns or throws. `read` and
/// `write` are the table's accessors (FluidAudio's `ModelRegistry.repoOverrides`
/// in production), so the scoping rule is testable against a plain dictionary.
struct ScopedRedirect: Sendable {
  var read: @Sendable () -> [String: String]
  var write: @Sendable ([String: String]) -> Void

  func run<T: Sendable>(
    _ key: String, to value: String, body: @Sendable () async throws -> T
  ) async throws -> T {
    let previous = read()
    var redirected = previous
    redirected[key] = value
    write(redirected)
    defer { write(previous) }
    return try await body()
  }
}

#if canImport(FluidAudio) && canImport(WhisperKit)
  import FluidAudio
  import WhisperKit

  /// Downloads through FluidAudio's `ModelHub` and WhisperKit's Hub client,
  /// one at a time. FluidAudio downloads only its own `Repo` cases, so the
  /// German fine-tune (v3 layout, another repository) is fetched by
  /// redirecting the v3 repository through the process-wide
  /// `ModelRegistry.repoOverrides` for the duration of that one download.
  /// `DownloadSerializer` runs the downloads strictly in sequence, which is
  /// what keeps a concurrent v3 download from seeing the redirect.
  public struct LiveModelDownloader: ModelDownloading {
    private let serializer = DownloadSerializer()

    /// FluidAudio's override table, read and written in place.
    static let repoOverrides = ScopedRedirect(
      read: { ModelRegistry.repoOverrides }, write: { ModelRegistry.repoOverrides = $0 })

    public init() {}

    public func download(
      _ asset: ModelAsset, under root: URL,
      progress: @escaping @Sendable (Double, String) -> Void
    ) async throws {
      try await serializer.run {
        try await Self.perform(asset, under: root, progress: progress)
      }
    }

    /// `AsrModels.download(to:)` takes the model directory itself;
    /// `ModelHub.download(_:to:)` and WhisperKit take the framework root and
    /// append their own layout (`ModelAsset.relativePath`). The tokenizer is
    /// fetched right after the Whisper weights so an installed asset works
    /// offline; `.largev3` is the variant WhisperKit maps the turbo weights
    /// to, and it resolves to `ModelAsset.whisperTokenizerRepo`.
    private static func perform(
      _ asset: ModelAsset, under root: URL,
      progress: @escaping @Sendable (Double, String) -> Void
    ) async throws {
      let handler: ProgressHandler = {
        progress($0.fractionCompleted, String(describing: $0.phase))
      }
      let directory = asset.directory(under: root)
      let frameworkRoot = asset.frameworkRoot(under: root)
      switch asset {
      case .parakeetV3:
        try await AsrModels.download(
          to: directory, version: .v3, encoderPrecision: .int8, progressHandler: handler)
      case .parakeetUltra:
        try await AsrModels.download(
          to: directory, version: .ultra, encoderPrecision: .int8, progressHandler: handler)
      case .parakeetDE:
        try await repoOverrides.run(Repo.parakeetV3.rawValue, to: asset.sourceRepo) {
          try await AsrModels.download(
            to: directory, version: .v3, encoderPrecision: .int8, progressHandler: handler)
        }
      case .offlineDiarizer:
        try await ModelHub.download(
          .diarizer, to: frameworkRoot, variant: "offline", progressHandler: handler)
      case .whisperLargeV3Turbo:
        let variant = asset.modelFolder
        _ = try await WhisperKit.download(
          variant: variant, downloadBase: frameworkRoot, from: asset.sourceRepo,
          progressCallback: { progress($0.fractionCompleted * 0.95, "downloading \(variant)") })
        progress(0.95, "downloading tokenizer")
        _ = try await ModelUtilities.loadTokenizer(for: .largev3, tokenizerFolder: frameworkRoot)
        progress(1, "installed")
      }
    }
  }
#else
  /// Linux builds have no model frameworks; every download fails with
  /// `ModelDownloadError.unsupportedPlatform`.
  public struct LiveModelDownloader: ModelDownloading {
    public init() {}

    public func download(
      _ asset: ModelAsset, under root: URL,
      progress: @escaping @Sendable (Double, String) -> Void
    ) async throws {
      throw ModelDownloadError.unsupportedPlatform(asset)
    }
  }
#endif
