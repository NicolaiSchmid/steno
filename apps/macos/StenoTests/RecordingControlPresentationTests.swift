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
