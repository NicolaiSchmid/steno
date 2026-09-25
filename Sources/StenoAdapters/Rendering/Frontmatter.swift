import Foundation

/// A YAML frontmatter block built from typed values, never from string
/// interpolation. Every string is double-quoted with `\\`, `\"` and control
/// characters escaped, so titles with colons, quotes, `#` or a leading `-`
/// are safe; dates, numbers and booleans are the plain scalars Obsidian types
/// as Date, Date & time, Number and Checkbox; tags are sanitised plain
/// scalars so Obsidian reads them as Tags.
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
    /// A list of plain tag scalars; entries are passed through
    /// `MarkdownText.tag` and empty results dropped.
    case tags([String])
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
      case .tags(let raw): lines += Self.list(key, raw.compactMap(MarkdownText.tag))
      }
    }
    lines.append("---")
    return lines.joined(separator: "\n") + "\n"
  }

  /// A YAML double-quoted scalar: `\` and `"` escaped, C0 controls and DEL
  /// as `\n`, `\t`, `\r` or `\uXXXX`, everything else (including non-ASCII)
  /// verbatim.
  public static func quoted(_ string: String) -> String {
    var result = "\""
    for scalar in string.unicodeScalars {
      switch scalar {
      case "\\": result += "\\\\"
      case "\"": result += "\\\""
      case "\n": result += "\\n"
      case "\t": result += "\\t"
      case "\r": result += "\\r"
      default:
        if scalar.value < 0x20 || scalar.value == 0x7F {
          result += "\\u" + Timecode.pad(Int(scalar.value), width: 4, radix: 16)
        } else {
          result.unicodeScalars.append(scalar)
        }
      }
    }
    return result + "\""
  }

  /// `key: []`, or `key:` followed by one `  - item` line each.
  private static func list(_ key: String, _ items: [String]) -> [String] {
    items.isEmpty ? ["\(key): []"] : ["\(key):"] + items.map { "  - \($0)" }
  }
}
