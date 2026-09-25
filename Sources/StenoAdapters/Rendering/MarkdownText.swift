import Foundation

/// Escaping and link helpers shared by the Markdown renderers.
enum MarkdownText {
  /// `[[Anna Müller]]` or `[[2026-09-24-slug|Title]]`. The target goes
  /// through `Slug.fileName` (a link names a note file) and the alias loses
  /// `|`, `]]` and line breaks so it cannot break out of the link.
  static func wikilink(_ target: String, alias: String? = nil) -> String {
    let name = Slug.fileName(target)
    guard let alias, !alias.isEmpty else { return "[[\(name)]]" }
    return "[[\(name)|\(linkAlias(alias))]]"
  }

  /// `[Transcript](<2026-09-24-slug - Transcript.md>)`: the CommonMark form
  /// with an angle-bracketed destination so spaces need no encoding.
  static func markdownLink(_ text: String, file: String) -> String {
    let destination = file.replacingOccurrences(of: ">", with: "%3E")
      .replacingOccurrences(of: "<", with: "%3C")
    return "[\(linkAlias(text))](<\(destination)>)"
  }

  static func linkAlias(_ text: String) -> String {
    singleLine(text)
      .replacingOccurrences(of: "]]", with: "]\u{200B}]")
      .replacingOccurrences(of: "|", with: "/")
      .replacingOccurrences(of: "[", with: "(")
      .replacingOccurrences(of: "]", with: ")")
  }

  /// A paragraph that would otherwise start a heading, list item, block
  /// quote or ordered list (`#`, `-`, `>`, `1.`) gets a backslash.
  static func escapeParagraphStart(_ paragraph: String) -> String {
    guard let first = paragraph.first else { return paragraph }
    if first == "#" || first == "-" || first == ">" { return "\\" + paragraph }
    let digits = paragraph.prefix { $0.isASCII && $0.isNumber }
    if !digits.isEmpty, paragraph.dropFirst(digits.count).first == "." {
      return "\\" + paragraph
    }
    return paragraph
  }

  /// Line breaks and runs of whitespace become one space; trimmed.
  static func singleLine(_ text: String) -> String {
    var result = ""
    var pendingSpace = false
    for scalar in text.unicodeScalars {
      if scalar.properties.isWhitespace || scalar == "\r" || scalar == "\n" {
        pendingSpace = true
        continue
      }
      if pendingSpace, !result.isEmpty { result.append(" ") }
      pendingSpace = false
      result.unicodeScalars.append(scalar)
    }
    return result
  }

  /// An Obsidian tag without `#`: whitespace to `-`, only `[A-Za-z0-9_/-]`
  /// kept, separators trimmed. Nil when nothing is left.
  static func tag(_ raw: String) -> String? {
    var result = ""
    var pendingHyphen = false
    for scalar in raw.unicodeScalars {
      if scalar.properties.isWhitespace {
        pendingHyphen = true
        continue
      }
      guard isTagCharacter(scalar) else { continue }
      if pendingHyphen, !result.isEmpty { result.append("-") }
      pendingHyphen = false
      result.unicodeScalars.append(scalar)
    }
    let trimmed = result.trimmingCharacters(in: CharacterSet(charactersIn: "-/_"))
    return trimmed.isEmpty ? nil : trimmed
  }

  private static func isTagCharacter(_ scalar: Unicode.Scalar) -> Bool {
    switch scalar.value {
    case 0x30...0x39, 0x41...0x5A, 0x61...0x7A: true
    default: scalar == "_" || scalar == "/" || scalar == "-"
    }
  }
}
