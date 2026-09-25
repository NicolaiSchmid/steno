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
  /// Folder inside the vault for per-person pages; nil disables person
  /// links and pages.
  public var peopleFolder: String?
  /// Tag appended to every task line, for vaults with a Tasks global filter.
  public var taskTag: String?
  /// Time zone of dates in frontmatter, the info line and person lines.
  public var timeZone: TimeZone

  public init(
    linkStyle: LinkStyle = .none,
    peopleFolder: String? = nil,
    taskTag: String? = nil,
    timeZone: TimeZone = RenderOptions.utc
  ) {
    self.linkStyle = linkStyle
    self.peopleFolder = peopleFolder
    self.taskTag = taskTag
    self.timeZone = timeZone
  }

  /// Plain names, no people, no tag, UTC.
  public static let plain = RenderOptions()

  public static let utc = TimeZone(identifier: "UTC")!

  /// People are linked and get pages only with wikilinks and a people
  /// folder.
  var linksPeople: Bool { linkStyle == .wikilink && peopleFolder != nil }
}
