import Crypto
import Foundation
import StenoCore
import Testing
import X509

@testable import StenoHandover

/// Mint, fingerprint, Mac id and the committed test identity: the values
/// the phone pins and shows.
@Suite struct IdentityTests {
  @Test func mintYieldsSelfSignedP256CertificateForTenYears() throws {
    let now = Date(timeIntervalSince1970: 1_790_000_000)
    let minted = try MintedIdentity.mint(commonName: "Steno on Test Mac", now: now)
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
    let a = try MintedIdentity.mint(commonName: "A")
    let b = try MintedIdentity.mint(commonName: "A")
    #expect(a.fingerprint.count == 32)
    #expect(a.fingerprint == HandoverIdentity.fingerprint(ofDER: a.certificateDER))
    #expect(a.fingerprint != b.fingerprint)
    #expect(a.certificateDER != b.certificateDER)
  }

  @Test func macIDDerivesFromTheFingerprintDeterministically() throws {
    let fingerprint = Data(repeating: 0xAB, count: 32)
    let first = HandoverIdentity.macID(forFingerprint: fingerprint)
    let second = HandoverIdentity.macID(forFingerprint: fingerprint)
    #expect(first == second)
    #expect(first != HandoverIdentity.macID(forFingerprint: Data(repeating: 0xAC, count: 32)))
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

  /// The SEC1 DER `IdentityKeychain` imports (#88): RFC 5915 with the named
  /// curve and the public key, read back by swift-crypto as the same key.
  @Test func privateKeySEC1DERRoundTripsAndNamesTheCurve() throws {
    let minted = try MintedIdentity.mint(commonName: "A")
    let der = try minted.privateKeySEC1DER()
    #expect(der.first == 0x30, "a SEQUENCE")
    let parsed = try P256.Signing.PrivateKey(derRepresentation: der)
    #expect(parsed.rawRepresentation == minted.privateKey.rawRepresentation)
    #expect(parsed.publicKey.x963Representation == minted.privateKey.publicKey.x963Representation)
    // 1.2.840.10045.3.1.7 (prime256v1), explicitly tagged [0].
    let curve: [UInt8] = [0xA0, 0x0A, 0x06, 0x08, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07]
    #expect(Array(der).firstRange(of: curve) != nil)
    #expect(der != minted.privateKey.derRepresentation, "PKCS#8 is a different shape")
  }

  @Test func sanLabelIsDNSSafe() {
    #expect(
      MintedIdentity.sanLabel("Steno on Nicolai's MacBook Pro")
        == "steno-on-nicolai-s-macbook-pro.local")
    #expect(MintedIdentity.sanLabel("---") == "steno.local")
  }
}
