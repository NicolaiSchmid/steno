import Crypto
import Foundation

/// One open pairing window: the secret in the QR code, expiring on the
/// injected wall clock. Single use because the engine drops the session
/// before it saves the paired device; `beginPairing` replaces any open
/// session.
struct PairingSession: Sendable {
  let payload: PairingPayload
  private let now: @Sendable () -> Date

  init(
    macID: UUID, macName: String, fingerprint: Data, window: Duration,
    now: @escaping @Sendable () -> Date
  ) {
    self.now = now
    self.payload = PairingPayload(
      macID: macID, macName: macName, fingerprint: fingerprint,
      secret: DeviceTokens.randomBytes(),
      expiresAt: now().addingTimeInterval(window / .seconds(1)))
  }

  var isOpen: Bool { now() < payload.expiresAt }

  /// Checks a presented credential without leaking timing: both sides are
  /// hashed and swift-crypto compares digests in constant time. The phone
  /// sends the secret as standard base64 (`wire.ts` converts the QR's
  /// base64url); both encodings are accepted.
  func matches(_ presented: String) -> Bool {
    guard isOpen, let bytes = Data(base64Encoded: presented) ?? Base64URL.decode(presented) else {
      return false
    }
    return SHA256.hash(data: bytes) == SHA256.hash(data: payload.secret)
  }
}
