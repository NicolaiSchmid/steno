import StenoCore
import XCTest

/// `Settings.llmConfigured` and `vaultConfigured` (pure functions of the
/// stored settings), the banner message they select, and the detail pane's
/// `SummaryStatus` and `ExportStatus` selectors.
final class SetupStatusTests: XCTestCase {
  private func settings(llm: Bool, vault: Bool) -> Settings {
    var settings = Settings()
    if llm {
      settings.llmBaseURL = URL(string: "http://127.0.0.1:1234/v1")
      settings.llmModel = "qwen"
    }
    if vault {
      settings.obsidian = ObsidianSettings(
        vaultPath: "/tmp/vault", peopleFolder: nil, includeAudio: false, taskTag: nil)
    }
    return settings
  }

  func testConfiguredFlagsCoverTheFourCombinations() {
    for llm in [false, true] {
      for vault in [false, true] {
        let settings = settings(llm: llm, vault: vault)
        XCTAssertEqual(settings.llmConfigured, llm, "llm=\(llm) vault=\(vault)")
        XCTAssertEqual(settings.vaultConfigured, vault, "llm=\(llm) vault=\(vault)")
      }
    }
    // A URL without a model, or a model without a URL, is not an endpoint.
    var half = Settings()
    half.llmBaseURL = URL(string: "http://127.0.0.1:1234/v1")
    XCTAssertFalse(half.llmConfigured)
    half.llmBaseURL = nil
    half.llmModel = "qwen"
    XCTAssertFalse(half.llmConfigured)
  }

  func testBannerMessageFollowsWhatIsMissing() {
    XCTAssertNil(SetupBannerMessage(settings: settings(llm: true, vault: true)))
    let both = SetupBannerMessage(settings: settings(llm: false, vault: false))
    XCTAssertEqual(both, .bothMissing)
    XCTAssertTrue(both?.offersSummaries == true && both?.offersVault == true)
    let endpoint = SetupBannerMessage(settings: settings(llm: false, vault: true))
    XCTAssertEqual(endpoint, .endpointMissing)
    XCTAssertTrue(endpoint?.offersSummaries == true && endpoint?.offersVault == false)
    let vault = SetupBannerMessage(settings: settings(llm: true, vault: false))
    XCTAssertEqual(vault, .vaultMissing)
    XCTAssertTrue(vault?.offersSummaries == false && vault?.offersVault == true)
    XCTAssertTrue(both?.text.hasPrefix("Summaries and export are off.") == true)
    XCTAssertTrue(endpoint?.text.hasPrefix("Summaries are off.") == true)
    XCTAssertTrue(vault?.text.hasPrefix("Export is off.") == true)
  }

  func testSummaryStatusKeysOffTheSummaryTheStateAndTheEndpoint() {
    var meeting = SampleData.meeting(state: .ready)
    XCTAssertNotNil(meeting.summary)
    XCTAssertEqual(SummaryStatus(meeting: meeting, llmConfigured: false), .present)
    XCTAssertEqual(SummaryStatus(meeting: meeting, llmConfigured: true), .present)

    meeting.summary = nil
    XCTAssertEqual(SummaryStatus(meeting: meeting, llmConfigured: false), .skippedUnconfigured)
    XCTAssertEqual(SummaryStatus(meeting: meeting, llmConfigured: true), .skippedRunnable)

    for state in [MeetingState.queued, .processing, .recording, .failed(reason: "boom")] {
      meeting.state = state
      XCTAssertEqual(SummaryStatus(meeting: meeting, llmConfigured: true), .pending, "\(state)")
    }
    XCTAssertEqual(SummaryStatus(meeting: nil, llmConfigured: true), .pending)
  }

  /// Which action and footnote each skipped row carries; the words are
  /// pinned by `TabTextSnapshotTests`.
  func testSkippedRowsCarryTheActions() throws {
    let unconfigured = try XCTUnwrap(SummaryStatus.skippedUnconfigured.skippedRow(for: .summary))
    XCTAssertEqual(unconfigured.action, .setUpSummaries)
    XCTAssertNil(unconfigured.footnote)

    let runnable = try XCTUnwrap(SummaryStatus.skippedRunnable.skippedRow(for: .summary))
    XCTAssertEqual(runnable.action, .runSummary)
    XCTAssertNotNil(runnable.footnote, "the re-run keeps the transcript as recorded")

    XCTAssertEqual(
      SummaryStatus.skippedUnconfigured.skippedRow(for: .tasks)?.action, .setUpSummaries)
    XCTAssertEqual(SummaryStatus.skippedRunnable.skippedRow(for: .tasks)?.action, .runSummary)

    XCTAssertNil(SummaryStatus.present.skippedRow(for: .summary))
    XCTAssertNil(SummaryStatus.pending.skippedRow(for: .tasks))
    XCTAssertNil(SummaryStatus.skippedRunnable.skippedRow(for: .transcript))
    XCTAssertNil(SummaryStatus.skippedRunnable.skippedRow(for: .scratchpad))
  }

  func testExportStatusKeysOffTheDeliveriesAndTheVault() {
    XCTAssertEqual(ExportStatus(deliveries: [], vaultConfigured: false), .noVault)
    XCTAssertEqual(ExportStatus(deliveries: [], vaultConfigured: true), .notExported)
    let delivery = SampleData.delivery()
    XCTAssertEqual(
      ExportStatus(deliveries: [delivery], vaultConfigured: false), .exported([delivery]),
      "rows win over the configuration: a vault removed later keeps its badges")
  }
}
