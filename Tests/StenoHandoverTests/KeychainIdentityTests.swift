import Foundation
import StenoCore
import Testing
import X509

@testable import StenoHandover

#if canImport(Security)
  import Security

  /// Opt-in: touches the login keychain. `STENO_KEYCHAIN_TESTS=1` enables it;
  /// the hosted runner and every default `swift test` skip it.
  @Suite(.serialized) struct KeychainIdentityTests {
    static var enabled: Bool { ProcessInfo.processInfo.environment["STENO_KEYCHAIN_TESTS"] == "1" }

    @Test(.enabled(if: enabled, "set STENO_KEYCHAIN_TESTS=1 to run the login keychain round trip"))
    func storeLoadDeleteUnderAUniqueLabel() throws {
      let label = "Steno test identity \(UUID().uuidString)"
      defer { try? IdentityKeychain.delete(label: label) }

      #expect(try IdentityKeychain.load(label: label) == nil)
      let minted = try MintedIdentity.mint(commonName: "Steno on Test Mac")
      try IdentityKeychain.store(minted, label: label)

      let loaded = try #require(try IdentityKeychain.load(label: label))
      let identity = try HandoverIdentity(secIdentity: loaded)
      #expect(identity.certificateDER == minted.certificateDER)
      #expect(identity.fingerprint == minted.fingerprint)

      // TLS-usable: the private key behind the identity signs, and the
      // certificate's public key verifies it (what the handshake does).
      var privateKey: SecKey?
      #expect(SecIdentityCopyPrivateKey(loaded, &privateKey) == errSecSuccess)
      let signer = try #require(privateKey)
      let message = Data("client hello".utf8)
      var error: Unmanaged<CFError>?
      let signature = try #require(
        SecKeyCreateSignature(signer, .ecdsaSignatureMessageX962SHA256, message as CFData, &error))
      let certificate = try #require(
        SecCertificateCreateWithData(nil, minted.certificateDER as CFData))
      let publicKey = try #require(SecCertificateCopyKey(certificate))
      #expect(
        SecKeyVerifySignature(
          publicKey, .ecdsaSignatureMessageX962SHA256, message as CFData, signature, &error))

      let again = try IdentityKeychain.loadOrCreate(label: label, commonName: "ignored")
      #expect(again.fingerprint == minted.fingerprint, "loadOrCreate returns the stored identity")

      try IdentityKeychain.delete(label: label)
      #expect(try IdentityKeychain.load(label: label) == nil)
    }

    /// The #88 trap: a keychain that holds other identities (MDM ones on a
    /// managed Mac) but none of ours. `kSecClassIdentity` queries ignore the
    /// label there, so `load` must still say nil and `loadOrCreate` must mint.
    /// The foreign identity is the committed test p12, imported into the
    /// default keychain under its own CN and removed at the end.
    @Test(.enabled(if: enabled, "set STENO_KEYCHAIN_TESTS=1 to run the login keychain round trip"))
    func mintsWhenTheKeychainHoldsOnlyForeignIdentities() throws {
      let label = "Steno test identity \(UUID().uuidString)"
      let foreignLabel = TestIdentity.commonName
      try importForeignIdentity()
      defer {
        try? IdentityKeychain.delete(label: label)
        try? IdentityKeychain.delete(label: foreignLabel)
      }

      let foreign = try #require(try IdentityKeychain.load(label: foreignLabel))
      #expect(
        ContentHash.hex(try HandoverIdentity(secIdentity: foreign).fingerprint)
          == TestIdentity.fingerprintHex, "the foreign identity is in the keychain")

      #expect(try IdentityKeychain.load(label: label) == nil, "a foreign identity is not ours")
      let created = try IdentityKeychain.loadOrCreate(label: label, commonName: "Steno on Test Mac")
      #expect(ContentHash.hex(created.fingerprint) != TestIdentity.fingerprintHex)
      let certificate = try Certificate(derEncoded: Array(created.certificateDER))
      #expect(String(describing: certificate.subject) == "CN=Steno on Test Mac")

      let again = try IdentityKeychain.loadOrCreate(label: label, commonName: "ignored")
      #expect(again.fingerprint == created.fingerprint, "the minted identity is the stored one")

      try IdentityKeychain.delete(label: foreignLabel)
      #expect(try IdentityKeychain.load(label: foreignLabel) == nil)
      #expect(try IdentityKeychain.load(label: label) != nil, "deleting the foreign one keeps ours")
    }

    /// `SecPKCS12Import` of the fixture into the default file keychain; an
    /// identity already there (an earlier aborted run) is fine.
    private func importForeignIdentity() throws {
      var keychain: SecKeychain?
      // Deprecated file keychain API, the one the product uses too (#88).
      let copy = SecKeychainCopyDefault(&keychain)
      guard copy == errSecSuccess, let keychain else {
        throw IdentityError.security("SecKeychainCopyDefault", copy)
      }
      var items: CFArray?
      let options: [CFString: Any] = [
        kSecImportExportPassphrase: TestIdentity.password,
        kSecImportExportKeychain: keychain,
      ]
      let status = SecPKCS12Import(
        try Data(contentsOf: TestIdentity.p12URL) as CFData, options as CFDictionary, &items)
      guard status == errSecSuccess || status == errSecDuplicateItem else {
        throw IdentityError.security("SecPKCS12Import", status)
      }
    }
  }
#endif
