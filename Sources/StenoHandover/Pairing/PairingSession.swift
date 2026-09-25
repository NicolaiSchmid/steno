import Crypto
import Foundation

/// One open pairing window: the secret in the QR code, expiring on the
/// injected clock. Single use because the engine drops the session once it
/// pairs; `beginPairing` replaces any open session.
struct PairingSession: Sendable {
  let payload: PairingPayload
  /// True once the window on the injected clock has passed.
  let isExpired: @Sendable () -> Bool

  init(
    macID: UUID, macName: String, fingerprint: Data, window: Duration,
    clock: any Clock<Duration>, now: Date
  ) {
    self.isExpired = clock.expiryCheck(after: window)
    self.payload = PairingPayload(
      macID: macID, macName: macName, fingerprint: fingerprint,
      secret: DeviceTokens.randomBytes(),
      expiresAt: now.addingTimeInterval(window / .seconds(1)))
  }

  var isOpen: Bool { !isExpired() }

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

extension Clock where Duration == Swift.Duration {
  /// A check that turns true once `window` has passed on this clock.
  func expiryCheck(after window: Duration) -> @Sendable () -> Bool {
    let deadline = now.advanced(by: window)
    return { self.now >= deadline }
  }
}
