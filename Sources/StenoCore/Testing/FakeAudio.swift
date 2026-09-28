import Foundation

/// An `AudioDecoder` over any other that records the lane of every `decode`,
/// so a test can count how often each lane is decoded in a run. `mixdown`
/// passes through unrecorded.
public struct RecordingAudioDecoder: AudioDecoder, Sendable {
  public let inner: any AudioDecoder
  public let decodes = CallLog<AudioLane>()

  public init(wrapping inner: any AudioDecoder = WAVAudioDecoder()) {
    self.inner = inner
  }

  public func decode(_ asset: AudioAsset, lane: AudioLane) async throws -> AudioBuffer16k {
    await decodes.record(lane)
    return try await inner.decode(asset, lane: lane)
  }

  public var mixdownFormat: AudioFormat { inner.mixdownFormat }

  public func mixdown(_ asset: AudioAsset, to url: URL) async throws {
    try await inner.mixdown(asset, to: url)
  }
}
