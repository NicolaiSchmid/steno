import Foundation
import StenoCore

/// Pure renderers from the canonical model to bytes: folder note, transcript
/// and tasks Markdown, WebVTT, `meeting.json` and person pages. No I/O, no
/// clock; equal inputs give equal bytes on every machine. Destinations pass
/// the pinned folder basename as `folderSlug` on re-export so note names
/// stay put when the title changes.
public struct ArtifactRenderer: Sendable {
  /// Bumped whenever any renderer's bytes change; recorded in every receipt
  /// and pinned by `Tests/Fixtures/snapshots/obsidian/VERSION`.
  public static let version = 2

  public init() {}

  /// Every artefact of one meeting: `meeting.json` first, because a folder
  /// is recognised as this meeting's crashed attempt by that file alone, so
  /// it must be the first one on disk; then the four notes, then one
  /// `.personPage` per person when the options have a people folder. Audio
  /// is copied by the destination, not rendered.
  public func render(_ export: MeetingExport, options: RenderOptions, folderSlug: String? = nil)
    throws -> [RenderedArtifact]
  {
    let slug = folderSlug ?? Self.folderSlug(export, options)
    var artifacts = [
      RenderedArtifact(kind: .json, fileName: ObsidianLayout.json, data: try renderJSON(export)),
      RenderedArtifact(
        kind: .folderNote, fileName: ObsidianLayout.folderNote(slug: slug),
        data: Data(renderFolderNote(export, options: options, folderSlug: slug).utf8)),
      RenderedArtifact(
        kind: .transcript, fileName: ObsidianLayout.transcriptNote(slug: slug),
        data: Data(renderTranscript(export, options: options).utf8)),
      RenderedArtifact(
        kind: .tasks, fileName: ObsidianLayout.tasksNote(slug: slug),
        data: Data(renderTasks(export, options: options).utf8)),
      RenderedArtifact(
        kind: .vtt, fileName: ObsidianLayout.vtt, data: Data(renderVTT(export).utf8)),
    ]
    if options.linksPeople {
      for person in export.persons {
        artifacts.append(
          RenderedArtifact(
            kind: .personPage,
            fileName: ObsidianLayout.personPage(displayName: person.displayName),
            data: Data(
              renderPersonPage(person, export: export, options: options, folderSlug: slug).utf8)))
      }
    }
    return artifacts
  }

  /// Frontmatter, `# Title`, the info line, `## Summary` (core's
  /// `SummaryMarkdown.render` verbatim), `## Decisions` and `## Scratchpad`
  /// when present.
  public func renderFolderNote(
    _ export: MeetingExport, options: RenderOptions, folderSlug: String? = nil
  ) -> String {
    FolderNoteRenderer(
      export: export, options: options, folderSlug: folderSlug ?? Self.folderSlug(export, options)
    ).render()
  }

  /// One `## Name — 00:12:34` header per turn, paragraphs split at gaps of
  /// three seconds or more.
  public func renderTranscript(_ export: MeetingExport, options: RenderOptions) -> String {
    TranscriptMarkdownRenderer(export: export, options: options).render()
  }

  /// Obsidian Tasks lines: `- [ ] text [[Assignee]] #tag ⏫ 📅 YYYY-MM-DD`.
  public func renderTasks(_ export: MeetingExport, options: RenderOptions) -> String {
    TasksMarkdownRenderer(export: export, options: options).render()
  }

  /// WebVTT with one cue per segment and `<v Name>` voice spans.
  public func renderVTT(_ export: MeetingExport) -> String {
    WebVTTRenderer(export: export).render()
  }

  /// `meeting.json`: the `StenoJSON` encoding of the export, byte-identical
  /// to `steno export`.
  public func renderJSON(_ export: MeetingExport) throws -> Data {
    try StenoJSON.encode(export)
  }

  /// A person page as created from scratch: frontmatter, `# Name` and the
  /// managed block holding this meeting's line.
  public func renderPersonPage(
    _ person: Person, export: MeetingExport, options: RenderOptions, folderSlug: String? = nil
  ) -> String {
    PersonPageRenderer(
      export: export, options: options, folderSlug: folderSlug ?? Self.folderSlug(export, options)
    ).page(for: person)
  }

  /// `- 2026-09-24 [[<folder slug>|<title>]] %%steno:<meeting uuid>%%`.
  public func renderPersonLine(_ export: MeetingExport, folderSlug: String, options: RenderOptions)
    -> String
  {
    PersonPageRenderer(export: export, options: options, folderSlug: folderSlug).line()
  }

  static func folderSlug(_ export: MeetingExport, _ options: RenderOptions) -> String {
    MeetingFolder.basename(for: export.meeting, timeZone: options.timeZone)
  }

  /// The lowercase UUID every note carries as `steno_id`.
  static func stenoID(_ export: MeetingExport) -> String {
    export.meeting.id.uuidString.lowercased()
  }

  /// The frontmatter and `# Title — Kind` heading the transcript and tasks
  /// notes open with; `kind` is `"Transcript"` or `"Tasks"`.
  static func noteHead(_ export: MeetingExport, kind: String, options: RenderOptions) -> [String] {
    var frontmatter = Frontmatter(timeZone: options.timeZone)
    frontmatter.append("title", .string("\(export.meeting.title) — \(kind)"))
    frontmatter.append("type", .string(kind.lowercased()))
    frontmatter.append("steno_id", .string(stenoID(export)))
    return [
      frontmatter.encoded(), "# \(MarkdownText.singleLine(export.meeting.title)) — \(kind)\n",
    ]
  }

  /// Segments by start, ties broken by id so the order is total.
  static func orderedSegments(_ export: MeetingExport) -> [TranscriptSegment] {
    export.segments.sorted { ($0.start, $0.id.uuidString) < ($1.start, $1.id.uuidString) }
  }
}
