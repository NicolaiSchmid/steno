import StenoAdapters
import StenoCore
import XCTest

/// The pure state mapping behind the detail pane: the states table row a
/// tab without content shows (`TabPlaceholder`), when the header carries
/// the Stop control (`HeaderStop`), and the footer's destination name.
final class DetailStatesTests: XCTestCase {
  private let tabs: [MeetingDetailViewModel.Tab] = [.summary, .transcript, .tasks]

  /// Recording, queued, processing and failed read the same on every
  /// content tab; only the ready rows differ per tab.
  func testRecordingQueuedProcessingAndFailedRowsAreTheSameOnEveryTab() {
    for tab in tabs {
      let recording = TabPlaceholder(tab: tab, state: .recording)
      XCTAssertEqual(recording.symbol, "waveform")
      XCTAssertEqual(recording.title, "Recording")
      XCTAssertEqual(
        recording.body, "The summary, transcript and tasks appear a few minutes after you stop.")
      XCTAssertFalse(recording.showsSpinner)
      XCTAssertFalse(recording.offersTryAgain)

      let queued = TabPlaceholder(tab: tab, state: .queued)
      XCTAssertEqual(queued.symbol, "clock")
      XCTAssertEqual(
        queued.lines, ["Queued", "Processing starts when the current meeting finishes."])

      let processing = TabPlaceholder(tab: tab, state: .processing)
      XCTAssertEqual(processing.symbol, "waveform.badge.magnifyingglass")
      XCTAssertEqual(
        processing.lines,
        ["Processing", "Audio stays on this Mac. This usually takes a minute or two."])
      XCTAssertTrue(processing.showsSpinner, "the processing row has the one spinner")

      let failed = TabPlaceholder(
        tab: tab, state: .failed(reason: "The LLM endpoint did not answer. Try again later."))
      XCTAssertEqual(failed.symbol, "exclamationmark.triangle")
      XCTAssertEqual(failed.lines, ["Processing failed", "The LLM endpoint did not answer."])
      XCTAssertTrue(failed.offersTryAgain)
      XCTAssertFalse(failed.showsSpinner)
    }
  }

  func testReadyRowsNameTheMissingContentWithoutAWell() {
    let summary = TabPlaceholder(tab: .summary, state: .ready)
    XCTAssertNil(summary.symbol)
    XCTAssertEqual(summary.lines, ["No summary", "The template produced no sections."])
    let transcript = TabPlaceholder(tab: .transcript, state: .ready)
    XCTAssertNil(transcript.symbol)
    XCTAssertEqual(transcript.lines, ["No transcript", "No speech was recognised."])
    let tasks = TabPlaceholder(tab: .tasks, state: .ready)
    XCTAssertNil(tasks.symbol)
    XCTAssertEqual(tasks.lines, ["No tasks", "No tasks were found."])
    XCTAssertFalse(summary.offersTryAgain)
    XCTAssertFalse(summary.showsSpinner)
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
