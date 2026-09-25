import Foundation

/// Speaker diarization of one lane. Clusters carry ranges, an embedding, the
/// cluster confidence and the diarizer-chosen sample clip range.
public protocol Diarizer: Sendable {
  func prepare() async throws
  func diarize(_ audio: AudioBuffer16k) async throws -> DiarizationResult
}
