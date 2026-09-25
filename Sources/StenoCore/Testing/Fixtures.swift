import Foundation

/// Locates files under `Tests/Fixtures/` from any test target. Folder names
/// are lowercase because APFS is case-insensitive by default.
public enum Fixtures {
  /// The repository's `Tests/Fixtures/` directory, derived from this file's
  /// location.
  public static var root: URL {
    URL(fileURLWithPath: #filePath)
      .deletingLastPathComponent()  // Testing
      .deletingLastPathComponent()  // StenoCore
      .deletingLastPathComponent()  // Sources
      .deletingLastPathComponent()  // repository root
      .appendingPathComponent("Tests", isDirectory: true)
      .appendingPathComponent("Fixtures", isDirectory: true)
  }

  /// `Fixtures.url("audio/sweep-3s.wav")`.
  public static func url(_ relative: String) -> URL {
    root.appendingPathComponent(relative)
  }

  public static func data(_ relative: String) throws -> Data {
    try Data(contentsOf: url(relative))
  }

  /// A fresh, empty temporary directory for one test. Callers remove it.
  public static func temporaryDirectory(_ label: String = "steno-test") throws -> URL {
    let url = FileManager.default.temporaryDirectory
      .appendingPathComponent("\(label)-\(UUID().uuidString)", isDirectory: true)
    try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
    return url
  }
}
