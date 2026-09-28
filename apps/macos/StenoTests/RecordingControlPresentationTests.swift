import XCTest

/// The state table of the recording control: every `RecordingState` times
/// every combination of denied required permissions, asserted once here for
/// the sidebar control, the Record menu and the menu bar item.
final class RecordingControlPresentationTests: XCTestCase {
  private let deniedSets: [[PermissionKind]] = [
    [], [.microphone], [.systemAudio], [.microphone, .systemAudio],
  ]

  func testIdleWithNothingDeniedOffersBothModes() {
    let presentation = RecordingControlPresentation.make(state: .idle, denied: [])
    XCTAssertEqual(
      presentation,
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
      let presentation = RecordingControlPresentation.make(state: .idle, denied: denied)
      XCTAssertEqual(presentation.label, "Record call", "\(denied)")
      XCTAssertEqual(presentation.role, .primary, "\(denied)")
      XCTAssertFalse(presentation.isEnabled, "\(denied)")
      XCTAssertFalse(presentation.isBusy, "\(denied)")
      XCTAssertFalse(presentation.offersInPerson, "\(denied)")
      XCTAssertEqual(presentation.disabledReason, reason)
    }
  }

  func testStartingIsBusyAndDisabledWhateverIsDenied() {
    for denied in deniedSets {
      let presentation = RecordingControlPresentation.make(state: .starting, denied: denied)
      XCTAssertEqual(presentation.label, "Starting…", "\(denied)")
      XCTAssertEqual(presentation.role, .primary, "\(denied)")
      XCTAssertFalse(presentation.isEnabled, "\(denied)")
      XCTAssertTrue(presentation.isBusy, "\(denied)")
      XCTAssertFalse(presentation.offersInPerson, "\(denied)")
      XCTAssertNil(presentation.disabledReason, "transient states show no reason: \(denied)")
    }
  }

  func testRecordingIsStopWhateverIsDenied() {
    for denied in deniedSets {
      let presentation = RecordingControlPresentation.make(
        state: .recording(since: TestSupport.now), denied: denied)
      XCTAssertEqual(presentation.label, "Stop", "\(denied)")
      XCTAssertEqual(presentation.role, .stop, "\(denied)")
      XCTAssertTrue(presentation.isEnabled, "a running recording can always be stopped: \(denied)")
      XCTAssertFalse(presentation.isBusy, "\(denied)")
      XCTAssertFalse(presentation.offersInPerson, "\(denied)")
      XCTAssertNil(presentation.disabledReason, "\(denied)")
    }
  }

  func testStoppingIsBusyAndDisabledWhateverIsDenied() {
    for denied in deniedSets {
      let presentation = RecordingControlPresentation.make(state: .stopping, denied: denied)
      XCTAssertEqual(presentation.label, "Finishing…", "\(denied)")
      XCTAssertEqual(presentation.role, .stop, "\(denied)")
      XCTAssertFalse(presentation.isEnabled, "\(denied)")
      XCTAssertTrue(presentation.isBusy, "\(denied)")
      XCTAssertFalse(presentation.offersInPerson, "\(denied)")
      XCTAssertNil(presentation.disabledReason, "\(denied)")
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

  func testDeniedMessagesAreSentences() {
    for kind in PermissionKind.allCases {
      XCTAssertNotEqual(kind.deniedMessage, kind.rawValue)
      XCTAssertEqual(kind.deniedMessage.first?.isUppercase, true, kind.deniedMessage)
      XCTAssertTrue(kind.deniedMessage.hasSuffix("."), kind.deniedMessage)
    }
  }
}
