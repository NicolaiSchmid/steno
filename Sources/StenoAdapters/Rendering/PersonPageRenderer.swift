import Foundation
import StenoCore

/// Person pages: created whole when missing, otherwise only the managed
/// block between `ManagedBlock.start` and `ManagedBlock.end` is Steno's.
struct PersonPageRenderer {
  let export: MeetingExport
  let options: RenderOptions
  let folderSlug: String

  /// The page as created from scratch.
  func page(for person: Person) -> String {
    var frontmatter = Frontmatter(timeZone: options.timeZone)
    frontmatter.append("steno_person_id", .string(person.id.uuidString.lowercased()))
    if let email = person.email, !email.isEmpty {
      frontmatter.append("email", .string(email))
    }
    frontmatter.append("type", .string("person"))
    return [
      frontmatter.encoded(),
      "# \(MarkdownText.singleLine(person.displayName))\n",
      ManagedBlock.block(lines: [line()]),
    ]
    .joined(separator: "\n")
  }

  /// `- 2026-09-24 [[2026-09-24-slug|Title]] %%steno:<meeting uuid>%%`.
  func line() -> String {
    let meeting = export.meeting
    let link =
      switch options.linkStyle {
      case .wikilink: MarkdownText.wikilink(folderSlug, alias: meeting.title)
      case .none:
        MarkdownText.markdownLink(
          meeting.title, file: "/\(MeetingFolder.root)/\(folderSlug)/\(folderSlug).md")
      }
    return
      "- \(DateText.day(meeting.startedAt, in: options.timeZone)) \(link) \(ManagedBlock.marker(meeting.id))"
  }
}
