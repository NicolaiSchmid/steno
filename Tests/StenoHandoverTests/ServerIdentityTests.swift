import Crypto
import Foundation
import StenoCore
import Testing
import X509

@testable import StenoHandover

@Suite struct ServerIdentityTests {
  @Test func mintYieldsSelfSignedP256CertificateForTenYears() throws {
    let now = Date(timeIntervalSince1970: 1_790_000_000)
    let minted = try ServerIdentity.mint(commonName: "Steno on Test Mac", now: now)
    let certificate = try minted.certificate()

    #expect(String(describing: certificate.subject) == "CN=Steno on Test Mac")
    #expect(certificate.issuer == certificate.subject)
    #expect(certificate.signatureAlgorithm == .ecdsaWithSHA256)
    #expect(P256.Signing.PublicKey(certificate.publicKey) != nil)
    #expect(certificate.publicKey == Certificate.PublicKey(minted.privateKey.publicKey))
    #expect(certificate.notValidBefore <= now)
    let years =
      certificate.notValidAfter.timeIntervalSince(certificate.notValidBefore) / (365 * 24 * 3600)
    #expect(years == 10)
    #expect(certificate.publicKey.isValidSignature(certificate.signature, for: certificate))
    let constraints = try certificate.extensions.basicConstraints
    #expect(constraints == .notCertificateAuthority)
    // RFC 5480 §3: keyEncipherment is not a use an EC key has; a strict
    // validator rejects a critical KeyUsage that claims it.
    #expect(try certificate.extensions.keyUsage == KeyUsage(digitalSignature: true))
  }

  @Test func fingerprintIsStableForOneDERAndDiffersBetweenMints() throws {
    let a = try ServerIdentity.mint(commonName: "A")
    let b = try ServerIdentity.mint(commonName: "A")
    #expect(a.fingerprint.count == 32)
    #expect(a.fingerprint == ServerIdentity.fingerprint(der: a.certificateDER))
    #expect(a.fingerprint != b.fingerprint)
    #expect(a.certificateDER != b.certificateDER)
  }

  @Test func macIDDerivesFromTheFingerprintDeterministically() throws {
    let fingerprint = Data(repeating: 0xAB, count: 32)
    let first = MacIdentifier.derive(fromFingerprint: fingerprint)
    let second = MacIdentifier.derive(fromFingerprint: fingerprint)
    #expect(first == second)
    #expect(first != MacIdentifier.derive(fromFingerprint: Data(repeating: 0xAC, count: 32)))
    // Version 4, variant 1, so it never collides with random ids by shape.
    #expect(
      first.uuidString[first.uuidString.index(first.uuidString.startIndex, offsetBy: 14)] == "4")
  }

  @Test func testIdentityLoadsWithTheRecordedFingerprint() throws {
    let identity = try TestIdentity.load()
    #expect(ContentHash.hex(identity.fingerprint) == TestIdentity.fingerprintHex)
    let certificate = try Certificate(derEncoded: Array(identity.certificateDER))
    #expect(String(describing: certificate.subject) == "CN=\(TestIdentity.commonName)")
    #expect(P256.Signing.PublicKey(certificate.publicKey) != nil)
  }

  @Test func sanLabelIsDNSSafe() {
    #expect(
      ServerIdentity.sanLabel("Steno on Nicolai's MacBook Pro")
        == "steno-on-nicolai-s-macbook-pro.local")
    #expect(ServerIdentity.sanLabel("---") == "steno.local")
  }
}
