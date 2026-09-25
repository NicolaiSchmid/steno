import Foundation

public enum AudioFormat: String, Codable, Sendable, Equatable, Hashable, CaseIterable {
  /// The Mac master recording: CAF, 48 kHz, Float32, one channel per lane.
  case caf48kFloat32
  /// Phone recordings and the optional export mixdown.
  case m4aAAC
  /// Fixtures, sample clips and `steno process` input: 16 kHz mono Int16.
  case wav16kInt16

  /// The file extension `RecordingLayout` gives a file in this format.
  public var fileExtension: String {
    switch self {
    case .caf48kFloat32: "caf"
    case .m4aAAC: "m4a"
    case .wav16kInt16: "wav"
    }
  }
}

/// How long the audio files of a meeting stay on disk.
public enum AudioRetention: Codable, Sendable, Equatable, Hashable {
  /// The scope's `0`: the retention stage sets `expiresAt = now`.
  case deleteAfterProcessing
  case keepDays(Int)
  /// The scope's empty value and the per-meeting keep toggle.
  case keepForever

  /// When the files expire counted from `now`; nil for `.keepForever`.
  public func expiry(from now: Date) -> Date? {
    switch self {
    case .deleteAfterProcessing: now
    case .keepDays(let days): now.addingTimeInterval(TimeInterval(days) * 86_400)
    case .keepForever: nil
    }
  }

  /// The case names, shared by `meeting.json` and the `audioAsset.retention`
  /// column.
  public enum Kind: String, CaseIterable, Codable, Sendable {
    case deleteAfterProcessing, keepDays, keepForever
  }

  public var kind: Kind {
    switch self {
    case .deleteAfterProcessing: .deleteAfterProcessing
    case .keepDays: .keepDays
    case .keepForever: .keepForever
    }
  }

  public init(from decoder: any Decoder) throws {
    let (kind, payload) = try CaseCoding.decode(Kind.self, from: decoder)
    switch kind {
    case .deleteAfterProcessing: self = .deleteAfterProcessing
    case .keepForever: self = .keepForever
    case .keepDays:
      self = .keepDays(try CaseCoding.decodePayload(Int.self, from: payload, case: kind))
    }
  }

  public func encode(to encoder: any Encoder) throws {
    switch self {
    case .keepDays(let days): try CaseCoding.encode(kind, payload: days, to: encoder)
    default: try CaseCoding.encode(kind, to: encoder)
    }
  }
}

/// The recording files of one meeting. `url` is the master, `sidecars16k` the
/// per-lane 16 kHz decodes the capture writer produces, `mixdownURL` the AAC
/// mono export written by the persist stage.
public struct AudioAsset: Codable, Sendable, Equatable, Hashable, Identifiable {
  public var id: UUID
  public var meetingID: UUID
  public var url: URL
  public var format: AudioFormat
  public var lanes: [AudioLane]
  public var sidecars16k: [AudioLane: URL]
  public var mixdownURL: URL?
  public var retention: AudioRetention
  /// Stored so the sweep is one query and the keep toggle clears it.
  public var expiresAt: Date?

  public init(
    id: UUID,
    meetingID: UUID,
    url: URL,
    format: AudioFormat,
    lanes: [AudioLane],
    sidecars16k: [AudioLane: URL] = [:],
    mixdownURL: URL? = nil,
    retention: AudioRetention,
    expiresAt: Date? = nil
  ) {
    self.id = id
    self.meetingID = meetingID
    self.url = url
    self.format = format
    self.lanes = lanes
    self.sidecars16k = sidecars16k
    self.mixdownURL = mixdownURL
    self.retention = retention
    self.expiresAt = expiresAt
  }

  /// Master, sidecars and mixdown: what `RetentionSweep` removes together.
  /// Sample clips are `Speaker.sampleClipURL` and never part of this.
  public var expirableFiles: [URL] {
    var files = [url]
    files.append(contentsOf: sidecars16k.sorted { $0.key.rawValue < $1.key.rawValue }.map(\.value))
    if let mixdownURL { files.append(mixdownURL) }
    return files
  }
}
