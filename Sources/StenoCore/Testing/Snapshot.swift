import Foundation

/// Golden-file comparison without a test framework dependency, so the CLI and
/// every test target share it. A mismatch throws `Snapshot.Mismatch` with a
/// unified diff; `STENO_UPDATE_SNAPSHOTS=1` rewrites the golden instead and
/// the diff in the PR is the reviewed output change. On a mismatch the actual
/// bytes are also written next to the golden as `<name>.actual` so a run on
/// another machine can be inspected.
public enum Snapshot {
  public static let updateEnvironmentKey = "STENO_UPDATE_SNAPSHOTS"

  public struct Mismatch: Error, CustomStringConvertible {
    public var path: String
    public var diff: String
    public var description: String { "snapshot mismatch at \(path)\n\(diff)" }
  }

  public struct Missing: Error, CustomStringConvertible {
    public var path: String
    public var description: String {
      "snapshot missing at \(path); run with \(updateEnvironmentKey)=1 to create it"
    }
  }

  /// Compares `data` with `Tests/Fixtures/<relative>` byte for byte.
  public static func assert(
    _ data: Data, matches relative: String,
    environment: [String: String] = ProcessInfo.processInfo.environment
  ) throws {
    let url = Fixtures.url(relative)
    if environment[updateEnvironmentKey] == "1" {
      try FileManager.default.createDirectory(
        at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
      try data.write(to: url, options: .atomic)
      return
    }
    guard let expected = try? Data(contentsOf: url) else {
      throw Missing(path: url.path)
    }
    if expected == data { return }
    let actualURL = url.appendingPathExtension("actual")
    try? data.write(to: actualURL, options: .atomic)
    throw Mismatch(
      path: url.path,
      diff: unifiedDiff(
        expected: String(decoding: expected, as: UTF8.self),
        actual: String(decoding: data, as: UTF8.self)))
  }

  public static func assert(_ string: String, matches relative: String) throws {
    try assert(Data(string.utf8), matches: relative)
  }

  /// A minimal unified diff (no context folding) built from a longest common
  /// subsequence over lines.
  public static func unifiedDiff(expected: String, actual: String) -> String {
    let old = expected.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
    let new = actual.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
    let m = old.count
    let n = new.count
    var table = [[Int]](repeating: [Int](repeating: 0, count: n + 1), count: m + 1)
    for i in stride(from: m - 1, through: 0, by: -1) {
      for j in stride(from: n - 1, through: 0, by: -1) {
        table[i][j] =
          old[i] == new[j] ? table[i + 1][j + 1] + 1 : max(table[i + 1][j], table[i][j + 1])
      }
    }
    var lines = ["--- expected", "+++ actual"]
    var i = 0
    var j = 0
    while i < m && j < n {
      if old[i] == new[j] {
        lines.append(" \(old[i])")
        i += 1
        j += 1
      } else if table[i + 1][j] >= table[i][j + 1] {
        lines.append("-\(old[i])")
        i += 1
      } else {
        lines.append("+\(new[j])")
        j += 1
      }
    }
    while i < m {
      lines.append("-\(old[i])")
      i += 1
    }
    while j < n {
      lines.append("+\(new[j])")
      j += 1
    }
    return lines.joined(separator: "\n")
  }
}
