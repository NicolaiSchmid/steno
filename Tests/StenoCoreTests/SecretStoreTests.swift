import Foundation
import Testing

@testable import StenoCore

@Suite struct SecretStoreTests {
  @Test func writesAFileWithMode0600AndRoundTrips() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let url = directory.appendingPathComponent("nested/secrets.json")
    let store = FileSecretStore(url: url, environment: [:])

    #expect(try await store.secret(for: .llmAPIKey) == nil)
    try await store.setSecret("sk-test", for: .llmAPIKey)
    #expect(try await store.secret(for: .llmAPIKey) == "sk-test")

    let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
    let permissions = try #require(attributes[.posixPermissions] as? Int)
    #expect(permissions & 0o777 == 0o600)

    try await store.setSecret("sk-two", for: SecretKey(rawValue: "other"))
    #expect(try await store.secret(for: .llmAPIKey) == "sk-test")
    #expect(try await store.secret(for: SecretKey(rawValue: "other")) == "sk-two")
    #expect(
      try FileManager.default.attributesOfItem(atPath: url.path)[.posixPermissions] as? Int == 0o600
    )

    try await store.setSecret(nil, for: .llmAPIKey)
    #expect(try await store.secret(for: .llmAPIKey) == nil)
  }

  @Test func environmentOverridesTheFile() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let url = directory.appendingPathComponent("secrets.json")
    #expect(FileSecretStore.environmentVariable(for: .llmAPIKey) == "STENO_LLM_API_KEY")
    let store = FileSecretStore(url: url, environment: ["STENO_LLM_API_KEY": "from-env"])
    try await store.setSecret("from-file", for: .llmAPIKey)
    #expect(try await store.secret(for: .llmAPIKey) == "from-env")
    #expect(
      try await FileSecretStore(url: url, environment: [:]).secret(for: .llmAPIKey) == "from-file")
  }
}
