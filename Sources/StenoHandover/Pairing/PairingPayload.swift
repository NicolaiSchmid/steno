import Foundation

/// What the QR code carries, and the deep link it doubles as:
///
///     steno://pair/v1?mac=<uuid>&name=<pct>&fp=<base64url>&secret=<base64url>&exp=<unix>
///
/// `fp` is the SHA-256 of the Mac's leaf certificate DER, `secret` the
/// single-use pairing secret, both 32 bytes in base64url without padding
/// (the one place the wire uses base64url; JSON and headers use standard
/// base64). The phone's `pairing-payload.ts` parses exactly this.
public struct PairingPayload: Codable, Sendable, Equatable {
  public static let scheme = "steno"
  public static let version = "v1"

  public let macID: UUID
  public let macName: String
  public let fingerprint: Data
  public let secret: Data
  /// Whole seconds; the phone refuses a code past this on its own clock.
  public let expiresAt: Date

  public init(macID: UUID, macName: String, fingerprint: Data, secret: Data, expiresAt: Date) {
    self.macID = macID
    self.macName = macName
    self.fingerprint = fingerprint
    self.secret = secret
    self.expiresAt = Date(timeIntervalSince1970: expiresAt.timeIntervalSince1970.rounded(.down))
  }

  public var urlString: String {
    let name =
      macName.addingPercentEncoding(withAllowedCharacters: Self.unreserved) ?? "Steno"
    return "\(Self.scheme)://pair/\(Self.version)?mac=\(macID.uuidString.lowercased())"
      + "&name=\(name)&fp=\(Base64URL.encode(fingerprint))&secret=\(Base64URL.encode(secret))"
      + "&exp=\(Int64(expiresAt.timeIntervalSince1970))"
  }

  public var url: URL { URL(string: urlString)! }

  public init(parsing url: URL) throws {
    guard url.scheme?.lowercased() == Self.scheme, url.host?.lowercased() == "pair" else {
      throw PairingPayloadError.notSteno
    }
    guard url.path == "/\(Self.version)" else { throw PairingPayloadError.version }
    var fields: [String: String] = [:]
    for item in URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? [] {
      guard fields[item.name] == nil else {
        throw PairingPayloadError.badEncoding("duplicate \(item.name)")
      }
      fields[item.name] = item.value ?? ""
    }
    guard let mac = fields["mac"], let name = fields["name"], let fp = fields["fp"],
      let secret = fields["secret"], let exp = fields["exp"]
    else {
      throw PairingPayloadError.missingField
    }
    guard let macID = UUID(uuidString: mac) else { throw PairingPayloadError.badEncoding("mac") }
    guard !name.isEmpty else { throw PairingPayloadError.badEncoding("name") }
    guard let fingerprint = Base64URL.decode(fp), fingerprint.count == 32 else {
      throw PairingPayloadError.badEncoding("fp")
    }
    guard let secretBytes = Base64URL.decode(secret), secretBytes.count == 32 else {
      throw PairingPayloadError.badEncoding("secret")
    }
    guard exp.count <= 12, exp.allSatisfy(\.isNumber), let seconds = TimeInterval(exp) else {
      throw PairingPayloadError.badEncoding("exp")
    }
    self.init(
      macID: macID, macName: name, fingerprint: fingerprint, secret: secretBytes,
      expiresAt: Date(timeIntervalSince1970: seconds))
  }

  public func isExpired(at now: Date) -> Bool { now >= expiresAt }

  /// RFC 3986 unreserved characters; everything else, including `+`, is
  /// percent-encoded so the phone's `decodeURIComponent` reads it back.
  static let unreserved = CharacterSet(
    charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")
}

public enum PairingPayloadError: Error, Equatable, CustomStringConvertible, Sendable {
  case notSteno
  case version
  case missingField
  case badEncoding(String)

  public var description: String {
    switch self {
    case .notSteno: "not a steno://pair URL"
    case .version: "unsupported pairing payload version"
    case .missingField: "a pairing field is missing"
    case .badEncoding(let field): "pairing field \(field) is malformed"
    }
  }
}
