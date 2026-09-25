import Foundation

/// base64url without padding (RFC 4648 §5), used only inside the pairing QR
/// URL; every JSON body and header uses standard base64 (`StenoJSON`).
enum Base64URL {
  static func encode(_ data: Data) -> String {
    data.base64EncodedString()
      .replacingOccurrences(of: "+", with: "-")
      .replacingOccurrences(of: "/", with: "_")
      .replacingOccurrences(of: "=", with: "")
  }

  /// Accepts unpadded and padded input; rejects characters outside the
  /// alphabet.
  static func decode(_ string: String) -> Data? {
    let allowed = Set("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_=")
    guard string.allSatisfy(allowed.contains) else { return nil }
    var standard =
      string
      .replacingOccurrences(of: "-", with: "+")
      .replacingOccurrences(of: "_", with: "/")
      .replacingOccurrences(of: "=", with: "")
    let remainder = standard.count % 4
    if remainder == 1 { return nil }
    if remainder > 0 { standard += String(repeating: "=", count: 4 - remainder) }
    return Data(base64Encoded: standard)
  }
}
