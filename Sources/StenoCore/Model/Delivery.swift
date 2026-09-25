import Foundation

public enum DeliveryStatus: Codable, Sendable, Equatable, Hashable {
  case pending
  case delivered
  case failed(String)

  /// The case names, shared by `meeting.json` and the `delivery.status`
  /// column.
  public enum Kind: String, CaseIterable, Codable, Sendable {
    case pending, delivered, failed
  }

  public var kind: Kind {
    switch self {
    case .pending: .pending
    case .delivered: .delivered
    case .failed: .failed
    }
  }

  public init(from decoder: any Decoder) throws {
    let (kind, payload) = try CaseCoding.decode(Kind.self, from: decoder)
    switch kind {
    case .pending: self = .pending
    case .delivered: self = .delivered
    case .failed:
      self = .failed(try CaseCoding.decodePayload(String.self, from: payload, case: kind))
    }
  }

  public func encode(to encoder: any Encoder) throws {
    switch self {
    case .failed(let message): try CaseCoding.encode(kind, payload: message, to: encoder)
    default: try CaseCoding.encode(kind, to: encoder)
    }
  }
}

/// Per-destination delivery state of one meeting. There is one row per
/// (meeting, destination): `id` derives from the pair, so every dispatcher
/// computes the same id and `MeetingStore.save` is a plain upsert.
public struct Delivery: Codable, Sendable, Equatable, Hashable, Identifiable {
  public var id: UUID
  public var meetingID: UUID
  public var destinationID: String
  public var status: DeliveryStatus
  public var lastAttemptAt: Date?
  public var receipt: DeliveryReceipt?

  public init(
    meetingID: UUID,
    destinationID: String,
    status: DeliveryStatus,
    lastAttemptAt: Date? = nil,
    receipt: DeliveryReceipt? = nil
  ) {
    self.id = Self.id(meetingID: meetingID, destinationID: destinationID)
    self.meetingID = meetingID
    self.destinationID = destinationID
    self.status = status
    self.lastAttemptAt = lastAttemptAt
    self.receipt = receipt
  }

  /// The one id of the (meeting, destination) pair.
  public static func id(meetingID: UUID, destinationID: String) -> UUID {
    UUID(derivedFrom: meetingID, salt: "delivery-\(destinationID)")
  }
}

/// Whether a delivered file belongs to Steno outright or is a block inside a
/// file the user also edits.
public enum FileOwnership: String, Codable, Sendable, Equatable, Hashable {
  case owned
  case managedBlock
}

public struct DeliveredFile: Codable, Sendable, Equatable, Hashable {
  public var relativePath: String
  public var ownership: FileOwnership
  public var sha256: Data

  public init(relativePath: String, ownership: FileOwnership, sha256: Data) {
    self.relativePath = relativePath
    self.ownership = ownership
    self.sha256 = sha256
  }
}

/// What a `Destination` wrote, passed back as `previous` on re-delivery so it
/// overwrites its own files and never touches anything else.
public struct DeliveryReceipt: Codable, Sendable, Equatable, Hashable {
  /// The destination root at delivery time, for example the vault path.
  public var root: String
  /// The meeting folder relative to `root`, pinned at first delivery.
  public var folder: String
  public var files: [DeliveredFile]
  public var rendererVersion: Int

  public init(root: String, folder: String, files: [DeliveredFile], rendererVersion: Int) {
    self.root = root
    self.folder = folder
    self.files = files
    self.rendererVersion = rendererVersion
  }
}
