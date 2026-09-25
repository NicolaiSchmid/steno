import Foundation
import StenoCore
import Testing

@testable import StenoAdapters

@Suite struct ManagedBlockTests {
  let renderer = ArtifactRenderer()
  let export = FixtureMeeting.export()
  let line = ArtifactRenderer().renderPersonLine(
    FixtureMeeting.export(), folderSlug: FixtureMeeting.folderSlug, options: FixtureMeeting.wikilink
  )

  @Test func personLineAndNewPage() throws {
    #expect(
      line
        == "- 2026-09-24 [[\(FixtureMeeting.folderSlug)|Produktstrategie: \"90/10\" & Roadmap für Q4]] %%steno:00000000-0000-0000-0000-000000000001%%"
    )
    let page = renderer.renderPersonPage(
      FixtureMeeting.persons()[0], export: export, options: FixtureMeeting.wikilink)
    #expect(
      page == """
        ---
        steno_person_id: "00000000-0000-0000-0000-00000000000a"
        email: "anna@example.com"
        type: "person"
        ---

        # Anna Müller

        <!-- steno:meetings:start -->
        \(line)
        <!-- steno:meetings:end -->

        """)
    try Snapshot.assert(page, matches: "snapshots/obsidian/person-page-new.md")
  }

  @Test func appendsTheBlockWhenMarkersAreMissing() {
    let user = "# Anna\n\nSome notes."
    let merged = ManagedBlock.merge(line, meetingID: export.meeting.id, into: user)
    #expect(
      merged
        == "# Anna\n\nSome notes.\n\n<!-- steno:meetings:start -->\n\(line)\n<!-- steno:meetings:end -->\n"
    )
    #expect(
      ManagedBlock.merge(line, meetingID: export.meeting.id, into: "")
        == ManagedBlock.block(lines: [line]))
  }

  @Test func replacesTheLineForTheSameMeetingAndKeepsBytesOutside() throws {
    let older =
      "- 2026-08-01 [[2026-08-01-kickoff|Kickoff]] %%steno:00000000-0000-0000-0000-000000000009%%"
    let newer =
      "- 2026-10-02 [[2026-10-02-retro|Retro]] %%steno:00000000-0000-0000-0000-000000000008%%"
    let stale = "- 2026-09-24 [[old-slug|Old title]] %%steno:00000000-0000-0000-0000-000000000001%%"
    let page = """
      ---
      steno_person_id: "00000000-0000-0000-0000-00000000000a"
      type: "person"
      ---
      # Anna Müller

      Her notes above the block.
      <!-- steno:meetings:start -->
      \(older)
      \(stale)
      <!-- steno:meetings:end -->
      Her notes below the block, with %%steno:00000000-0000-0000-0000-000000000001%% mentioned.

      """
    let merged = ManagedBlock.merge(line, meetingID: export.meeting.id, into: page)
    #expect(
      merged == """
        ---
        steno_person_id: "00000000-0000-0000-0000-00000000000a"
        type: "person"
        ---
        # Anna Müller

        Her notes above the block.
        <!-- steno:meetings:start -->
        \(line)
        \(older)
        <!-- steno:meetings:end -->
        Her notes below the block, with %%steno:00000000-0000-0000-0000-000000000001%% mentioned.

        """)
    let again = ManagedBlock.merge(newer, meetingID: SampleData.uuid(8), into: merged)
    #expect(again.contains("start -->\n\(newer)\n\(line)\n\(older)\n<!--"), "newest first")
    try Snapshot.assert(again, matches: "snapshots/obsidian/person-page-merged.md")
    #expect(
      ManagedBlock.merge(newer, meetingID: SampleData.uuid(8), into: again) == again, "idempotent")
  }

  @Test func sortsByDateThenText() {
    let lines = ["- 2026-01-01 b", "- 2026-03-01 a", "- 2026-01-01 a", "no date at all"]
    #expect(
      ManagedBlock.sortedNewestFirst(lines) == [
        "- 2026-03-01 a", "- 2026-01-01 a", "- 2026-01-01 b", "no date at all",
      ])
  }

  @Test func aHostileTitleCannotBreakOutOfThePersonLine() {
    var export = self.export
    export.meeting.title = "Sync | Q4 ]] %%steno:evil%% [x]\nline two"
    let line = renderer.renderPersonLine(
      export, folderSlug: FixtureMeeting.folderSlug, options: FixtureMeeting.wikilink)
    let alias = "Sync / Q4 )\u{200B}) %%steno:evil%% (x) line two"
    let slug = FixtureMeeting.folderSlug
    let marker = "%%steno:00000000-0000-0000-0000-000000000001%%"
    #expect(line == "- 2026-09-24 [[\(slug)|\(alias)]] \(marker)")
    #expect(line.components(separatedBy: "]]").count == 2, "exactly one link close")
    #expect(!alias.contains("|"))
    // The marker at the end still identifies the meeting on merge.
    let merged = ManagedBlock.merge(line, meetingID: export.meeting.id, into: "")
    let replaced = ManagedBlock.merge(
      "- 2026-09-24 new", meetingID: export.meeting.id, into: merged)
    #expect(replaced == ManagedBlock.block(lines: ["- 2026-09-24 new"]))

    let plain = renderer.renderPersonLine(
      export, folderSlug: FixtureMeeting.folderSlug, options: .plain)
    #expect(
      plain == "- 2026-09-24 [\(alias)](</Meetings/\(slug)/\(slug).md>) \(marker)",
      "plain style uses a vault-absolute Markdown link")
  }
}
