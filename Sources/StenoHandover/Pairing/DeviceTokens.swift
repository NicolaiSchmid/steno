import Crypto
import Foundation

/// Bearer tokens: 32 random bytes as standard base64. The Mac stores only
/// `hash(token)` (`MeetingStore.save(_:tokenHash:)`), so a copy of the
/// database pairs no phone.
enum DeviceTokens {
  static let byteCount = 32

  static func mint() -> String {
    var bytes = [UInt8](repeating: 0, count: byteCount)
    var generator = SystemRandomNumberGenerator()
    for index in bytes.indices {
      bytes[index] = generator.next()
    }
    return Data(bytes).base64EncodedString()
  }

  static func hash(_ token: String) -> Data {
    Data(SHA256.hash(data: Data(token.utf8)))
  }

  /// 32 random bytes for the pairing secret.
  static func randomBytes() -> Data {
    var bytes = [UInt8](repeating: 0, count: byteCount)
    var generator = SystemRandomNumberGenerator()
    for index in bytes.indices {
      bytes[index] = generator.next()
    }
    return Data(bytes)
  }
}
