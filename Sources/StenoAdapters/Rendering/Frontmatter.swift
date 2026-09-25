import Foundation

/// A YAML frontmatter block built from typed values, never from string
/// interpolation. Every string is double-quoted with `\\`, `\"` and control
/// characters escaped, so titles with colons, quotes, `#` or a leading `-`
/// are safe and a tag of `2026`, `true` or `null` stays a string; dates,
/// numbers and booleans are the plain scalars Obsidian types as Date, Date &
/// time, Number and Checkbox. The emitter knows no consumer: tag grammar
/// belongs to the renderer that builds the list.
public struct Frontmatter: Sendable {
  public enum Value: Sendable {
    case string(String)
    case int(Int)
    case bool(Bool)
    /// `2026-09-24`, in the frontmatter's time zone.
    case date(Date)
    /// `2026-09-24T14:00:00`, in the frontmatter's time zone, no offset.
    case dateTime(Date)
    /// A list of quoted strings.
    case list([String])
  }

  /// Insertion order is output order.
  public var fields: [(key: String, value: Value)]
  public var timeZone: TimeZone

  public init(fields: [(key: String, value: Value)] = [], timeZone: TimeZone = .gmt) {
    self.fields = fields
    self.timeZone = timeZone
  }

  public mutating func append(_ key: String, _ value: Value) {
    fields.append((key, value))
  }

  /// `"---\n…\n---\n"`.
  public func encoded() -> String {
    var lines = ["---"]
    for (key, value) in fields {
      switch value {
      case .string(let string): lines.append("\(key): \(Self.quoted(string))")
      case .int(let int): lines.append("\(key): \(int)")
      case .bool(let bool): lines.append("\(key): \(bool)")
      case .date(let date): lines.append("\(key): \(DateText.day(date, in: timeZone))")
      case .dateTime(let date): lines.append("\(key): \(DateText.dateTime(date, in: timeZone))")
      case .list(let items): lines += Self.list(key, items.map(Self.quoted))
      }
    }
    lines.append("---")
    return lines.joined(separator: "\n") + "\n"
  }

  /// A YAML double-quoted scalar: `\` and `"` escaped; C0 and C1 controls,
  /// DEL, the line and paragraph separators and the byte order mark as
  /// `\n`, `\t`, `\r` or `\uXXXX` (YAML forbids them unescaped); everything
  /// else, including non-ASCII, verbatim.
  static func quoted(_ string: String) -> String {
    var result = "\""
    for scalar in string.unicodeScalars {
      switch scalar {
      case "\\": result += "\\\\"
      case "\"": result += "\\\""
      case "\n": result += "\\n"
      case "\t": result += "\\t"
      case "\r": result += "\\r"
      default:
        if needsEscape(scalar) {
          result += "\\u" + Timecode.pad(Int(scalar.value), width: 4, radix: 16)
        } else {
          result.unicodeScalars.append(scalar)
        }
      }
    }
    return result + "\""
  }

  private static func needsEscape(_ scalar: Unicode.Scalar) -> Bool {
    switch scalar.value {
    case 0x00..<0x20, 0x7F...0x9F, 0x2028, 0x2029, 0xFEFF: true
    default: false
    }
  }

  /// `key: []`, or `key:` followed by one `  - item` line each.
  private static func list(_ key: String, _ items: [String]) -> [String] {
    items.isEmpty ? ["\(key): []"] : ["\(key):"] + items.map { "  - \($0)" }
  }
}
