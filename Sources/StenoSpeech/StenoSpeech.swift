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

public enum SpeechEngineError: Error, Sendable, Equatable, CustomStringConvertible {
  /// `Settings.speechEngineID` names no known engine.
  case unknownEngine(String)
  /// The engine exists in the table but not in this build (Linux).
  case unavailable(SpeechEngineID)
  /// `transcribe` was called before `prepare` could load the models.
  case notPrepared(String)

  public var description: String {
    switch self {
    case .unknownEngine(let id):
      "unknown speech engine \(id); known: \(SpeechEngineID.allCases.map(\.rawValue).joined(separator: ", "))"
    case .unavailable(let id): "speech engine \(id.rawValue) is not available on this platform"
    case .notPrepared(let id): "speech engine \(id) has not loaded its models"
    }
  }
}
