import StenoCore
import XCTest

/// The four tabs rendered as text for StenoCore's fixture meeting, pinned
/// as goldens under `Tests/Fixtures/snapshots/macos/` (rewritten with
/// `STENO_UPDATE_SNAPSHOTS=1`; the diff is the reviewed output change). The
/// UI smoke test clicks the same tabs and looks for one string each; those
/// strings are asserted here too, so the two proofs cannot drift apart.
final class TabTextSnapshotTests: XCTestCase {
  private let locale = Locale(identifier: "en_US_POSIX")
  private let utc = TimeZone(identifier: "UTC")!

  private func lines(_ tab: MeetingDetailViewModel.Tab, _ export: MeetingExport) -> [String] {
    TabText.lines(tab, export: export, locale: locale, timeZone: utc)
  }

  func testEveryTabMatchesItsGolden() throws {
    let export = SampleData.export()
    for tab in MeetingDetailViewModel.Tab.allCases {
      let rendered = lines(tab, export)
      XCTAssertFalse(rendered.isEmpty, tab.rawValue)
      XCTAssertFalse(rendered.contains { $0.isEmpty && tab != .tasks }, "\(tab): blank line")
      try Snapshot.assert(
        Data((rendered.joined(separator: "\n") + "\n").utf8),
        matches: "snapshots/macos/tab-\(tab.rawValue).txt", root: TestSupport.fixtures)
    }
  }

  func testTabsCarryTheTextTheUISmokeTestLooksFor() {
    let export = SampleData.export()
    XCTAssertTrue(lines(.summary, export).contains("Executive Summary"))
    XCTAssertTrue(lines(.transcript, export).contains("Wir setzen neunzig Prozent auf den Kern."))
    XCTAssertTrue(lines(.tasks, export).contains { $0.hasSuffix("Budgetzahlen prüfen") })
    XCTAssertEqual(lines(.scratchpad, export), ["Nachfassen wegen Budget."])
  }

  func testTranscriptNamesSpeakersAndLeavesUnassignedSegmentsUnknown() {
    let export = SampleData.export()
    let rendered = lines(.transcript, export)
    XCTAssertEqual(rendered.first, "Nicolai 00:00:00 Mic", "confirmed speaker by name")
    XCTAssertTrue(rendered.contains("Jérôme 00:00:02 System"), "suggested speaker by name")
    XCTAssertTrue(rendered.contains("Unknown 00:00:05 System"), "no speaker: Unknown")
    XCTAssertEqual(rendered.count, 6, "three turns, header and text each")
  }

  func testTasksShowAssigneePriorityAndDueDate() {
    let rendered = lines(.tasks, SampleData.export())
    XCTAssertEqual(rendered, ["[ ] Budgetzahlen prüfen", "Jérôme · High · Sep 29, 2026"])

    var export = SampleData.export()
    export.tasks[0].done = true
    export.tasks[0].priority = .normal
    export.tasks[0].dueDate = nil
    export.tasks[0].assigneePersonID = nil
    export.tasks[0].assigneeName = "Someone"
    XCTAssertEqual(lines(.tasks, export), ["[x] Budgetzahlen prüfen", "Someone"])
  }

  func testEmptyTabsSayWhetherContentIsStillComing() {
    var export = SampleData.export()
    export.segments = []
    export.tasks = []
    export.decisions = []
    export.meeting.summary = nil
    export.meeting.scratchpad = ""

    export.meeting.state = .processing
    XCTAssertEqual(lines(.summary, export), ["Summary appears after processing"])
    XCTAssertEqual(lines(.transcript, export), ["Transcript appears after processing"])
    XCTAssertEqual(lines(.tasks, export), ["Tasks appear after processing"])
    XCTAssertEqual(lines(.scratchpad, export), [""])

    export.meeting.state = .ready
    XCTAssertEqual(
      lines(.summary, export),
      ["Summary skipped", "No LLM endpoint is configured. The transcript is complete."],
      "ready without a summary and no endpoint: the setup row")
    XCTAssertEqual(lines(.transcript, export), ["No transcript"])
    XCTAssertEqual(lines(.tasks, export), ["No tasks", "The summary was skipped."])

    // The template produced nothing: a summary exists, so no setup row.
    export.meeting.summary = SummaryDocument(templateID: "default", sections: [])
    XCTAssertEqual(lines(.summary, export), ["No summary"])
    XCTAssertEqual(lines(.tasks, export), ["No tasks"])
  }

  /// A ready meeting without a summary once an endpoint exists offers the
  /// re-run, with the Summary tab's footnote; the Tasks tab shares the body.
  func testSkippedSummaryOffersTheRunOnceAnEndpointExists() {
    var export = SampleData.export()
    export.tasks = []
    export.decisions = []
    export.meeting.summary = nil
    export.meeting.state = .ready
    XCTAssertEqual(
      TabText.lines(.summary, export: export, llmConfigured: true),
      [
        "No summary yet", "This meeting was processed before an LLM endpoint was configured.",
        "Summary only; the transcript stays as recorded.",
      ])
    XCTAssertEqual(
      TabText.lines(.tasks, export: export, llmConfigured: true),
      ["No tasks", "The summary was skipped."])
    XCTAssertEqual(
      TabText.lines(.transcript, export: export, llmConfigured: true).count, 6,
      "the transcript is untouched by the setup state")

    export.meeting.state = .processing
    XCTAssertEqual(
      TabText.lines(.summary, export: export, llmConfigured: true),
      ["Summary appears after processing"], "pending keeps today's text")
  }
}
