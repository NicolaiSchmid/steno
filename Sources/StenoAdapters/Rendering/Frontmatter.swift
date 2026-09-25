import Foundation

/// A YAML frontmatter block built from typed values, never from string
/// interpolation. Every string is double-quoted with `\\`, `\"` and control
/// characters escaped, so titles with colons, quotes, `#` or a leading `-`
/// are safe; dates, numbers and booleans are the plain scalars Obsidian types
/// as Date, Date & time, Number and Checkbox; tags are sanitised plain
/// scalars so Obsidian reads them as Tags.
public struct Frontmatter: Sendable, Equatable {
  public enum Value: Sendable, Equatable {
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

  public struct Field: Sendable, Equatable {
    public var key: String
    public var value: Value

    public init(_ key: String, _ value: Value) {
      self.key = key
      self.value = value
    }
  }

  /// Insertion order is output order.
  public var fields: [Field]
  public var timeZone: TimeZone

  public init(fields: [Field] = [], timeZone: TimeZone = RenderOptions.utc) {
    self.fields = fields
    self.timeZone = timeZone
  }

  public mutating func append(_ key: String, _ value: Value) {
    fields.append(Field(key, value))
  }

  /// `"---\n…\n---\n"`.
  public func encoded() -> String {
    var lines = ["---"]
    for field in fields {
      switch field.value {
      case .string(let string):
        lines.append("\(field.key): \(Self.quoted(string))")
      case .int(let int):
        lines.append("\(field.key): \(int)")
      case .bool(let bool):
        lines.append("\(field.key): \(bool ? "true" : "false")")
      case .date(let date):
        lines.append("\(field.key): \(DateText.day(date, in: timeZone))")
      case .dateTime(let date):
        lines.append("\(field.key): \(DateText.dateTime(date, in: timeZone))")
      case .list(let items):
        if items.isEmpty {
          lines.append("\(field.key): []")
        } else {
          lines.append("\(field.key):")
          for item in items { lines.append("  - \(Self.quoted(item))") }
        }
      case .tags(let raw):
        let tags = raw.compactMap(MarkdownText.tag)
        if tags.isEmpty {
          lines.append("\(field.key): []")
        } else {
          lines.append("\(field.key):")
          for tag in tags { lines.append("  - \(tag)") }
        }
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
          result += "\\u" + hex4(scalar.value)
        } else {
          result.unicodeScalars.append(scalar)
        }
      }
    }
    return result + "\""
  }

  private static func hex4(_ value: UInt32) -> String {
    let digits = String(value, radix: 16, uppercase: true)
    return String(repeating: "0", count: max(0, 4 - digits.count)) + digits
  }
}
