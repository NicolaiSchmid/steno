import Foundation
import StenoCore

/// Person pages: created whole when missing, otherwise only the managed
/// block between `ManagedBlock.start` and `ManagedBlock.end` is Steno's.
struct PersonPageRenderer {
  let export: MeetingExport
  let options: RenderOptions
  let folderSlug: String

  /// The page as created from scratch plus this meeting's line. The file is
  /// named after the display name, sanitised the way `MarkdownText.wikilink`
  /// sanitises its target, so `[[Anna Müller]]` resolves to it.
  func page(for person: Person) -> PersonPage {
    let line = self.line()
    var frontmatter = Frontmatter(timeZone: options.timeZone)
    frontmatter.append("steno_person_id", .string(person.id.uuidString.lowercased()))
    if let email = person.email, !email.isEmpty {
      frontmatter.append("email", .string(email))
    }
    frontmatter.append("type", .string("person"))
    let page = [
      frontmatter.encoded(),
      "# \(MarkdownText.singleLine(person.displayName))\n",
      ManagedBlock.block(lines: [line]),
    ]
    .joined(separator: "\n")
    return PersonPage(fileName: "\(Slug.fileName(person.displayName)).md", page: page, line: line)
  }

  /// `- 2026-09-24 [[2026-09-24-slug|Title]] %%steno:<meeting uuid>%%`, or
  /// with a root-absolute Markdown link to the folder note in `.none` style.
  func line() -> String {
    let meeting = export.meeting
    let link =
      switch options.linkStyle {
      case .wikilink: MarkdownText.wikilink(folderSlug, alias: meeting.title)
      case .none:
        MarkdownText.markdownLink(
          meeting.title,
          file:
            "/\(MeetingFolder.root)/\(folderSlug)/\(MeetingFolder.noteFile(.folder, slug: folderSlug))"
        )
      }
    return
      "- \(DateText.day(meeting.startedAt, in: options.timeZone)) \(link) \(ManagedBlock.marker(meeting.id))"
  }
}
