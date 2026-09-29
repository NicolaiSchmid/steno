import StenoCore
import XCTest

/// The four tabs rendered as text for StenoCore's fixture meeting, pinned
/// as goldens under `Tests/Fixtures/snapshots/macos/` (rewritten with
/// `STENO_UPDATE_SNAPSHOTS=1`; the diff is the reviewed output change). The
/// UI smoke test clicks the same tabs and looks for one string each; those
/// strings are asserted here too, so the two proofs cannot drift apart.
@MainActor
final class TabTextSnapshotTests: XCTestCase {
  private let locale = Locale(identifier: "en_US_POSIX")
  private let utc = TimeZone(identifier: "UTC")!

  private func lines(
    _ tab: MeetingDetailViewModel.Tab, _ export: MeetingExport,
    progress: ProcessingProgressModel.Entry? = nil
  ) -> [String] {
    TabText.lines(tab, export: export, progress: progress, locale: locale, timeZone: utc)
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

    // Processing with the progress model's entry: the card's title row is
    // the pending text, on every tab, the scratchpad above its own line.
    export.meeting.state = .processing
    let transcribing = ProcessingProgressModel.Entry(
      meetingID: export.meeting.id,
      progress: ProcessingProgress(
        stage: .transcribe, fraction: 0.1, nextFraction: 0.4, estimatedRemaining: .seconds(150),
        isEstimateSeeded: false),
      since: TestSupport.now)
    let card = ["Transcribing…", "~3 min remaining"]
    XCTAssertEqual(lines(.summary, export, progress: transcribing), card)
    XCTAssertEqual(lines(.transcript, export, progress: transcribing), card)
    XCTAssertEqual(lines(.tasks, export, progress: transcribing), card)
    XCTAssertEqual(lines(.scratchpad, export, progress: transcribing), card + [""])
    // Before the run's first event the title alone.
    let waiting = ProcessingProgressModel.Entry(
      meetingID: export.meeting.id, since: TestSupport.now)
    XCTAssertEqual(lines(.summary, export, progress: waiting), ["Waiting to process"])
    // Without an entry (the model has not seen the meeting yet) the states
    // table's processing row stands, the same on every content tab.
    let processing = ["Processing", "Audio stays on this Mac. This usually takes a minute or two."]
    XCTAssertEqual(lines(.summary, export), processing)
    XCTAssertEqual(lines(.transcript, export), processing)
    XCTAssertEqual(lines(.tasks, export), processing)
    XCTAssertEqual(lines(.scratchpad, export), [""])

    // Recording, queued and failed: the table's rows, one line of copy each.
    export.meeting.state = .recording
    XCTAssertEqual(
      lines(.summary, export),
      ["Recording", "The summary, transcript and tasks appear a few minutes after you stop."])
    export.meeting.state = .queued
    XCTAssertEqual(
      lines(.transcript, export),
      ["Queued", "Processing starts when the current meeting finishes."])
    export.meeting.state = .failed(reason: "The LLM endpoint did not answer.\nRe-run later.")
    XCTAssertEqual(
      lines(.tasks, export), ["Processing failed", "The LLM endpoint did not answer."])
    export.meeting.state = .processing

    // Content that is still there while the meeting is processed again sits
    // under the card.
    let processedAgain = SampleData.export()
    XCTAssertEqual(
      Array(lines(.summary, processedAgain, progress: transcribing).prefix(3)),
      card + ["Executive Summary"])

    export.meeting.state = .ready
    XCTAssertEqual(
      lines(.summary, export),
      ["Summary skipped", "No LLM endpoint is configured. The transcript is complete."],
      "ready without a summary and no endpoint: the setup row")
    XCTAssertEqual(lines(.transcript, export), ["No transcript", "No speech was recognised."])
    XCTAssertEqual(lines(.tasks, export), ["No tasks", "The summary was skipped."])

    // The template produced nothing: a summary exists, so no setup row.
    export.meeting.summary = SummaryDocument(templateID: "default", sections: [])
    XCTAssertEqual(lines(.summary, export), ["No summary", "The template produced no sections."])
    XCTAssertEqual(lines(.tasks, export), ["No tasks", "No tasks were found."])
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
      ["Processing", "Audio stays on this Mac. This usually takes a minute or two."],
      "pending keeps the states table's row")
  }
}
