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

/// Where one phone recording's handover stands.
public enum HandoverState: Codable, Sendable, Equatable, Hashable {
  case receiving
  case verifying
  case complete(meetingID: UUID)
  case failed(String)

  /// The case names, shared by the wire and the `handoverReceipt.state`
  /// column.
  public enum Kind: String, CaseIterable, Codable, Sendable {
    case receiving, verifying, complete, failed
  }

  public var kind: Kind {
    switch self {
    case .receiving: .receiving
    case .verifying: .verifying
    case .complete: .complete
    case .failed: .failed
    }
  }

  public var meetingID: UUID? {
    if case .complete(let meetingID) = self { return meetingID }
    return nil
  }

  private struct Complete: Codable {
    var meetingID: UUID
  }

  public init(from decoder: any Decoder) throws {
    let (kind, payload) = try CaseCoding.decode(Kind.self, from: decoder)
    switch kind {
    case .receiving: self = .receiving
    case .verifying: self = .verifying
    case .complete:
      let complete = try CaseCoding.decodePayload(Complete.self, from: payload, case: kind)
      self = .complete(meetingID: complete.meetingID)
    case .failed:
      self = .failed(try CaseCoding.decodePayload(String.self, from: payload, case: kind))
    }
  }

  public func encode(to encoder: any Encoder) throws {
    switch self {
    case .complete(let meetingID):
      try CaseCoding.encode(kind, payload: Complete(meetingID: meetingID), to: encoder)
    case .failed(let message): try CaseCoding.encode(kind, payload: message, to: encoder)
    default: try CaseCoding.encode(kind, to: encoder)
    }
  }
}

/// Progress of one phone recording being handed over. Idempotency key of
/// `RecordingIntake.admit`: a recording that already reached `.complete`
/// returns the same meeting id.
public struct HandoverReceipt: Codable, Sendable, Equatable, Hashable, Identifiable {
  public var recordingID: UUID
  public var deviceID: UUID
  public var state: HandoverState
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
    state: HandoverState,
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
