/// A secret's name. The app stores secrets in the Keychain, the CLI and tests
/// in a 0600 file or the environment.
public struct SecretKey: RawRepresentable, Sendable, Hashable {
  public var rawValue: String

  public init(rawValue: String) {
    self.rawValue = rawValue
  }

  public static let llmAPIKey = SecretKey(rawValue: "llm-api-key")
}

public protocol SecretStore: Sendable {
  func secret(for key: SecretKey) async throws -> String?
  /// nil removes the secret.
  func setSecret(_ value: String?, for key: SecretKey) async throws
}
