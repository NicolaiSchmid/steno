import Foundation
import StenoCore
import Testing

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

      let again = try IdentityKeychain.loadOrCreate(label: label, commonName: "ignored")
      #expect(again.fingerprint == minted.fingerprint, "loadOrCreate returns the stored identity")

      try IdentityKeychain.delete(label: label)
      #expect(try IdentityKeychain.load(label: label) == nil)
    }
  }
#endif
