import Foundation

/// One downloadable model bundle. The table below is the single place that
/// knows where an asset comes from, how its files are laid out under the
/// models root and which of them must be complete for it to count as
/// installed.
public enum ModelAsset: String, Sendable, CaseIterable, Codable, Hashable {
  case parakeetV3
  case parakeetUltra
  case parakeetDE
  case whisperLargeV3Turbo
  case offlineDiarizer

  /// The Hugging Face repository the files come from.
  public var sourceRepo: String {
    switch self {
    case .parakeetV3: "FluidInference/parakeet-tdt-0.6b-v3-coreml"
    case .parakeetUltra: "FluidInference/parakeet-ultra-coreml"
    case .parakeetDE: "ValentinWeyer/parakeet-primeline-de-coreml"
    case .whisperLargeV3Turbo: "argmaxinc/whisperkit-coreml"
    case .offlineDiarizer: "FluidInference/speaker-diarization-coreml"
    }
  }

  /// Size on disk after the download, measured from the repository trees on
  /// 2026-09-25. Shown before a download, not asserted.
  public var approximateBytes: Int64 {
    switch self {
    case .parakeetV3: 485_000_000
    case .parakeetUltra: 632_000_000
    case .parakeetDE: 1_220_000_000
    case .whisperLargeV3Turbo: 1_640_000_000
    case .offlineDiarizer: 22_000_000
    }
  }

  public var licence: String {
    switch self {
    case .parakeetV3, .parakeetUltra, .parakeetDE: "CC-BY-4.0"
    case .whisperLargeV3Turbo: "MIT (WhisperKit), OpenAI weights"
    case .offlineDiarizer: "Apache-2.0 (pyannote and WeSpeaker upstream)"
    }
  }

  public var displayName: String {
    switch self {
    case .parakeetV3: "Parakeet TDT 0.6B v3 (int8)"
    case .parakeetUltra: "Parakeet Ultra (int8)"
    case .parakeetDE: "Parakeet German fine-tune (fp16)"
    case .whisperLargeV3Turbo: "Whisper large-v3 turbo"
    case .offlineDiarizer: "Speaker diarization (pyannote community-1)"
    }
  }

  // MARK: Layout under the models root

  /// The folder handed to the framework, relative to the models root: the
  /// parent FluidAudio's `ModelHub` writes the repository folder into, or
  /// WhisperKit's `downloadBase`. The German fine-tune reuses the v3 folder
  /// name, so it gets its own parent and can never shadow the official model.
  public var frameworkRoot: String {
    switch self {
    case .parakeetV3, .parakeetUltra, .offlineDiarizer: "fluidaudio"
    case .parakeetDE: "fluidaudio-de"
    case .whisperLargeV3Turbo: "whisperkit"
    }
  }

  /// The framework's own name for the model inside its root: FluidAudio's
  /// `Repo.folderName` (the repository minus `-coreml`; the fine-tune keeps
  /// the v3 name because it is fetched through the v3 `Repo` case), or the
  /// WhisperKit variant the engine and the downloader pass to the framework.
  public var modelFolder: String {
    switch self {
    case .parakeetV3, .parakeetDE: "parakeet-tdt-0.6b-v3"
    case .parakeetUltra: "parakeet-ultra"
    case .offlineDiarizer: "speaker-diarization"
    case .whisperLargeV3Turbo: "openai_whisper-large-v3-v20240930_turbo"
    }
  }

  /// Where the asset's files live, relative to the models root: the
  /// framework root plus the suffix the framework fixes. FluidAudio puts the
  /// repository folder straight under its parent; WhisperKit lays its Hub
  /// cache out as `models/<repo>/<variant>`.
  public var relativePath: String {
    switch self {
    case .parakeetV3, .parakeetUltra, .parakeetDE, .offlineDiarizer:
      "\(frameworkRoot)/\(modelFolder)"
    case .whisperLargeV3Turbo:
      "\(frameworkRoot)/models/\(sourceRepo)/\(modelFolder)"
    }
  }

  /// The tokenizer repository WhisperKit resolves for large-v3 weights
  /// (`ModelUtilities.tokenizerNameForVariant(.largev3)`); it is cached under
  /// `downloadBase/models/<repo>` like any other Hub repository.
  static let whisperTokenizerRepo = "openai/whisper-large-v3"

  /// Files (or compiled model bundles) inside `relativePath` that the
  /// framework writes for the model itself.
  public var requiredFiles: [String] {
    switch self {
    case .parakeetV3, .parakeetUltra, .parakeetDE:
      [
        "Preprocessor.mlmodelc", "Encoder.mlmodelc", "Decoder.mlmodelc",
        "JointDecisionv3.mlmodelc", "parakeet_vocab.json",
      ]
    case .whisperLargeV3Turbo:
      [
        "config.json", "MelSpectrogram.mlmodelc", "AudioEncoder.mlmodelc",
        "TextDecoder.mlmodelc",
      ]
    case .offlineDiarizer:
      [
        "Segmentation.mlmodelc", "FBank.mlmodelc", "Embedding.mlmodelc", "PldaRho.mlmodelc",
        "plda-parameters.json",
      ]
    }
  }

  /// Everything that must be complete under the models root for the asset
  /// to count as installed: `requiredFiles` inside `relativePath`, plus what
  /// the framework fetches beside the weights. WhisperKit loads its
  /// tokenizer from `downloadBase/models/openai/whisper-large-v3` and goes
  /// online when it is missing, so the file belongs to the asset. Checked by
  /// `ModelStore.isInstalled`; written as markers by `FakeModelDownloader`.
  var requiredPaths: [String] {
    var paths = requiredFiles.map { "\(relativePath)/\($0)" }
    if self == .whisperLargeV3Turbo {
      paths.append("\(frameworkRoot)/models/\(Self.whisperTokenizerRepo)/tokenizer.json")
    }
    return paths
  }

  /// `relativePath` under `root`.
  func directory(under root: URL) -> URL {
    root.appendingPathComponent(relativePath, isDirectory: true)
  }

  /// `frameworkRoot` under `root`.
  func frameworkRoot(under root: URL) -> URL {
    root.appendingPathComponent(frameworkRoot, isDirectory: true)
  }
}

/// One progress event from `ModelStore.ensure`. `phase` is the downloader's
/// own wording ("downloading Encoder.mlmodelc", "installed").
public struct ModelDownloadProgress: Sendable, Equatable {
  public var asset: ModelAsset
  public var fractionCompleted: Double
  public var phase: String

  public init(asset: ModelAsset, fractionCompleted: Double, phase: String) {
    self.asset = asset
    self.fractionCompleted = fractionCompleted
    self.phase = phase
  }
}
