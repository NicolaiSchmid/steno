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

/// Runs jobs strictly one after another, in submission order, whatever
/// their outcome. An actor alone cannot promise this for work it awaits (it
/// is re-entrant across the suspension), and `LiveModelDownloader` needs it:
/// the German fine-tune is fetched through a process-wide repository
/// redirect that no other download may observe.
actor DownloadSerializer {
  private var last: Task<Void, Never>?

  func run<T: Sendable>(_ body: @escaping @Sendable () async throws -> T) async throws -> T {
    try await enqueue(body).value
  }

  /// Appends the job to the chain and returns it without waiting for it.
  /// Once this returns the job's place in the order is fixed, which is what
  /// lets a test submit two jobs in a known order without relying on the
  /// scheduler.
  func enqueue<T: Sendable>(_ body: @escaping @Sendable () async throws -> T) -> Task<T, any Error>
  {
    let previous = last
    let job = Task {
      await previous?.value
      return try await body()
    }
    last = Task { _ = try? await job.value }
    return job
  }
}

/// One entry of a process-wide string table set for the duration of a job
/// and put back afterwards, whether the job returns or throws. Only that one
/// key is touched: entries another party writes meanwhile survive. `read`
/// and `write` are the table's accessors (FluidAudio's
/// `ModelRegistry.repoOverrides` in production), so the scoping rule is
/// testable against a plain dictionary.
struct ScopedRedirect: Sendable {
  var read: @Sendable () -> [String: String]
  var write: @Sendable ([String: String]) -> Void

  func run<T: Sendable>(
    _ key: String, to value: String, body: @Sendable () async throws -> T
  ) async throws -> T {
    let previous = read()[key]
    set(key, to: value)
    defer { set(key, to: previous) }
    return try await body()
  }

  private func set(_ key: String, to value: String?) {
    var table = read()
    table[key] = value
    write(table)
  }
}

/// Downloads through FluidAudio's `ModelHub` and WhisperKit's Hub client,
/// one at a time for the whole process. FluidAudio downloads only its own
/// `Repo` cases, so the German fine-tune (v3 layout, another repository) is
/// fetched by redirecting the v3 repository through the process-wide
/// `ModelRegistry.repoOverrides` for the duration of that one download. The
/// chain is process-wide too: `ModelStore`s are cheap and the app, the CLI
/// and the pipeline each build their own, and two stores must not overlap a
/// v3 download with the redirect active. Where the frameworks are missing
/// (Linux) every download fails with `StenoSpeechError.unsupportedPlatform`.
public struct LiveModelDownloader: ModelDownloading {
  /// One chain per process, not per store.
  static let serializer = DownloadSerializer()

  public init() {}

  public func download(
    _ asset: ModelAsset, under root: URL,
    progress: @escaping @Sendable (Double, String) -> Void
  ) async throws {
    try await Self.serializer.run {
      try await Self.perform(asset, under: root, progress: progress)
    }
  }

  #if canImport(FluidAudio) && canImport(WhisperKit)
    /// FluidAudio's override table, read and written in place.
    static let repoOverrides = ScopedRedirect(
      read: { ModelRegistry.repoOverrides }, write: { ModelRegistry.repoOverrides = $0 })

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
        // `ModelHub.download` for a repository reports the download as the
        // first half of an operation whose second half (the CoreML compile)
        // it never runs here, so the fraction is doubled to reach 1.
        try await ModelHub.download(
          .diarizer, to: frameworkRoot, variant: "offline",
          progressHandler: {
            progress(min(1, $0.fractionCompleted * 2), String(describing: $0.phase))
          })
      case .whisperLargeV3Turbo:
        let variant = asset.modelFolder
        _ = try await WhisperKit.download(
          variant: variant, downloadBase: frameworkRoot, from: asset.sourceRepo,
          progressCallback: { progress($0.fractionCompleted * 0.95, "downloading \(variant)") })
        progress(0.95, "downloading tokenizer")
        _ = try await ModelUtilities.loadTokenizer(for: .largev3, tokenizerFolder: frameworkRoot)
      }
    }
  #else
    private static func perform(
      _ asset: ModelAsset, under root: URL,
      progress: @escaping @Sendable (Double, String) -> Void
    ) async throws {
      throw StenoSpeechError.unsupportedPlatform(asset)
    }
  #endif
}

#if canImport(FluidAudio) && canImport(WhisperKit)
  import FluidAudio
  import WhisperKit
#endif
