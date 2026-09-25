import CryptoKit
import Foundation
import Security

/// The one place that decides whether a TLS server is the paired Mac.
///
/// Trust is the SHA-256 of the leaf certificate's DER bytes, compared in
/// constant time against the fingerprint the phone learned from the pairing
/// QR code (plan decision 3). No system trust evaluation is consulted: the
/// Mac's certificate is self-signed and rotation is a non-goal.
///
/// This file is the canonical copy. `Tests/StenoHandoverTests/Support/`
/// symlinks to it so the Mac's `PinningTests` compile the same bytes. Keep it
/// to Foundation, Security and CryptoKit; no Expo or UIKit imports.
public enum PinnedTrustEvaluator {
  /// SHA-256 over `SecCertificateCopyData`, the same bytes the Mac hashes.
  public static func fingerprint(of certificate: SecCertificate) -> Data {
    let der = SecCertificateCopyData(certificate) as Data
    return Data(SHA256.hash(data: der))
  }

  /// Fingerprint of the leaf (index 0) of the presented chain, or nil when empty.
  public static func leafFingerprint(of trust: SecTrust) -> Data? {
    guard let chain = SecTrustCopyCertificateChain(trust) as? [SecCertificate],
      let leaf = chain.first
    else {
      return nil
    }
    return fingerprint(of: leaf)
  }

  /// True iff the leaf's fingerprint equals `pinnedFingerprint` (32 bytes).
  public static func evaluate(_ trust: SecTrust, pinnedFingerprint: Data) -> Bool {
    guard pinnedFingerprint.count == SHA256.byteCount,
      let actual = leafFingerprint(of: trust)
    else {
      return false
    }
    return constantTimeEquals(actual, pinnedFingerprint)
  }

  /// URLSession challenge adapter: accepts server trust only through `evaluate`.
  /// Anything but a server-trust challenge falls through to default handling;
  /// the handover protocol never issues HTTP authentication challenges.
  public static func respond(
    to challenge: URLAuthenticationChallenge,
    pinnedFingerprint: Data
  ) -> (URLSession.AuthChallengeDisposition, URLCredential?) {
    guard challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust else {
      return (.performDefaultHandling, nil)
    }
    guard let trust = challenge.protectionSpace.serverTrust,
      evaluate(trust, pinnedFingerprint: pinnedFingerprint)
    else {
      return (.cancelAuthenticationChallenge, nil)
    }
    return (.useCredential, URLCredential(trust: trust))
  }

  public static func constantTimeEquals(_ lhs: Data, _ rhs: Data) -> Bool {
    guard lhs.count == rhs.count else { return false }
    var difference: UInt8 = 0
    for (a, b) in zip(lhs, rhs) {
      difference |= a ^ b
    }
    return difference == 0
  }
}
