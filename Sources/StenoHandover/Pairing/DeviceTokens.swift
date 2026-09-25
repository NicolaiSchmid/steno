import Crypto
import Foundation

/// Bearer tokens: 32 random bytes as standard base64. The Mac stores only
/// `hash(token)` (`MeetingStore.save(_:tokenHash:)`), so a copy of the
/// database pairs no phone.
enum DeviceTokens {
  static let byteCount = 32

  static func mint() -> String {
    randomBytes().base64EncodedString()
  }

  static func hash(_ token: String) -> Data {
    Data(SHA256.hash(data: Data(token.utf8)))
  }

  /// 32 bytes from the system generator, for tokens and pairing secrets.
  static func randomBytes() -> Data {
    Data((0..<byteCount).map { _ in UInt8.random(in: .min ... .max) })
  }
}
