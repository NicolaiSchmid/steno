import Foundation

/// Names derived from titles and display names that are safe as folder and
/// file names in a vault. Both functions are pure and machine-independent:
/// no locale enters, so `de_DE` and `en_US` processes produce the same bytes.
public enum Slug {
  /// `"Produktstrategie: \"90/10\" & Roadmap für Q4"` becomes
  /// `"produktstrategie-90-10-roadmap-fuer-q4"`: lowercased, `ä ö ü ß`
  /// transliterated to `ae oe ue ss`, other diacritics stripped, every run
  /// outside `[a-z0-9]` collapsed to one hyphen, hyphens trimmed, cut to
  /// `maxLength` at the last hyphen, `"meeting"` when nothing is left.
  public static func title(_ string: String, maxLength: Int = 60) -> String {
    var result = ""
    var pendingHyphen = false
    for scalar in stripDiacritics(transliterateGerman(string.lowercased())).unicodeScalars {
      if isSlugCharacter(scalar) {
        if pendingHyphen, !result.isEmpty { result.append("-") }
        pendingHyphen = false
        result.unicodeScalars.append(scalar)
      } else {
        pendingHyphen = true
      }
    }
    let cut = truncate(result, to: maxLength)
    return cut.isEmpty ? "meeting" : cut
  }

  /// A display name as a note file name: strips `/ \ : * ? " < > | # ^ [ ]`
  /// and control characters, collapses whitespace, trims spaces and dots.
  /// `"Unnamed"` when nothing is left.
  public static func fileName(_ string: String) -> String {
    var result = ""
    var pendingSpace = false
    for scalar in string.unicodeScalars {
      if forbiddenInFileName.contains(scalar) || scalar.value < 0x20 || scalar.value == 0x7F {
        continue
      }
      if scalar.properties.isWhitespace {
        pendingSpace = true
        continue
      }
      if pendingSpace, !result.isEmpty { result.append(" ") }
      pendingSpace = false
      result.unicodeScalars.append(scalar)
    }
    let trimmed = result.trimmingCharacters(in: CharacterSet(charactersIn: " ."))
    return trimmed.isEmpty ? "Unnamed" : trimmed
  }

  // MARK: - Pieces

  private static let forbiddenInFileName = Set("/\\:*?\"<>|#^[]".unicodeScalars)

  private static func isSlugCharacter(_ scalar: Unicode.Scalar) -> Bool {
    (scalar.value >= 0x61 && scalar.value <= 0x7A) || (scalar.value >= 0x30 && scalar.value <= 0x39)
  }

  /// `ä ö ü ß` to `ae oe ue ss`, matched on characters so decomposed forms
  /// (`a` + U+0308) fold the same way as precomposed ones.
  static func transliterateGerman(_ string: String) -> String {
    var result = ""
    result.reserveCapacity(string.utf8.count)
    for character in string {
      switch character {
      case "ä": result += "ae"
      case "ö": result += "oe"
      case "ü": result += "ue"
      case "ß": result += "ss"
      default: result.append(character)
      }
    }
    return result
  }

  /// Canonical decomposition, then every combining mark dropped: `é` to `e`,
  /// `ñ` to `n`. Unicode data only, no locale.
  static func stripDiacritics(_ string: String) -> String {
    var scalars = String.UnicodeScalarView()
    for scalar in string.decomposedStringWithCanonicalMapping.unicodeScalars {
      switch scalar.properties.generalCategory {
      case .nonspacingMark, .spacingMark, .enclosingMark: continue
      default: scalars.append(scalar)
      }
    }
    return String(scalars)
  }

  /// Cuts at the last hyphen inside the first `maxLength` characters unless
  /// the cut already falls on a word boundary; trims hyphens either way.
  static func truncate(_ slug: String, to maxLength: Int) -> String {
    guard slug.count > maxLength else { return slug }
    let limit = slug.index(slug.startIndex, offsetBy: maxLength)
    var head = slug[..<limit]
    if slug[limit] != "-", let lastHyphen = head.lastIndex(of: "-"), lastHyphen != head.startIndex {
      head = head[..<lastHyphen]
    }
    while head.last == "-" { head.removeLast() }
    return String(head)
  }
}
