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
/// loopback listener.
enum TestIdentity {
  static let password = "steno-test"
  static let commonName = "Steno test identity"
  /// SHA-256 of `test-identity.der`, recorded when the fixture was generated.
  static let fingerprintHex =
    "76ac0c0b28f976f25589214d5b2c614d983df9de081b06ec1fe9247bf1e40d92"

  static var p12URL: URL { Fixtures.url("handover/test-identity.p12") }
  static var derURL: URL { Fixtures.url("handover/test-identity.der") }

  static func load() throws -> HandoverIdentity {
    #if canImport(Security)
      let data = try Data(contentsOf: p12URL)
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
      return HandoverIdentity(certificateDER: try Data(contentsOf: derURL))
    #endif
  }
}
