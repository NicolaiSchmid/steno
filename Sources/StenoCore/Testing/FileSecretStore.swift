import Foundation

/// A `SecretStore` over one JSON file with mode 0600, for the CLI and tests.
/// An environment variable named `STENO_<KEY>` (`llm-api-key` becomes
/// `STENO_LLM_API_KEY`) overrides the file so CI can inject a key without
/// writing one to disk.
public struct FileSecretStore: SecretStore, Sendable {
  public var url: URL
  public var environment: [String: String]

  public init(url: URL, environment: [String: String] = ProcessInfo.processInfo.environment) {
    self.url = url
    self.environment = environment
  }

  public static func environmentVariable(for key: SecretKey) -> String {
    "STENO_" + key.rawValue.uppercased().replacingOccurrences(of: "-", with: "_")
  }

  public func secret(for key: SecretKey) async throws -> String? {
    if let fromEnvironment = environment[Self.environmentVariable(for: key)],
      !fromEnvironment.isEmpty
    {
      return fromEnvironment
    }
    return try read()[key.rawValue]
  }

  public func setSecret(_ value: String?, for key: SecretKey) async throws {
    var secrets = try read()
    secrets[key.rawValue] = value
    try write(secrets)
  }

  private func read() throws -> [String: String] {
    guard FileManager.default.fileExists(atPath: url.path) else { return [:] }
    let data = try Data(contentsOf: url)
    guard !data.isEmpty else { return [:] }
    return try StenoJSON.decode([String: String].self, from: data)
  }

  private func write(_ secrets: [String: String]) throws {
    let directory = url.deletingLastPathComponent()
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    let data = try StenoJSON.encode(secrets)
    guard
      FileManager.default.createFile(
        atPath: url.path, contents: data, attributes: [.posixPermissions: 0o600])
    else {
      throw CocoaError(.fileWriteUnknown, userInfo: [NSFilePathErrorKey: url.path])
    }
    try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
  }
}
