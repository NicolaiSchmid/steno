import Foundation
import NIOHTTP1
import StenoCore

/// The protocol core behind the listener: the auth gate and every route,
/// independent of NIO and of TLS so the same code runs on Linux in tests.
/// Owned by `HandoverService`; one actor, so pairing, tokens and partial
/// files are touched by one request at a time.
actor HandoverEngine: RequestHandling {
  let configuration: HandoverConfiguration
  let macID: UUID
  let store: MeetingStore
  let intake: any HandoverIntake
  let now: @Sendable () -> Date

  init(
    configuration: HandoverConfiguration,
    macID: UUID,
    store: MeetingStore,
    intake: any HandoverIntake,
    now: @escaping @Sendable () -> Date
  ) {
    self.configuration = configuration
    self.macID = macID
    self.store = store
    self.intake = intake
    self.now = now
  }

  // MARK: - Auth gate

  func authenticate(_ route: Route, authorization: String?) async -> AuthOutcome {
    switch route.auth {
    case .none:
      return .allowed(.anonymous)
    case .pairing:
      return .forbidden
    case .bearer:
      guard let token = Self.credential(scheme: "Bearer", in: authorization),
        let device = try? await store.device(forTokenHash: DeviceTokens.hash(token))
      else {
        return .unauthorized
      }
      return .allowed(.device(device))
    }
  }

  /// The credential after `<scheme> ` in an `Authorization` header, case
  /// insensitive on the scheme.
  static func credential(scheme: String, in authorization: String?) -> String? {
    guard let authorization else { return nil }
    let parts = authorization.split(separator: " ", maxSplits: 1, omittingEmptySubsequences: true)
    guard parts.count == 2, parts[0].lowercased() == scheme.lowercased() else { return nil }
    let credential = parts[1].trimmingCharacters(in: .whitespaces)
    return credential.isEmpty ? nil : credential
  }

  // MARK: - Routes

  func handle(_ request: HandoverRequest) async -> HandoverResponse {
    switch request.route {
    case .hello:
      return .json(.ok, Wire.Hello(macID: macID))
    case .pair, .unpair, .announce, .status, .chunk, .complete:
      return .problem(.notImplemented, "not yet")
    }
  }
}
