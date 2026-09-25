import Foundation
import NIOHTTP1

/// What a request must carry before its body is read.
enum AuthRequirement: Sendable, Equatable {
  /// `/v1/hello`: anyone who completed the pinned handshake.
  case none
  /// `Authorization: Pairing <secret>` from the QR code.
  case pairing
  /// `Authorization: Bearer <token>` of a paired device.
  case bearer
}

/// The seven routes of the wire, matched from method and path. Anything else
/// is 404 and never has its body read.
enum Route: Sendable, Equatable {
  case hello
  case pair
  case unpair
  case announce(UUID)
  case status(UUID)
  case chunk(UUID, Int)
  case complete(UUID)

  static func match(method: HTTPMethod, uri: String) -> Route? {
    let path =
      uri.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false).first
      .map(String.init) ?? uri
    let parts = path.split(separator: "/", omittingEmptySubsequences: true).map {
      $0.removingPercentEncoding ?? String($0)
    }
    guard parts.first == "v1" else { return nil }
    switch (method, parts.count) {
    case (.GET, 2) where parts[1] == "hello":
      return .hello
    case (.POST, 2) where parts[1] == "pair":
      return .pair
    case (.DELETE, 2) where parts[1] == "pairing":
      return .unpair
    case (.PUT, 3) where parts[1] == "recordings":
      return UUID(uuidString: parts[2]).map(Route.announce)
    case (.GET, 3) where parts[1] == "recordings":
      return UUID(uuidString: parts[2]).map(Route.status)
    case (.PUT, 5) where parts[1] == "recordings" && parts[3] == "chunks":
      guard let id = UUID(uuidString: parts[2]), let index = Int(parts[4]), index >= 0,
        String(index) == parts[4]
      else { return nil }
      return .chunk(id, index)
    case (.POST, 4) where parts[1] == "recordings" && parts[3] == "complete":
      return UUID(uuidString: parts[2]).map(Route.complete)
    default:
      return nil
    }
  }

  var auth: AuthRequirement {
    switch self {
    case .hello: .none
    case .pair: .pairing
    case .unpair, .announce, .status, .chunk, .complete: .bearer
    }
  }

  /// Chunk bodies may be a chunk plus 64 KiB; everything else is small JSON.
  func bodyLimit(_ configuration: HandoverConfiguration) -> Int {
    switch self {
    case .chunk: configuration.chunkBodyLimit
    default: HandoverConfiguration.jsonBodyLimit
    }
  }
}
