import StenoAdapters
import StenoCore
import XCTest

/// The pure state mapping behind the detail pane: the states table row a
/// tab without content shows (`TabPlaceholder`), when the header carries
/// the Stop control (`HeaderStop`), and the footer's destination name. The
/// rows' words are pinned once, in
/// `TabTextSnapshotTests.testEmptyTabsSayWhetherContentIsStillComing`; here
/// the shape: glyph, spinner, Try again, and which tabs share a row.
final class DetailStatesTests: XCTestCase {
  private let tabs: [MeetingDetailViewModel.Tab] = [.summary, .transcript, .tasks]

  /// Recording, queued, processing and failed are one row for every content
  /// tab; only the ready rows differ per tab.
  func testTransientRowsAreTheSameOnEveryTab() {
    let reason = "The LLM endpoint did not answer. Try again later."
    for state in [.recording, .queued, .processing, .failed(reason: reason)] as [MeetingState] {
      let rows = tabs.map { TabPlaceholder(tab: $0, state: state) }
      XCTAssertEqual(Set(rows).count, 1, "\(state): the tabs disagree")
      let row = rows[0]
      XCTAssertNotNil(row.symbol, "\(state) has a well")
      XCTAssertEqual(row.lines, [row.title, row.body])
      XCTAssertEqual(row.showsSpinner, state == .processing, "only processing spins")
      XCTAssertEqual(row.offersTryAgain, state.isFailed, "only the failed row retries")
    }
    let failed = TabPlaceholder(tab: .summary, state: .failed(reason: reason))
    XCTAssertEqual(failed.body, TabPlaceholder.firstSentence(of: reason))
  }

  func testReadyRowsNameTheMissingContentWithoutAWell() {
    let rows = tabs.map { TabPlaceholder(tab: $0, state: .ready) }
    XCTAssertEqual(Set(rows).count, tabs.count, "each tab names its own missing content")
    for row in rows {
      XCTAssertNil(row.symbol, "\(row.title): the ready variant has no well")
      XCTAssertFalse(row.offersTryAgain)
      XCTAssertFalse(row.showsSpinner)
      XCTAssertFalse(row.title.isEmpty)
      XCTAssertFalse(row.body.isEmpty)
    }
  }

  /// The failed row's body is the reason's first sentence: a full stop that
  /// ends a sentence, or the first line; a version number's dot does not
  /// end it; blank reasons get a generic line.
  func testFirstSentenceOfTheReason() {
    XCTAssertEqual(
      TabPlaceholder.firstSentence(of: "The endpoint did not answer. Re-run later."),
      "The endpoint did not answer.")
    XCTAssertEqual(
      TabPlaceholder.firstSentence(of: "The endpoint did not answer.\nRe-run later."),
      "The endpoint did not answer.")
    XCTAssertEqual(
      TabPlaceholder.firstSentence(of: "Model parakeet-v3.1 is missing.\nDownload it in Settings."),
      "Model parakeet-v3.1 is missing.")
    XCTAssertEqual(
      TabPlaceholder.firstSentence(of: "  \n  Timed out after 30 s  \n"), "Timed out after 30 s")
    XCTAssertEqual(TabPlaceholder.firstSentence(of: "boom"), "boom")
    XCTAssertEqual(TabPlaceholder.firstSentence(of: ""), "The pipeline reported no reason.")
    XCTAssertEqual(TabPlaceholder.firstSentence(of: " \n "), "The pipeline reported no reason.")
  }

  /// The Stop control shows only for the meeting the recorder holds, enabled
  /// while recording and disabled while finishing; never while starting
  /// (no meeting yet) or idle, and never for another meeting.
  func testHeaderStopFollowsTheRecorderForTheActiveMeetingOnly() {
    let live = UUID()
    let other = UUID()
    let since = Date(timeIntervalSince1970: 1_790_250_000)
    XCTAssertEqual(
      HeaderStop.make(meetingID: live, recording: .recording(since: since), activeMeetingID: live),
      .stop(since: since))
    XCTAssertEqual(
      HeaderStop.make(meetingID: live, recording: .stopping, activeMeetingID: live), .stopping)
    XCTAssertNil(
      HeaderStop.make(meetingID: other, recording: .recording(since: since), activeMeetingID: live),
      "another meeting keeps its chip")
    XCTAssertNil(HeaderStop.make(meetingID: live, recording: .idle, activeMeetingID: nil))
    XCTAssertNil(
      HeaderStop.make(meetingID: live, recording: .starting, activeMeetingID: nil),
      "no row is the recorder's while starting")
    XCTAssertNil(
      HeaderStop.make(meetingID: live, recording: .starting, activeMeetingID: live),
      "starting shows no Stop even once the intake has named the row")
    XCTAssertNil(
      HeaderStop.make(meetingID: live, recording: .idle, activeMeetingID: live),
      "an idle recorder shows no Stop even if a stale id lingers")
  }

  /// The footer names the destination, never its storage id.
  func testDeliveryNamesItsDestination() {
    let obsidian = Delivery(
      meetingID: SampleData.meetingID, destinationID: ObsidianFolderDestination.destinationID,
      status: .delivered)
    XCTAssertEqual(obsidian.destinationDisplayName, "Obsidian")
    let unknown = Delivery(
      meetingID: SampleData.meetingID, destinationID: "notion", status: .pending)
    XCTAssertEqual(
      unknown.destinationDisplayName, "notion", "an unknown destination has only its id")
  }
}
