import Foundation
import StenoCore

/// The JSON bodies of the handover wire (v1), encoded with `StenoJSON`.
/// `mobile/modules/steno-link/src/wire.ts` mirrors every name here;
/// `RecordingMetadata` is core's type. Internal: the app reads wire values
/// only through `HandoverReceipt` and `PairingPayload`.
enum Wire {
  static let protocolVersion = 1
  static let serviceType = "_steno._tcp"
  static let chunkHashHeader = "X-Steno-Chunk-SHA256"

  /// `GET /v1/hello`.
  struct Hello: Codable, Sendable, Equatable {
    var macID: UUID
    var `protocol`: Int

    init(macID: UUID, protocol: Int = Wire.protocolVersion) {
      self.macID = macID
      self.`protocol` = `protocol`
    }
  }

  /// `POST /v1/pair` body, sent with `Authorization: Pairing <secret>`.
  struct PairRequest: Codable, Sendable, Equatable {
    var deviceID: UUID
    var deviceName: String
  }

  /// `POST /v1/pair` response; `token` is the bearer for every later call.
  struct PairResponse: Codable, Sendable, Equatable {
    var token: String
    var macID: UUID
    var macName: String
  }

  /// `GET /v1/recordings/{id}`, the announce response and the 409 body.
  struct RecordingStatus: Codable, Sendable, Equatable {
    var state: HandoverState.Kind
    var receivedChunks: [Int]
  }

  /// `POST /v1/recordings/{id}/complete` 200 body.
  struct CompleteResponse: Codable, Sendable, Equatable {
    var meetingID: UUID
  }

  /// Every error status carries one of these, for logs on the phone.
  struct Problem: Codable, Sendable, Equatable {
    var error: String

    init(_ error: String) {
      self.error = error
    }
  }
}
