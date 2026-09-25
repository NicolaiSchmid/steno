import XCTest

@MainActor
final class OnboardingViewModelTests: XCTestCase {
  func testStepsRunInOrderAndRequiredOnesGateCompletion() async {
    let permissions = FakePermissions()
    let model = OnboardingViewModel(permissions: permissions)
    await model.load()
    XCTAssertEqual(model.steps.map(\.kind), [.microphone, .systemAudio, .calendar, .localNetwork])
    XCTAssertEqual(model.current, .microphone)
    XCTAssertFalse(model.isComplete)

    await model.request(.microphone)
    XCTAssertEqual(model.current, .systemAudio)
    XCTAssertFalse(model.isComplete)

    await model.request(.systemAudio)
    XCTAssertTrue(model.isComplete, "both required permissions granted")
    XCTAssertFalse(model.isFinished, "optional steps are still open")
    XCTAssertEqual(model.current, .calendar)
    XCTAssertEqual(permissions.requests, [.microphone, .systemAudio])
  }

  func testOptionalStepsCanBeSkippedRequiredCannot() async {
    let permissions = FakePermissions()
    let model = OnboardingViewModel(permissions: permissions)
    await model.load()
    model.skip(.microphone)
    XCTAssertEqual(model.current, .microphone, "required steps cannot be skipped")
    await model.request(.microphone)
    await model.request(.systemAudio)
    model.skip(.calendar)
    XCTAssertEqual(model.current, .localNetwork)
    model.skip(.localNetwork)
    XCTAssertTrue(model.isFinished)
  }

  func testDeniedStepsStayCurrentAndOpenSettings() async {
    let permissions = FakePermissions()
    permissions.answers[.microphone] = .denied
    let model = OnboardingViewModel(permissions: permissions)
    await model.load()
    await model.request(.microphone)
    XCTAssertEqual(model.state(of: .microphone), .denied)
    XCTAssertEqual(model.current, .microphone)
    model.openSystemSettings(.microphone)
    XCTAssertEqual(permissions.openedPanes, [.microphone])
    XCTAssertFalse(model.isComplete)
  }

  func testAlreadyGrantedPermissionsFinishImmediately() async {
    let model = OnboardingViewModel(permissions: FakePermissions.allGranted())
    await model.load()
    XCTAssertTrue(model.isComplete)
    XCTAssertTrue(model.isFinished)
  }
}
