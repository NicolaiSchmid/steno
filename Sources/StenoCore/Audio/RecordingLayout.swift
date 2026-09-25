import Foundation

/// The one spelling of a meeting's folder and the files inside it:
///
/// ```
/// <audioFolder>/<meetingID>/
///   recording.<ext>          master (`master(_:)`)
///   mic.wav, system.wav      16 kHz sidecars (`sidecar(_:)`)
///   audio.<ext>              mixdown (`mixdown(_:)`)
///   speakers/<speakerID>.wav sample clips (`sampleClip(speakerID:)`)
/// ```
///
/// Whoever creates an `AudioAsset` (the capture writer, `RecordingIntake`,
/// `steno process`) picks the folder with `init(audioFolder:meetingID:)`. The
/// pipeline never reads `Settings.audioFolder`: it derives the layout from the
/// asset with `init(directory: asset.url.deletingLastPathComponent())`.
public struct RecordingLayout: Sendable, Equatable, Hashable {
  public var directory: URL

  public init(directory: URL) {
    self.directory = directory
  }

  public init(audioFolder: URL, meetingID: UUID) {
    self.init(
      directory: audioFolder.appendingPathComponent(meetingID.uuidString, isDirectory: true))
  }

  /// The folder holding `asset.url`.
  public init(asset: AudioAsset) {
    self.init(directory: asset.url.deletingLastPathComponent())
  }

  public func master(_ format: AudioFormat) -> URL {
    directory.appendingPathComponent("recording." + format.fileExtension)
  }

  public func sidecar(_ lane: AudioLane) -> URL {
    directory.appendingPathComponent(lane.rawValue + ".wav")
  }

  public func mixdown(_ format: AudioFormat) -> URL {
    directory.appendingPathComponent("audio." + format.fileExtension)
  }

  public var speakersDirectory: URL {
    directory.appendingPathComponent("speakers", isDirectory: true)
  }

  public func sampleClip(speakerID: UUID) -> URL {
    speakersDirectory.appendingPathComponent(speakerID.uuidString + ".wav")
  }

  /// Creates `directory` (and `speakers/` when asked) if needed.
  public func createDirectories(speakers: Bool = false) throws {
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    if speakers {
      try FileManager.default.createDirectory(
        at: speakersDirectory, withIntermediateDirectories: true)
    }
  }
}
