#if canImport(FluidAudio)
  import FluidAudio
  import Foundation

  /// FluidAudio's download entry points, one asset at a time. `ModelHub`
  /// writes `<parent>/<repo folder>`, so the asset directory's parent is what
  /// it receives; the folder name FluidAudio derives matches
  /// `ModelAsset.relativePath` by construction.
  actor FluidAudioDownloads {
    static let shared = FluidAudioDownloads()

    func download(
      _ asset: ModelAsset, into directory: URL,
      progress: @escaping @Sendable (Double, String) -> Void
    ) async throws {
      let handler: ProgressHandler = { update in
        progress(update.fractionCompleted, String(describing: update.phase))
      }
      switch asset {
      case .parakeetV3:
        _ = try await AsrModels.download(
          to: directory, version: .v3, encoderPrecision: .int8, progressHandler: handler)
      case .parakeetUltra:
        _ = try await AsrModels.download(
          to: directory, version: .ultra, encoderPrecision: .int8, progressHandler: handler)
      case .parakeetDE:
        // The German fine-tune has the v3 layout but lives in another
        // repository. FluidAudio only downloads its own `Repo` cases, so the
        // v3 repository is redirected for the duration of this one download.
        let key = Repo.parakeetV3.rawValue
        let previous = ModelRegistry.repoOverrides
        ModelRegistry.repoOverrides[key] = asset.sourceRepo
        defer { ModelRegistry.repoOverrides = previous }
        _ = try await AsrModels.download(
          to: directory, version: .v3, encoderPrecision: .int8, progressHandler: handler)
      case .offlineDiarizer:
        try await ModelHub.download(
          .diarizer, to: directory.deletingLastPathComponent(), variant: "offline",
          progressHandler: handler)
      case .whisperLargeV3Turbo:
        throw ModelDownloadError.unsupportedPlatform(asset)
      }
    }
  }
#endif
