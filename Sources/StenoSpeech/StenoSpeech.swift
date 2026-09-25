import Foundation

// Speech and speakers: two on-device speech engines (Parakeet through
// FluidAudio, Whisper through WhisperKit) behind `SpeechEngine`, the
// FluidAudio offline diarizer behind `Diarizer`, cosine speaker memory over
// `MeetingStore`, the model store and the bake-off harness. Audio never
// leaves the device: the only network traffic in this module is the model
// download.
//
// Files that import FluidAudio or WhisperKit are compiled only where those
// frameworks exist (`canImport`), so the pure logic builds and tests on
// Linux as well.
//
// Public surface: `ModelStore`, `ModelAsset`, `ModelDownloadProgress`,
// `ModelDownloading` and `LiveModelDownloader`, `SpeechEngineID`,
// `makeSpeechEngine`, `makeDiarizer` and `FluidDiarizerConfig`,
// `CosineSpeakerMemory`, `BakeoffRunner` with `BakeoffReport` and
// `BakeoffRow`, `LanguageTagger.dominantLanguage(of:)`,
// `WordErrorRate.compute`, and this one error. Everything else is internal
// and reached by the tests through `@testable import`.

/// The one error type this module throws for its own conditions; the
/// frameworks' errors pass through untouched.
public enum StenoSpeechError: Error, Sendable, Equatable, CustomStringConvertible {
  /// `Settings.speechEngineID` names no known engine.
  case unknownEngine(String)
  /// This build has neither FluidAudio nor WhisperKit (Linux): the asset's
  /// engine cannot be built and the asset cannot be downloaded.
  case unsupportedPlatform(ModelAsset)
  /// The downloader returned but `ModelAsset.requiredPaths` are still
  /// missing.
  case incompleteDownload(ModelAsset, missing: [String])
  /// `ModelStore.remove` was called while the asset was being downloaded.
  case downloadInProgress(ModelAsset)

  public var description: String {
    switch self {
    case .unknownEngine(let id):
      "unknown speech engine \(id); known: \(SpeechEngineID.allCases.map(\.rawValue).joined(separator: ", "))"
    case .unsupportedPlatform(let asset):
      "\(asset.rawValue) is not available on this platform"
    case .incompleteDownload(let asset, let missing):
      "\(asset.rawValue) download finished but \(missing.joined(separator: ", ")) is missing"
    case .downloadInProgress(let asset):
      "\(asset.rawValue) is being downloaded"
    }
  }
}
