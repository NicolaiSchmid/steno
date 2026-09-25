import Foundation

/// How names are linked: `.none` writes plain names (a WebDAV or Drive
/// destination), `.wikilink` writes `[[Name]]` for a vault.
public enum LinkStyle: Sendable, Equatable {
  case none
  case wikilink
}

/// What a destination tells the renderers. Nothing here changes per meeting
/// and nothing renders the current time, so equal options and an equal
/// export always produce equal bytes.
public struct RenderOptions: Sendable, Equatable {
  public var linkStyle: LinkStyle
  /// Whether per-person pages are rendered (the destination places them in
  /// its people folder). With `.wikilink` this also links every person's
  /// name to that page; a link to a page that does not exist is never
  /// written.
  public var personPages: Bool
  /// Tag appended to every task line, for vaults with a Tasks global filter.
  public var taskTag: String?
  /// Time zone of dates in frontmatter, the info line and person lines.
  public var timeZone: TimeZone

  public init(
    linkStyle: LinkStyle = .none,
    personPages: Bool = false,
    taskTag: String? = nil,
    timeZone: TimeZone = .gmt
  ) {
    self.linkStyle = linkStyle
    self.personPages = personPages
    self.taskTag = taskTag
    self.timeZone = timeZone
  }

  /// Plain names, no people, no tag, UTC.
  public static let plain = RenderOptions()

  /// A person's name becomes `[[Name]]` only when there is a page to land on
  /// and links are wikilinks.
  var linksPeople: Bool { linkStyle == .wikilink && personPages }
}
