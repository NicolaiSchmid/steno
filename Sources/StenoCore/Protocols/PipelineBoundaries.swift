import Foundation

/// Decodes one lane of an asset to 16 kHz mono. StenoAudio's AVFoundation
/// codec reads CAF and m4a and resamples; core's `WAVAudioDecoder` reads
/// 16 kHz mono WAV only.
public protocol AudioDecoder: Sendable {
  /// The lane's sidecar when present, else decoded from the master.
  func decode(_ asset: AudioAsset, lane: AudioLane) async throws -> AudioBuffer16k
  /// The container `mixdown` writes: `.m4aAAC` for StenoAudio, `.wav16kInt16`
  /// for core's WAV copy. The persist stage names the file from it
  /// (`RecordingLayout.mixdown(_:)`), so `mixdownURL` never carries the
  /// wrong extension.
  var mixdownFormat: AudioFormat { get }
  /// Mono mixdown in `mixdownFormat` for the optional audio export.
  func mixdown(_ asset: AudioAsset, to url: URL) async throws
}

/// The cleanup pass (StenoLLM): fixes Denglish, casing and names, chunked,
/// segment count and order preserved.
public protocol TranscriptCleaner: Sendable {
  func clean(_ input: CleanupInput) async throws -> CleanupOutput
}

/// The summary pass (StenoLLM): title, structured summary, decisions, tasks
/// and speaker name suggestions for the selected template.
public protocol MeetingSummarizer: Sendable {
  func summarize(_ input: SummaryInput) async throws -> SummaryOutput
}

/// Delivers one meeting to every configured destination (StenoAdapters),
/// passing each destination's stored receipt as `previous`. Never throws: a
/// failed destination is a `Delivery` with `.failed`.
public protocol DeliveryDispatcher: Sendable {
  func deliverAll(meetingID: UUID) async -> [Delivery]
}

/// Admits a fully received phone recording; returns the new `Meeting.id`.
/// Core's `RecordingIntake` conforms.
public protocol HandoverIntake: Sendable {
  func admit(file: URL, metadata: RecordingMetadata, device: PairedDevice) async throws -> UUID
}
