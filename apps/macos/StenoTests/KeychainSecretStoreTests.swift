import StenoCore
import XCTest

/// Round trip through the login keychain on a throwaway service name.
/// Opt-in: the hosted runner's login keychain may be locked.
final class KeychainSecretStoreTests: XCTestCase {
  func testRoundTripOnAThrowawayService() async throws {
    try XCTSkipUnless(
      ProcessInfo.processInfo.environment["STENO_KEYCHAIN_TESTS"] == "1",
      "Set STENO_KEYCHAIN_TESTS=1 to exercise the login keychain")
    let store = KeychainSecretStore(service: "uno.schmid.steno.mac.tests.\(UUID().uuidString)")
    let key = SecretKey.llmAPIKey
    defer { Task { try? await store.setSecret(nil, for: key) } }

    XCTAssertNil(try await store.secret(for: key))
    try await store.setSecret("first", for: key)
    XCTAssertEqual(try await store.secret(for: key), "first")
    try await store.setSecret("second", for: key)
    XCTAssertEqual(try await store.secret(for: key), "second", "update, not duplicate")
    try await store.setSecret(nil, for: key)
    XCTAssertNil(try await store.secret(for: key))
    try await store.setSecret(nil, for: key)
  }

  func testEmptyValueDeletes() async throws {
    try XCTSkipUnless(
      ProcessInfo.processInfo.environment["STENO_KEYCHAIN_TESTS"] == "1",
      "Set STENO_KEYCHAIN_TESTS=1 to exercise the login keychain")
    let store = KeychainSecretStore(service: "uno.schmid.steno.mac.tests.\(UUID().uuidString)")
    try await store.setSecret("x", for: .llmAPIKey)
    try await store.setSecret("", for: .llmAPIKey)
    XCTAssertNil(try await store.secret(for: .llmAPIKey))
  }
}
