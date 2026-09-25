import Foundation

/// Where the database and support files live:
/// `~/Library/Application Support/Steno/`. Audio lives in
/// `Settings.audioFolder`.
public struct StenoPaths: Sendable, Equatable {
  public var supportDirectory: URL
  public var databaseURL: URL

  public init(supportDirectory: URL) {
    self.supportDirectory = supportDirectory
    self.databaseURL = supportDirectory.appendingPathComponent("steno.sqlite", isDirectory: false)
  }

  /// Follows `HOME`, so CLI tests with a temporary home never touch the real
  /// one. Read from the environment first: not every Foundation honours the
  /// variable in `homeDirectoryForCurrentUser`.
  public static var defaultSupportDirectory: URL {
    homeDirectory
      .appendingPathComponent("Library", isDirectory: true)
      .appendingPathComponent("Application Support", isDirectory: true)
      .appendingPathComponent("Steno", isDirectory: true)
  }

  /// `$HOME` when set and absolute, else Foundation's answer.
  public static var homeDirectory: URL {
    if let home = ProcessInfo.processInfo.environment["HOME"], home.hasPrefix("/") {
      return URL(fileURLWithPath: home, isDirectory: true)
    }
    return FileManager.default.homeDirectoryForCurrentUser
  }

  /// The default paths with the support directory created.
  public static func `default`() throws -> StenoPaths {
    let paths = StenoPaths(supportDirectory: defaultSupportDirectory)
    try FileManager.default.createDirectory(
      at: paths.supportDirectory, withIntermediateDirectories: true)
    return paths
  }
}
