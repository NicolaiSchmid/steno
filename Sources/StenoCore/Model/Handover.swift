import Foundation

/// A phone paired with this Mac. The bearer token itself is never stored;
/// `MeetingStore` keeps its SHA-256.
public struct PairedDevice: Codable, Sendable, Equatable, Hashable, Identifiable {
  public var id: UUID
  public var name: String
  public var pairedAt: Date
  public var lastSeenAt: Date?

  public init(id: UUID, name: String, pairedAt: Date, lastSeenAt: Date? = nil) {
    self.id = id
    self.name = name
    self.pairedAt = pairedAt
    self.lastSeenAt = lastSeenAt
  }
}

/// Progress of one phone recording being handed over. Idempotency key of
/// `RecordingIntake.admit`: a recording that already reached `.complete`
/// returns the same meeting id.
public struct HandoverReceipt: Codable, Sendable, Equatable, Hashable, Identifiable {
  public enum State: Codable, Sendable, Equatable, Hashable {
    case receiving
    case verifying
    case complete(meetingID: UUID)
    case failed(String)

    public var meetingID: UUID? {
      if case .complete(let meetingID) = self { return meetingID }
      return nil
    }

    private struct Complete: Codable {
      var meetingID: UUID
    }

    public init(from decoder: any Decoder) throws {
      let (name, payload) = try CaseCoding.decode(from: decoder)
      switch name {
      case "receiving": self = .receiving
      case "verifying": self = .verifying
      case "complete":
        let complete = try CaseCoding.decodePayload(Complete.self, from: payload, case: name)
        self = .complete(meetingID: complete.meetingID)
      case "failed":
        self = .failed(try CaseCoding.decodePayload(String.self, from: payload, case: name))
      default: throw CaseCoding.unknownCase(name, in: decoder)
      }
    }

    public func encode(to encoder: any Encoder) throws {
      switch self {
      case .receiving: try CaseCoding.encode("receiving", to: encoder)
      case .verifying: try CaseCoding.encode("verifying", to: encoder)
      case .complete(let meetingID):
        try CaseCoding.encode("complete", payload: Complete(meetingID: meetingID), to: encoder)
      case .failed(let message): try CaseCoding.encode("failed", payload: message, to: encoder)
      }
    }
  }

  public var recordingID: UUID
  public var deviceID: UUID
  public var state: State
  public var byteCount: Int64
  public var sha256: Data
  public var chunkSize: Int
  /// Indexes of the chunks received so far, ascending.
  public var receivedChunks: [Int]
  public var createdAt: Date
  public var updatedAt: Date

  public init(
    recordingID: UUID,
    deviceID: UUID,
    state: State,
    byteCount: Int64,
    sha256: Data,
    chunkSize: Int,
    receivedChunks: [Int] = [],
    createdAt: Date,
    updatedAt: Date
  ) {
    self.recordingID = recordingID
    self.deviceID = deviceID
    self.state = state
    self.byteCount = byteCount
    self.sha256 = sha256
    self.chunkSize = chunkSize
    self.receivedChunks = receivedChunks
    self.createdAt = createdAt
    self.updatedAt = updatedAt
  }

  public var id: UUID { recordingID }
}

/// What the phone declares before uploading; the handover wire mirrors these
/// names in `wire.ts`. `sha256` is the whole-file digest.
public struct RecordingMetadata: Codable, Sendable, Equatable, Hashable {
  public var recordingID: UUID
  public var startedAt: Date
  public var durationSeconds: Double
  public var byteCount: Int64
  public var sha256: Data
  public var chunkSize: Int
  public var format: AudioFormat
  public var deviceName: String

  public init(
    recordingID: UUID,
    startedAt: Date,
    durationSeconds: Double,
    byteCount: Int64,
    sha256: Data,
    chunkSize: Int,
    format: AudioFormat,
    deviceName: String
  ) {
    self.recordingID = recordingID
    self.startedAt = startedAt
    self.durationSeconds = durationSeconds
    self.byteCount = byteCount
    self.sha256 = sha256
    self.chunkSize = chunkSize
    self.format = format
    self.deviceName = deviceName
  }
}
