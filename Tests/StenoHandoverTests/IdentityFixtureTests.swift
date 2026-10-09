import Crypto
import Foundation
import StenoCore
import Testing
import X509

@testable import StenoHandover

/// The committed test identity as the Rust app's import of the Swift
/// identity sees it: `crates/steno-services/tests/swift_keychain.rs`
/// stores `test-identity.der` and `test-identity.key.der` the way
/// `IdentityKeychain.store` does, exports them and must get back the
/// fingerprint and Mac id computed here, the values every paired phone
/// pins and shows.
@Suite struct IdentityFixtureTests {
  @Test func theFixtureHasTheFingerprintAndMacIDTheRustImportKeeps() throws {
    let der = try Data(contentsOf: Fixtures.url("handover/test-identity.der"))
    let fingerprint = HandoverIdentity.fingerprint(ofDER: der)
    #expect(
      ContentHash.hex(fingerprint)
        == "76ac0c0b28f976f25589214d5b2c614d983df9de081b06ec1fe9247bf1e40d92")
    #expect(
      HandoverIdentity.macID(forFingerprint: fingerprint).uuidString
        == "FCF0D2A1-D2CE-48F7-BAFF-E17FD2E9C814")
  }

  /// The SEC1 key beside it is the certificate's: the keychain test pairs
  /// the two into one identity.
  @Test func theSEC1KeyFixtureIsTheCertificatesKey() throws {
    let certificate = try Certificate(
      derEncoded: Array(try Data(contentsOf: Fixtures.url("handover/test-identity.der"))))
    let key = try P256.Signing.PrivateKey(
      derRepresentation: try Data(contentsOf: Fixtures.url("handover/test-identity.key.der")))
    #expect(Certificate.PublicKey(key.publicKey) == certificate.publicKey)
  }
}
