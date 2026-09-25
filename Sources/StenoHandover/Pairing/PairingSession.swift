import Foundation

/// One open pairing window: the secret in the QR code, single use, expiring
/// on the injected clock. `beginPairing` replaces any open session.
struct PairingSession: Sendable {
  let payload: PairingPayload
  /// True once the window on the injected clock has passed.
  let isExpired: @Sendable () -> Bool
  var used = false

  init(
    macID: UUID, macName: String, fingerprint: Data, window: Duration,
    clock: any Clock<Duration>, now: Date
  ) {
    self.isExpired = clock.expiryCheck(after: window)
    self.payload = PairingPayload(
      macID: macID, macName: macName, fingerprint: fingerprint,
      secret: DeviceTokens.randomBytes(),
      expiresAt: now.addingTimeInterval(window.seconds))
  }

  var isOpen: Bool { !used && !isExpired() }

  /// Constant-time check of a presented credential. The phone sends the
  /// secret as standard base64 (`wire.ts` converts the QR's base64url);
  /// both encodings are accepted.
  func matches(_ presented: String) -> Bool {
    guard isOpen,
      let bytes = Data(base64Encoded: presented) ?? Base64URL.decode(presented)
    else {
      return false
    }
    return ConstantTime.equals(bytes, payload.secret)
  }
}

extension Clock where Duration == Swift.Duration {
  /// A check that turns true once `window` has passed on this clock.
  func expiryCheck(after window: Duration) -> @Sendable () -> Bool {
    let deadline = now.advanced(by: window)
    return { self.now >= deadline }
  }
}

extension Duration {
  /// Whole and fractional seconds as a `TimeInterval`.
  var seconds: TimeInterval {
    TimeInterval(components.seconds) + TimeInterval(components.attoseconds) / 1e18
  }
}
