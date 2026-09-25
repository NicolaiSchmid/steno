import CryptoKit
import Foundation

/// SHA-256 for delivery receipts, the fixture manifest and handover
/// verification.
public enum ContentHash {
  public static func sha256(_ data: Data) -> Data {
    Data(SHA256.hash(data: data))
  }

  public static func hex(_ digest: Data) -> String {
    digest.map { String(format: "%02x", $0) }.joined()
  }

  public static func sha256Hex(_ data: Data) -> String {
    hex(sha256(data))
  }
}
