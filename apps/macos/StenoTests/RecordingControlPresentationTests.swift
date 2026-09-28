import XCTest

/// The state table of the recording control: every `RecordingState` times
/// every combination of denied required permissions, asserted once here for
/// the sidebar control, the Record menu and the menu bar item.
final class RecordingControlPresentationTests: XCTestCase {
  private let deniedSets: [[PermissionKind]] = [
    [], [.microphone], [.systemAudio], [.microphone, .systemAudio],
  ]

  func testIdleWithNothingDeniedOffersBothModes() {
    XCTAssertEqual(
      RecordingControlPresentation.make(state: .idle, denied: []),
      RecordingControlPresentation(
        label: "Record call", role: .primary, isEnabled: true, isBusy: false,
        offersInPerson: true, disabledReason: nil))
  }

  func testIdleWithADeniedRequiredKindIsDisabledWithTheReason() {
    let expectations: [([PermissionKind], String)] = [
      ([.microphone], "Microphone access is denied."),
      ([.systemAudio], "System audio access is denied."),
      ([.microphone, .systemAudio], "Microphone access is denied. System audio access is denied."),
    ]
    for (denied, reason) in expectations {
      XCTAssertEqual(
        RecordingControlPresentation.make(state: .idle, denied: denied),
        RecordingControlPresentation(
          label: "Record call", role: .primary, isEnabled: false, isBusy: false,
          offersInPerson: false, disabledReason: reason),
        "\(denied)")
    }
  }

  func testStartingIsBusyAndDisabledWhateverIsDenied() {
    for denied in deniedSets {
      XCTAssertEqual(
        RecordingControlPresentation.make(state: .starting, denied: denied),
        RecordingControlPresentation(
          label: "Starting…", role: .primary, isEnabled: false, isBusy: true,
          offersInPerson: false, disabledReason: nil),
        "transient states show no reason: \(denied)")
    }
  }

  func testRecordingIsStopWhateverIsDenied() {
    for denied in deniedSets {
      XCTAssertEqual(
        RecordingControlPresentation.make(
          state: .recording(since: TestSupport.now), denied: denied),
        RecordingControlPresentation(
          label: "Stop", role: .stop, isEnabled: true, isBusy: false,
          offersInPerson: false, disabledReason: nil),
        "a running recording can always be stopped: \(denied)")
    }
  }

  func testStoppingIsBusyAndDisabledWhateverIsDenied() {
    for denied in deniedSets {
      XCTAssertEqual(
        RecordingControlPresentation.make(state: .stopping, denied: denied),
        RecordingControlPresentation(
          label: "Finishing…", role: .stop, isEnabled: false, isBusy: true,
          offersInPerson: false, disabledReason: nil),
        "\(denied)")
    }
  }

  /// The Record menu keeps macOS title case for the two settled states and
  /// shows the transient labels as they are; a denial disables it without
  /// changing the label.
  func testRecordMenuLabelUsesMenuCasing() {
    XCTAssertEqual(
      RecordingControlPresentation.make(state: .idle, denied: []).menuLabel, "Record Call")
    XCTAssertEqual(
      RecordingControlPresentation.make(state: .recording(since: TestSupport.now), denied: [])
        .menuLabel,
      "Stop Recording")
    XCTAssertEqual(
      RecordingControlPresentation.make(state: .starting, denied: []).menuLabel, "Starting…")
    XCTAssertEqual(
      RecordingControlPresentation.make(state: .stopping, denied: []).menuLabel, "Finishing…")
    let denied = RecordingControlPresentation.make(state: .idle, denied: [.microphone])
    XCTAssertEqual(denied.menuLabel, "Record Call")
    XCTAssertFalse(denied.isEnabled)
  }

  /// Before the controller exists the menu reads as the idle row, disabled.
  func testUnavailableIsTheIdleRowDisabled() {
    let unavailable = RecordingControlPresentation.unavailable
    XCTAssertEqual(unavailable.menuLabel, "Record Call")
    XCTAssertEqual(unavailable.role, .primary)
    XCTAssertFalse(unavailable.isEnabled)
    XCTAssertFalse(unavailable.isBusy)
    XCTAssertFalse(unavailable.offersInPerson)
    XCTAssertNil(unavailable.disabledReason)
  }

  func testAnOptionalKindNeverDisables() {
    for denied in [[.calendar], [.localNetwork], [.calendar, .localNetwork]] as [[PermissionKind]] {
      let presentation = RecordingControlPresentation.make(state: .idle, denied: denied)
      XCTAssertTrue(presentation.isEnabled, "\(denied)")
      XCTAssertTrue(presentation.offersInPerson, "\(denied)")
      XCTAssertNil(presentation.disabledReason, "\(denied)")
    }
    let mixed = RecordingControlPresentation.make(state: .idle, denied: [.calendar, .microphone])
    XCTAssertFalse(mixed.isEnabled)
    XCTAssertEqual(
      mixed.disabledReason, "Microphone access is denied.", "only the required kind is named")
  }
}
