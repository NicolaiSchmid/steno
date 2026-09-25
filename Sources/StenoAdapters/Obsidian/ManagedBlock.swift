import Foundation

/// The region of a person page that Steno owns: one line per meeting between
/// two HTML comments, newest first, each line ending in a `%%steno:<uuid>%%`
/// comment that identifies its meeting. Bytes outside the markers are
/// copied unchanged; missing markers are appended. Internal until a second
/// destination touches user-owned files.
enum ManagedBlock {
  static let start = "<!-- steno:meetings:start -->"
  static let end = "<!-- steno:meetings:end -->"

  /// `%%steno:0d6f…%%`, Obsidian's comment syntax so the id never shows.
  static func marker(_ meetingID: UUID) -> String {
    "%%steno:\(meetingID.uuidString.lowercased())%%"
  }

  /// A whole block around `lines`, terminated by a newline.
  static func block(lines: [String]) -> String {
    ([start] + lines + [end]).joined(separator: "\n") + "\n"
  }

  /// `existing` with `line` replacing the line that carries this meeting's
  /// marker (or inserted when there is none), the block re-sorted newest
  /// first; the block appended when the markers are missing.
  static func merge(_ line: String, meetingID: UUID, into existing: String) -> String {
    guard let startRange = existing.range(of: start),
      let endRange = existing.range(of: end, range: startRange.upperBound..<existing.endIndex)
    else {
      var result = existing
      if !result.isEmpty, !result.hasSuffix("\n") { result += "\n" }
      if !result.isEmpty { result += "\n" }
      return result + block(lines: [line])
    }
    let marker = self.marker(meetingID)
    var lines = existing[startRange.upperBound..<endRange.lowerBound]
      .split(separator: "\n", omittingEmptySubsequences: true)
      .map(String.init)
      .filter { !$0.trimmingCharacters(in: .whitespaces).isEmpty && !$0.contains(marker) }
    lines.append(line)
    return existing[..<startRange.upperBound] + "\n"
      + sortedNewestFirst(lines).joined(separator: "\n")
      + "\n" + existing[endRange.lowerBound...]
  }

  /// By the leading `- YYYY-MM-DD` descending, then by text so equal dates
  /// keep a stable order; lines without a date sort last.
  static func sortedNewestFirst(_ lines: [String]) -> [String] {
    lines.sorted { lhs, rhs in
      let (l, r) = (date(of: lhs), date(of: rhs))
      if l != r { return l > r }
      return lhs < rhs
    }
  }

  /// `"2026-09-24"` from `- 2026-09-24 …`, or `""` when the line has none.
  private static func date(of line: String) -> String {
    let candidate = String(line.drop { $0 == "-" || $0 == " " || $0 == "*" }.prefix(10))
    let scalars = Array(candidate.unicodeScalars)
    guard scalars.count == 10 else { return "" }
    for (index, scalar) in scalars.enumerated() {
      let isDash = index == 4 || index == 7
      if isDash ? scalar != "-" : !(scalar.value >= 0x30 && scalar.value <= 0x39) { return "" }
    }
    return candidate
  }
}
