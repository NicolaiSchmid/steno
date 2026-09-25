#if canImport(WhisperKit)
  import Foundation
  import WhisperKit

  /// WhisperKit's Hub download. `downloadBase` is the `whisperkit/` folder
  /// under the models root; the framework appends
  /// `models/<repo>/<variant>`, which is `ModelAsset.relativePath`. The
  /// tokenizer is fetched right after the weights so an installed asset works
  /// offline from then on.
  enum WhisperKitDownloads {
    static func download(
      _ asset: ModelAsset, into directory: URL,
      progress: @escaping @Sendable (Double, String) -> Void
    ) async throws {
      guard let variant = asset.whisperVariant else {
        throw ModelDownloadError.unsupportedPlatform(asset)
      }
      let downloadBase = WhisperKitEngine.downloadBase(for: directory)
      let repo = asset.sourceRepo
      let folder = try await WhisperKit.download(
        variant: variant, downloadBase: downloadBase, from: repo,
        progressCallback: { update in
          progress(update.fractionCompleted * 0.95, "downloading \(variant)")
        })
      if folder.standardizedFileURL != directory.standardizedFileURL {
        // Should not happen with the fixed repository; move the files where
        // `ModelStore` expects them rather than leaving them stranded.
        try? FileManager.default.removeItem(at: directory)
        try FileManager.default.createDirectory(
          at: directory.deletingLastPathComponent(), withIntermediateDirectories: true)
        try FileManager.default.moveItem(at: folder, to: directory)
      }
      progress(0.95, "downloading tokenizer")
      _ = try await ModelUtilities.loadTokenizer(for: .largev3, tokenizerFolder: downloadBase)
      progress(1, "installed")
    }
  }
#endif
