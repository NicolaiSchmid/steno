import Foundation
import StenoCore
import Testing

@testable import StenoAdapters

@Suite struct FolderNoteRendererTests {
  let renderer = ArtifactRenderer()
  let export = FixtureMeeting.export()

  @Test func matchesTheGoldensInEveryVariant() throws {
    try Snapshot.assert(
      renderer.renderFolderNote(export, options: FixtureMeeting.plain),
      matches: "snapshots/obsidian/folder-note-plain-utc.md")
    try Snapshot.assert(
      renderer.renderFolderNote(export, options: FixtureMeeting.plainBerlin),
      matches: "snapshots/obsidian/folder-note-plain-berlin.md")
    try Snapshot.assert(
      renderer.renderFolderNote(export, options: FixtureMeeting.wikilink),
      matches: "snapshots/obsidian/folder-note-wikilink-berlin.md")
    try Snapshot.assert(
      renderer.renderFolderNote(export, options: FixtureMeeting.wikilinkUTC),
      matches: "snapshots/obsidian/folder-note-wikilink-utc.md")
  }

  @Test func frontmatterCarriesTheScopeFieldsPlusTitle() {
    let note = renderer.renderFolderNote(export, options: FixtureMeeting.wikilink)
    #expect(note.hasPrefix("---\ntitle: \"Produktstrategie: \\\"90/10\\\" & Roadmap für Q4\"\n"))
    #expect(note.contains("\ndate: 2026-09-24T14:00:00\n"))
    #expect(note.contains("\nduration: 90\n"))
    #expect(
      note.contains(
        "\nparticipants:\n  - \"[[Anna Müller]]\"\n  - \"Jérôme Dupont\"\n  - \"[[Nicolai Schmid]]\"\n  - \"Speaker 2\"\n"
      ), "persons are linked; an attendee without a person and the unresolved speaker are not")
    #expect(note.contains("\ntags:\n  - meeting\n  - Kunde-ACME\n  - q4\n"))
    #expect(note.contains("\nsource: \"mac-call\"\n"))
    #expect(note.contains("\ntemplate: \"default\"\n"))
    #expect(note.contains("\nlanguage: \"de\"\n"))
    #expect(note.contains("\nsteno_id: \"00000000-0000-0000-0000-000000000001\"\n---\n"))
  }

  @Test func bodyHasTitleInfoLineSummaryDecisionsAndScratchpad() {
    let note = renderer.renderFolderNote(export, options: FixtureMeeting.wikilink)
    #expect(note.contains("\n# Produktstrategie: \"90/10\" & Roadmap für Q4\n"))
    #expect(
      note.contains(
        "\n2026-09-24 14:00–15:30 · 1 h 30 min · Mac call · [[\(FixtureMeeting.folderSlug) - Transcript|Transcript]] · [[\(FixtureMeeting.folderSlug) - Tasks|Tasks]]\n"
      ))
    let summary = SummaryMarkdown.render(export)
    #expect(note.contains("\n## Summary\n\n" + FolderNoteRenderer.demoted(summary)))
    #expect(
      note.contains("\n### Executive Summary\n\n- **Fokus**: "),
      "section headings sit under Summary")
    #expect(!note.contains("\n## Executive Summary\n"))
    #expect(
      summary.contains("**Anna Müller** setzt 90 Prozent"), "a confirmed speaker is named in bold")
    #expect(summary.contains("Speaker 2 verteilt"), "an unresolved speaker keeps its label")
    #expect(
      note.contains(
        "\n## Decisions\n\n- 90/10-Aufteilung wird umgesetzt.\n- Roadmap-Review am 15. Oktober.\n"))
    #expect(
      note.hasSuffix(
        "\n## Scratchpad\n\nNachfassen wegen Budget.\n\n---\n\n## Summary\n\nNicht vergessen: Jérôme fragen.\n"
      ), "the scratchpad is verbatim")
  }

  @Test func plainStyleLinksNothingAndUsesMarkdownLinksForTheNotes() {
    let note = renderer.renderFolderNote(export, options: FixtureMeeting.plain)
    #expect(!note.contains("[["))
    #expect(note.contains("\n  - \"Anna Müller\"\n"))
    #expect(
      note.contains(
        "\n2026-09-24 12:00–13:30 · 1 h 30 min · Mac call · [Transcript](<\(FixtureMeeting.folderSlug) - Transcript.md>) · [Tasks](<\(FixtureMeeting.folderSlug) - Tasks.md>)\n"
      ))
    #expect(note.contains("\ndate: 2026-09-24T12:00:00\n"))
  }

  @Test func emptySectionsAreOmittedAndTheFolderSlugIsPinned() {
    var export = self.export
    export.decisions = []
    export.meeting.scratchpad = "  \n"
    export.meeting.summary = nil
    export.meeting.language = nil
    export.meeting.tags = ["  ", "###"]
    export.meeting.duration = 45 * 60 + 1
    let note = renderer.renderFolderNote(
      export, options: FixtureMeeting.wikilink, folderSlug: "2026-09-24-pinned")
    #expect(!note.contains("## Decisions"))
    #expect(!note.contains("## Scratchpad"))
    #expect(!note.contains("language:"))
    #expect(note.contains("\n## Summary\n\nNo summary.\n"))
    #expect(note.contains("\ntags:\n  - meeting\nsource:"))
    #expect(note.contains("\nduration: 46\n"), "whole minutes, rounded up")
    #expect(note.contains(" · 46 min · "))
    #expect(note.contains("[[2026-09-24-pinned - Transcript|Transcript]]"))
    #expect(FolderNoteRenderer.durationText(7200) == "2 h")
    #expect(FolderNoteRenderer.durationText(0) == "0 min")
  }

  @Test func renderReturnsTheFiveMeetingFilesPlusPersonPages() throws {
    let artifacts = try renderer.render(export, options: FixtureMeeting.wikilink)
    #expect(
      artifacts.map(\.fileName) == [
        "\(FixtureMeeting.folderSlug).md", "\(FixtureMeeting.folderSlug) - Transcript.md",
        "\(FixtureMeeting.folderSlug) - Tasks.md", "transcript.vtt", "meeting.json",
        "Anna Müller.md", "Nicolai Schmid.md",
      ])
    #expect(
      artifacts.map(\.kind) == [
        .folderNote, .transcript, .tasks, .vtt, .json, .personPage, .personPage,
      ])
    #expect(artifacts[5].personID == FixtureMeeting.annaID)
    let plain = try renderer.render(export, options: FixtureMeeting.plain)
    #expect(plain.count == 5, "no people folder, no person pages")
  }
}
