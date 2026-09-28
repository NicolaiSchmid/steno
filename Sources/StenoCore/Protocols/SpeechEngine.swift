import Foundation

/// Speech-to-text behind one protocol; v1 ships `parakeet-v3` and
/// `whisperkit-large-v3-turbo` in StenoSpeech. Actors declare `id` and
/// `supportedLanguages` as `nonisolated let`.
public protocol SpeechEngine: Sendable {
  var id: String { get }
  var supportedLanguages: Set<Locale.Language> { get }
  /// Downloads or loads models. Called before the first `transcribe` and
  /// again by later runs and warm-ups: idempotent, a call on a loaded engine
  /// returns at once. Concurrent calls are serialised by the pipeline.
  func prepare() async throws
  /// `hint` is the other lane's dominant language, or nil for the first lane.
  func transcribe(_ audio: AudioBuffer16k, hint: Locale.Language?) async throws -> [RawSegment]
}
