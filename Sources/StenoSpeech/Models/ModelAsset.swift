import Foundation

/// One downloadable model bundle. The table below is the single place that
/// knows where an asset comes from, where it lives under the models root and
/// which files must be present for it to count as installed.
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

  /// Where the asset's files live, relative to the models root. FluidAudio
  /// derives the directory name from the repository (`<repo>` minus
  /// `-coreml`), so those names are fixed by the framework; the German
  /// fine-tune reuses the v3 name and gets its own parent so it can never
  /// shadow the official model. WhisperKit lays its Hugging Face cache out
  /// under `downloadBase/models/<repo>/<variant>`, so the last component is
  /// the variant name the engine and the downloader pass to the framework.
  public var relativePath: String {
    switch self {
    case .parakeetV3: "fluidaudio/parakeet-tdt-0.6b-v3"
    case .parakeetUltra: "fluidaudio/parakeet-ultra"
    case .parakeetDE: "fluidaudio-de/parakeet-tdt-0.6b-v3"
    case .whisperLargeV3Turbo:
      "whisperkit/models/argmaxinc/whisperkit-coreml/openai_whisper-large-v3-v20240930_turbo"
    case .offlineDiarizer: "fluidaudio/speaker-diarization"
    }
  }

  /// Files (or compiled model bundles) inside `relativePath` whose presence
  /// means the asset is installed. Checked by `ModelStore.isInstalled`;
  /// written as markers by `FakeModelDownloader`.
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
