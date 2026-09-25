import Foundation
import StenoCore

/// The JSON bodies of the handover wire (v1), encoded with `StenoJSON`.
/// `mobile/modules/steno-link/src/wire.ts` mirrors every name here;
/// `RecordingMetadata` is core's type.
public enum Wire {
  public static let protocolVersion = 1
  public static let serviceType = "_steno._tcp"
  public static let chunkHashHeader = "X-Steno-Chunk-SHA256"

  /// `GET /v1/hello`.
  public struct Hello: Codable, Sendable, Equatable {
    public var macID: UUID
    public var `protocol`: Int

    public init(macID: UUID, protocol: Int = Wire.protocolVersion) {
      self.macID = macID
      self.`protocol` = `protocol`
    }
  }

  /// `POST /v1/pair` body, sent with `Authorization: Pairing <secret>`.
  public struct PairRequest: Codable, Sendable, Equatable {
    public var deviceID: UUID
    public var deviceName: String

    public init(deviceID: UUID, deviceName: String) {
      self.deviceID = deviceID
      self.deviceName = deviceName
    }
  }

  /// `POST /v1/pair` response; `token` is the bearer for every later call.
  public struct PairResponse: Codable, Sendable, Equatable {
    public var token: String
    public var macID: UUID
    public var macName: String

    public init(token: String, macID: UUID, macName: String) {
      self.token = token
      self.macID = macID
      self.macName = macName
    }
  }

  /// `GET /v1/recordings/{id}`, the announce response and the 409 body.
  public struct RecordingStatus: Codable, Sendable, Equatable {
    public var state: HandoverState.Kind
    public var receivedChunks: [Int]

    public init(state: HandoverState.Kind, receivedChunks: [Int]) {
      self.state = state
      self.receivedChunks = receivedChunks
    }

    public init(_ receipt: HandoverReceipt) {
      self.init(state: receipt.state.kind, receivedChunks: receipt.receivedChunks)
    }
  }

  /// `POST /v1/recordings/{id}/complete` 200 body.
  public struct CompleteResponse: Codable, Sendable, Equatable {
    public var meetingID: UUID

    public init(meetingID: UUID) {
      self.meetingID = meetingID
    }
  }

  /// Every error status carries one of these, for logs on the phone.
  public struct Problem: Codable, Sendable, Equatable {
    public var error: String

    public init(_ error: String) {
      self.error = error
    }
  }
}
