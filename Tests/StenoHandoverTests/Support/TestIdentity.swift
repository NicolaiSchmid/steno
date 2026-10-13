import Foundation
import StenoCore

@testable import StenoHandover

#if canImport(Security)
  import Security
#endif

/// The committed test identity `Tests/Fixtures/handover/test-identity.p12`
/// (P-256, `CN=Steno test identity`, generated once with openssl, password
/// below, test-only). On Apple platforms it is imported with
/// `SecPKCS12Import` and `kSecImportToMemoryOnly`, so no keychain is touched;
/// on Linux only the certificate DER beside it is read, for the plaintext
/// loopback listener. `other-identity` beside it (`CN=Steno other test
/// identity`, same password) is a second one, for a keychain holding
/// another identity, and `third-identity` (`CN=Steno third test identity`)
/// a third.
enum TestIdentity {
  static let password = "steno-test"
  static let commonName = "Steno test identity"
  /// SHA-256 of `test-identity.der`, recorded when the fixture was generated.
  static let fingerprintHex =
    "76ac0c0b28f976f25589214d5b2c614d983df9de081b06ec1fe9247bf1e40d92"

  static var p12URL: URL { Fixtures.url("handover/test-identity.p12") }

  static func load(_ name: String = "test-identity") throws -> HandoverIdentity {
    #if canImport(Security)
      let data = try Data(contentsOf: Fixtures.url("handover/\(name).p12"))
      var items: CFArray?
      let options: [CFString: Any] = [
        kSecImportExportPassphrase: password,
        kSecImportToMemoryOnly: true,
      ]
      let status = SecPKCS12Import(data as CFData, options as CFDictionary, &items)
      guard status == errSecSuccess else {
        throw IdentityError.security("SecPKCS12Import", status)
      }
      guard let first = (items as? [[CFString: Any]])?.first,
        let identity = first[kSecImportItemIdentity]
      else {
        throw IdentityError.malformed("the test p12 holds no identity")
      }
      return try HandoverIdentity(
        secIdentity: unsafeBitCast(identity as CFTypeRef, to: SecIdentity.self))
    #else
      return HandoverIdentity(
        certificateDER: try Data(contentsOf: Fixtures.url("handover/\(name).der")))
    #endif
  }

  /// An identity whose certificate starts at `start`, a whole second: a
  /// fresh mint from a minute after it, beside the test identity's key on
  /// Apple platforms (the guard reads only the certificate's start and
  /// fingerprint). Two of them, or one and a fixture, can start in the same
  /// second, or after a pinned now. A mint encodes its start in whole
  /// seconds, so a fractional `start` is refused rather than rounded.
  static func minted(startingAt start: Date) throws -> HandoverIdentity {
    let der = try MintedIdentity.mint(
      commonName: "Steno minted test identity", now: start.addingTimeInterval(60)
    ).certificateDER
    #if canImport(Security)
      let identity = HandoverIdentity(secIdentity: try load().secIdentity, certificateDER: der)
    #else
      let identity = HandoverIdentity(certificateDER: der)
    #endif
    guard try identity.notValidBefore() == start else {
      throw IdentityError.malformed("a certificate minted to start at \(start) starts otherwise")
    }
    return identity
  }
}
