import Foundation

public enum DeliveryStatus: Codable, Sendable, Equatable, Hashable {
  case pending
  case delivered
  case failed(String)

  public init(from decoder: any Decoder) throws {
    let (name, payload) = try CaseCoding.decode(from: decoder)
    switch name {
    case "pending": self = .pending
    case "delivered": self = .delivered
    case "failed":
      self = .failed(try CaseCoding.decodePayload(String.self, from: payload, case: name))
    default: throw CaseCoding.unknownCase(name, in: decoder)
    }
  }

  public func encode(to encoder: any Encoder) throws {
    switch self {
    case .pending: try CaseCoding.encode("pending", to: encoder)
    case .delivered: try CaseCoding.encode("delivered", to: encoder)
    case .failed(let message): try CaseCoding.encode("failed", payload: message, to: encoder)
    }
  }
}

/// Per-destination delivery state of one meeting.
public struct Delivery: Codable, Sendable, Equatable, Hashable, Identifiable {
  public var id: UUID
  public var meetingID: UUID
  public var destinationID: String
  public var status: DeliveryStatus
  public var lastAttemptAt: Date?
  public var receipt: DeliveryReceipt?

  public init(
    id: UUID,
    meetingID: UUID,
    destinationID: String,
    status: DeliveryStatus,
    lastAttemptAt: Date? = nil,
    receipt: DeliveryReceipt? = nil
  ) {
    self.id = id
    self.meetingID = meetingID
    self.destinationID = destinationID
    self.status = status
    self.lastAttemptAt = lastAttemptAt
    self.receipt = receipt
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
