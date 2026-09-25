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

  /// `current`, `isComplete` and `isFinished` for every combination of the
  /// four permission states and the skippable optional steps (81 x 4 cases).
  func testCurrentIsDerivedForEveryPermissionCombination() async {
    let states: [PermissionState] = [.unknown, .granted, .denied]
    let kinds = PermissionKind.allCases
    let optional = kinds.filter { !$0.isRequired }
    XCTAssertEqual(optional, [.calendar, .localNetwork])
    var cases = 0
    for microphone in states {
      for systemAudio in states {
        for calendar in states {
          for localNetwork in states {
            for mask in 0..<(1 << optional.count) {
              let permissions = FakePermissions(states: [
                .microphone: microphone, .systemAudio: systemAudio, .calendar: calendar,
                .localNetwork: localNetwork,
              ])
              let model = OnboardingViewModel(permissions: permissions)
              await model.load()
              let skipped = optional.enumerated()
                .filter { mask & (1 << $0.offset) != 0 }
                .map(\.element)
              for kind in skipped { model.skip(kind) }
              model.skip(.microphone)
              model.skip(.systemAudio)
              let label =
                "mic=\(microphone) audio=\(systemAudio) cal=\(calendar) net=\(localNetwork) skipped=\(skipped)"

              let open = kinds.filter { model.state(of: $0) != .granted && !skipped.contains($0) }
              XCTAssertEqual(model.current, open.first ?? .localNetwork, label)
              XCTAssertEqual(
                model.isComplete, microphone == .granted && systemAudio == .granted, label)
              XCTAssertEqual(model.isFinished, open.isEmpty, label)
              XCTAssertEqual(model.skipped, Set(skipped), "required steps never skip: \(label)")
              XCTAssertEqual(permissions.requests, [], "deriving state asks for nothing: \(label)")
              cases += 1
            }
          }
        }
      }
    }
    XCTAssertEqual(cases, 81 * 4)
  }

  func testRequestUpdatesOnlyTheRequestedStep() async {
    let permissions = FakePermissions()
    permissions.answers[.calendar] = .denied
    let model = OnboardingViewModel(permissions: permissions)
    await model.load()
    XCTAssertNil(model.requesting)
    await model.request(.calendar)
    XCTAssertNil(model.requesting, "cleared once the prompt returns")
    XCTAssertEqual(model.state(of: .calendar), .denied)
    for kind in PermissionKind.allCases where kind != .calendar {
      XCTAssertEqual(model.state(of: kind), .unknown, "\(kind) untouched")
    }
    XCTAssertEqual(
      model.current, .microphone, "a denied optional step never blocks the required ones")
  }
}
