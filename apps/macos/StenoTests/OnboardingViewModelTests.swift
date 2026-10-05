import StenoAdapters
import StenoCore
import XCTest

@MainActor
final class OnboardingViewModelTests: XCTestCase {
  /// One suite per test; a `let` because `tearDown()` is a nonisolated
  /// override under Swift 6.0.
  private let defaultsSuite = "uno.schmid.steno.mac.tests.onboarding.\(UUID().uuidString)"

  override func tearDown() {
    UserDefaults.standard.removePersistentDomain(forName: defaultsSuite)
  }

  private func makeDefaults() throws -> UserDefaults {
    try XCTUnwrap(UserDefaults(suiteName: defaultsSuite))
  }

  /// Built from the environment, the model tells what the stored rule does
  /// to the recordings, one sentence per rule.
  func testRetentionSentenceFollowsTheStoredRule() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = OnboardingViewModel(environment: environment, defaults: try makeDefaults())
    XCTAssertNil(model.retentionSentence, "nothing before load")
    await model.load()
    XCTAssertTrue(model.isComplete, "permissions come from the environment")
    let forever = try XCTUnwrap(model.retentionSentence)
    XCTAssertEqual(
      forever,
      "Recordings are kept until you delete them. Change this any time in Settings > Recording.")

    try await environment.updateSettings { $0.defaultRetention = .keepDays(7) }
    await model.load()
    let days = try XCTUnwrap(model.retentionSentence)
    XCTAssertTrue(days.contains("deleted 7 days after it was processed and exported"), days)
    XCTAssertTrue(days.hasSuffix("Change this any time in Settings > Recording."), days)

    try await environment.updateSettings { $0.defaultRetention = .deleteAfterProcessing }
    await model.load()
    let delete = try XCTUnwrap(model.retentionSentence)
    XCTAssertTrue(delete.contains("as soon as it was transcribed, summarised and exported"), delete)

    XCTAssertNil(OnboardingViewModel(permissions: FakePermissions()).retentionSentence)
  }

  /// The rows in order: the four permissions on page 1, then the two setup
  /// steps on page 2 (two typed lists; the plan's one `steps` array would
  /// need a heterogeneous state).
  func testStepsRunInOrderAndRequiredOnesGateCompletion() async {
    let permissions = FakePermissions()
    let model = OnboardingViewModel(permissions: permissions)
    await model.load()
    XCTAssertEqual(model.steps.map(\.kind), [.microphone, .systemAudio, .calendar, .localNetwork])
    XCTAssertEqual(OnboardingViewModel.SetupStep.allCases, [.summaries, .vault])
    XCTAssertEqual(model.page, .permissions)
    XCTAssertEqual(model.current, .microphone)
    XCTAssertFalse(model.isComplete)

    await model.request(.microphone)
    XCTAssertEqual(model.current, .systemAudio)
    XCTAssertFalse(model.isComplete)

    await model.request(.systemAudio)
    XCTAssertTrue(model.isComplete, "both required permissions granted")
    XCTAssertFalse(model.permissionsHandled, "optional steps are still open")
    XCTAssertFalse(model.finished)
    XCTAssertEqual(model.current, .calendar)
    XCTAssertEqual(permissions.requests, [.microphone, .systemAudio])
  }

  func testOptionalStepsCanBeSkippedRequiredCannot() async throws {
    let permissions = FakePermissions()
    let model = OnboardingViewModel(permissions: permissions, defaults: try makeDefaults())
    await model.load()
    model.skip(.microphone)
    XCTAssertEqual(model.current, .microphone, "required steps cannot be skipped")
    await model.request(.microphone)
    await model.request(.systemAudio)
    model.skip(.calendar)
    XCTAssertEqual(model.current, .localNetwork)
    model.skip(.localNetwork)
    XCTAssertTrue(model.permissionsHandled)
    XCTAssertFalse(model.setupHandled, "the setup steps are still open")
    model.skipSetup(.summaries)
    model.skipSetup(.vault)
    XCTAssertTrue(model.setupHandled)
    XCTAssertFalse(model.finished, "page 1 never finishes; page 2 does")
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

  /// Every permission granted: page 1 has nothing to ask, so the window
  /// opens on page 2 and stays until Summaries and Obsidian vault are saved
  /// or skipped.
  func testAlreadyGrantedPermissionsOpenOnTheSetupPage() async throws {
    let model = OnboardingViewModel(
      permissions: FakePermissions.allGranted(), defaults: try makeDefaults())
    XCTAssertEqual(model.page, .permissions, "before load")
    await model.load()
    XCTAssertTrue(model.isComplete)
    XCTAssertTrue(model.permissionsHandled)
    XCTAssertEqual(model.page, .setup, "straight to page 2")
    XCTAssertFalse(model.setupHandled)
    XCTAssertFalse(model.finished, "the setup steps gate the finish")
  }

  // MARK: Exit

  /// Both rows skipped on page 2: the model finishes and writes the flag;
  /// one row alone does neither.
  func testSkippingBothRowsFinishesAndSetsTheFlag() async throws {
    let defaults = try makeDefaults()
    let model = OnboardingViewModel(permissions: FakePermissions.allGranted(), defaults: defaults)
    await model.load()
    XCTAssertEqual(model.page, .setup)
    model.skipSetup(.summaries)
    XCTAssertFalse(model.finished)
    XCTAssertFalse(defaults.bool(forKey: OnboardingViewModel.onboardingCompletedKey))
    model.skipSetup(.vault)
    XCTAssertTrue(model.finished)
    XCTAssertTrue(defaults.bool(forKey: OnboardingViewModel.onboardingCompletedKey))
  }

  /// Finish with both rows still open writes the flag, so the opener stays
  /// quiet afterwards.
  func testFinishWithRowsOpenSetsTheFlag() async throws {
    let defaults = try makeDefaults()
    let granted = FakePermissions.allGranted()
    let model = OnboardingViewModel(permissions: granted, defaults: defaults)
    await model.load()
    XCTAssertEqual(model.setupState(of: .summaries), .open)
    XCTAssertEqual(model.setupState(of: .vault), .open)
    model.finish()
    XCTAssertTrue(model.finished)
    XCTAssertTrue(defaults.bool(forKey: OnboardingViewModel.onboardingCompletedKey))
    let open = await OnboardingViewModel.shouldOpen(permissions: granted, defaults: defaults)
    XCTAssertFalse(open, "finished and granted: nothing to show")
  }

  /// Later with nothing granted, then both rows skipped: the window finishes
  /// and the flag is set, and the opener still reopens for the missing
  /// required permissions. The one route where the flag and the reopen rule
  /// interact.
  func testLaterThenBothRowsSkippedStillFinishes() async throws {
    let defaults = try makeDefaults()
    let permissions = FakePermissions()
    let model = OnboardingViewModel(permissions: permissions, defaults: defaults)
    await model.load()
    model.advance()
    model.skipSetup(.summaries)
    model.skipSetup(.vault)
    XCTAssertTrue(model.finished)
    XCTAssertTrue(defaults.bool(forKey: OnboardingViewModel.onboardingCompletedKey))
    let open = await OnboardingViewModel.shouldOpen(permissions: permissions, defaults: defaults)
    XCTAssertTrue(open, "a missing required permission reopens the window")
  }

  /// Done and Later on page 1 advance to page 2 instead of finishing; Back
  /// returns; a second `load()` ("Check again") never moves the page.
  func testDoneAndLaterAdvanceToTheSetupPage() async throws {
    let permissions = FakePermissions()
    let model = OnboardingViewModel(permissions: permissions, defaults: try makeDefaults())
    await model.load()
    XCTAssertEqual(model.page, .permissions)
    XCTAssertFalse(model.isComplete, "Later is the button on offer")
    model.advance()
    XCTAssertEqual(model.page, .setup)
    XCTAssertFalse(model.finished, "advancing with open rows finishes nothing")
    model.back()
    XCTAssertEqual(model.page, .permissions)

    await model.request(.microphone)
    await model.request(.systemAudio)
    XCTAssertTrue(model.isComplete, "Done is the button on offer")
    await model.load()
    XCTAssertEqual(model.page, .permissions, "Check again stays on page 1")
    model.advance()
    XCTAssertEqual(model.page, .setup)
  }

  /// `current`, `isComplete`, `page` and the exit for every combination of
  /// the four permission states and the skippable optional steps (81 x 4
  /// cases), with the two setup steps skipped: the window opens on page 2
  /// exactly when the required permissions are granted, and skipping both
  /// rows there finishes.
  func testCurrentIsDerivedForEveryPermissionCombination() async throws {
    let defaults = try makeDefaults()
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
              let model = OnboardingViewModel(permissions: permissions, defaults: defaults)
              await model.load()
              let skipped = optional.enumerated()
                .filter { mask & (1 << $0.offset) != 0 }
                .map(\.element)
              for kind in skipped { model.skip(kind) }
              model.skip(.microphone)
              model.skip(.systemAudio)
              model.skipSetup(.summaries)
              model.skipSetup(.vault)
              let label =
                "mic=\(microphone) audio=\(systemAudio) cal=\(calendar) net=\(localNetwork) skipped=\(skipped)"

              let open = kinds.filter { model.state(of: $0) != .granted && !skipped.contains($0) }
              XCTAssertEqual(model.current, open.first ?? .localNetwork, label)
              XCTAssertEqual(
                model.isComplete, microphone == .granted && systemAudio == .granted, label)
              XCTAssertEqual(model.permissionsHandled, open.isEmpty, label)
              XCTAssertEqual(model.page, model.isComplete ? .setup : .permissions, label)
              XCTAssertTrue(model.setupHandled, "both rows skipped: \(label)")
              XCTAssertEqual(model.finished, model.isComplete, "page 2 finishes: \(label)")
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

  // MARK: Page 2

  /// The ChatGPT way on the Summaries row: no Save, the consent button is
  /// the save, and without a sign-in on this Mac the row stays open and
  /// says what to do.
  func testChatGPTChoiceOnTheSummariesRow() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = OnboardingViewModel(environment: environment, defaults: try makeDefaults())
    await model.load()
    let llm = try XCTUnwrap(model.llm)
    await llm.selectPreset(.codex)
    XCTAssertFalse(model.canSaveSummaries, "ChatGPT has no Save button")
    await model.saveSummaries()
    XCTAssertEqual(model.setupState(of: .summaries), .open)

    await model.confirmSummariesWithCodex()
    XCTAssertEqual(model.setupState(of: .summaries), .open, "no sign-in, no model, still open")
    guard case .unavailable(let text) = llm.codexStatus else {
      return XCTFail("expected the missing sign-in to be reported: \(llm.codexStatus)")
    }
    XCTAssertTrue(text.contains("codex login"), text)
    let settings = try await environment.settings.load()
    XCTAssertEqual(settings.llmProvider, .codex)
    XCTAssertNotNil(settings.codexConfirmedAt, "the user did confirm")
    XCTAssertFalse(settings.llmConfigured)
    XCTAssertFalse(model.finished)
  }

  /// A Codex configuration made in Settings collapses the row with its own
  /// line.
  func testAStoredCodexConfigurationCollapsesTheSummariesRow() async throws {
    let environment = try await TestSupport.environment(seed: false)
    try await environment.updateSettings {
      $0.llmProvider = .codex
      $0.codexModel = "gpt-5.6-terra"
      $0.codexConfirmedAt = Date()
    }
    let model = OnboardingViewModel(environment: environment, defaults: try makeDefaults())
    await model.load()
    XCTAssertEqual(model.setupState(of: .summaries), .saved("Saved: gpt-5.6-terra via ChatGPT"))
  }

  /// Saving Summaries goes through the LLM tab's view model: a valid URL and
  /// a model mark the step done, store the endpoint and rebuild the pipeline.
  func testSavingSummariesConfiguresTheEndpointAndRebuildsThePipeline() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = OnboardingViewModel(environment: environment, defaults: try makeDefaults())
    await model.load()
    XCTAssertEqual(model.setupState(of: .summaries), .open, "nothing configured yet")
    let llm = try XCTUnwrap(model.llm)
    XCTAssertFalse(model.canSaveSummaries, "empty fields cannot be saved")
    llm.baseURLText = "not a url"
    llm.model = "qwen"
    XCTAssertFalse(model.canSaveSummaries)
    XCTAssertNotNil(llm.validationMessage, "the view model's message, shown as in Settings")
    await model.saveSummaries()
    XCTAssertEqual(model.setupState(of: .summaries), .open, "an invalid URL saves nothing")
    XCTAssertNil(llm.error, "the guard stops the save, not the view model's error row")

    llm.baseURLText = "http://127.0.0.1:1234/v1"
    llm.model = ""
    XCTAssertFalse(model.canSaveSummaries, "a URL without a model is not an endpoint")
    llm.model = "qwen"
    XCTAssertTrue(model.canSaveSummaries)
    let before = environment.pipeline
    await model.saveSummaries()
    XCTAssertNil(llm.error, llm.error ?? "")
    XCTAssertEqual(model.setupState(of: .summaries), .saved("Saved: qwen at 127.0.0.1"))
    XCTAssertTrue(model.setupState(of: .summaries).isHandled)
    let settings = try await environment.settings.load()
    XCTAssertTrue(settings.llmConfigured)
    XCTAssertEqual(settings.llmModel, "qwen")
    XCTAssertFalse(before === environment.pipeline, "saving rebuilt the pipeline")

    // A second model over the same store finds the row saved.
    let reopened = OnboardingViewModel(environment: environment, defaults: try makeDefaults())
    await reopened.load()
    XCTAssertEqual(reopened.setupState(of: .summaries), .saved("Saved: qwen at 127.0.0.1"))
    XCTAssertEqual(reopened.setupState(of: .vault), .open)
  }

  /// Saving Obsidian vault goes through the Obsidian tab's view model: a
  /// folder marks the step done; a missing path shows the destination's
  /// message verbatim and saves nothing (an unwritable path takes the same
  /// `validate()` route with its own `ObsidianError`, so one case pins it).
  func testSavingTheVaultValidatesThroughTheDestination() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = OnboardingViewModel(environment: environment, defaults: try makeDefaults())
    await model.load()
    let obsidian = try XCTUnwrap(model.obsidian)
    obsidian.vaultPath = "/definitely/not/a/vault"
    await model.saveVault()
    XCTAssertEqual(
      obsidian.validationMessage, ObsidianError.vaultMissing("/definitely/not/a/vault").description)
    XCTAssertEqual(model.setupState(of: .vault), .open)
    let afterInvalid = try await environment.settings.load()
    XCTAssertNil(afterInvalid.obsidian, "an invalid vault saves nothing")

    let vault = try TestSupport.temporaryDirectory("steno-onboarding-vault")
    defer { try? FileManager.default.removeItem(at: vault) }
    obsidian.vaultPath = vault.path
    await model.saveVault()
    XCTAssertNil(obsidian.validationMessage)
    XCTAssertEqual(model.setupState(of: .vault), .saved("Saved: \(vault.lastPathComponent)"))
    let stored = try await environment.settings.load()
    XCTAssertTrue(stored.vaultConfigured)
    XCTAssertEqual(stored.obsidian?.vaultPath, vault.path)
  }

  /// The opener's rule: the window shows while the flag is unset, whatever
  /// the permissions say, and afterwards only while a required permission
  /// is missing. The preview guard stays in the opener.
  func testShouldOpenFollowsTheFlagAndTheRequiredPermissions() async throws {
    let defaults = try makeDefaults()
    let granted = FakePermissions.allGranted()
    let open = await OnboardingViewModel.shouldOpen(permissions: granted, defaults: defaults)
    XCTAssertTrue(open, "unset flag: this install has not seen page 2")

    let model = OnboardingViewModel(permissions: granted, defaults: defaults)
    model.markCompleted()
    XCTAssertTrue(defaults.bool(forKey: OnboardingViewModel.onboardingCompletedKey))
    let afterFinish = await OnboardingViewModel.shouldOpen(permissions: granted, defaults: defaults)
    XCTAssertFalse(afterFinish, "finished and granted: nothing to show")

    let missing = FakePermissions(states: [.microphone: .granted, .systemAudio: .denied])
    let required = await OnboardingViewModel.shouldOpen(permissions: missing, defaults: defaults)
    XCTAssertTrue(required, "a missing required permission reopens the window")
    let optionalOnly = FakePermissions(states: [.microphone: .granted, .systemAudio: .granted])
    let optional = await OnboardingViewModel.shouldOpen(
      permissions: optionalOnly, defaults: defaults)
    XCTAssertFalse(optional, "optional permissions never reopen the window")
    XCTAssertEqual(granted.requests, [], "the rule asks for nothing")
  }

  /// An install with the flag unset but an endpoint and a vault already in
  /// Settings has nothing to ask: the opener writes the flag and stays
  /// closed instead of flashing page 1 on the way to an empty page 2.
  func testConfiguredInstallWithoutTheFlagDoesNotOpen() async throws {
    let defaults = try makeDefaults()
    let granted = FakePermissions.allGranted()
    let environment = try await TestSupport.environment(seed: false)
    let unconfigured = await OnboardingViewModel.shouldOpen(
      permissions: granted, settings: environment.settings, defaults: defaults)
    XCTAssertTrue(unconfigured, "nothing configured: page 2 has something to ask")
    XCTAssertFalse(defaults.bool(forKey: OnboardingViewModel.onboardingCompletedKey))

    try await environment.updateSettings {
      $0.llmBaseURL = URL(string: "http://127.0.0.1:1234/v1")
      $0.llmModel = "qwen"
      $0.obsidian = ObsidianSettings(
        vaultPath: "/tmp/vault", peopleFolder: nil, includeAudio: false, taskTag: nil)
    }
    let configured = await OnboardingViewModel.shouldOpen(
      permissions: granted, settings: environment.settings, defaults: defaults)
    XCTAssertFalse(configured, "endpoint and vault stored: nothing left to ask")
    XCTAssertTrue(
      defaults.bool(forKey: OnboardingViewModel.onboardingCompletedKey),
      "the check is not repeated on the next launch")

    let missing = FakePermissions(states: [.microphone: .denied, .systemAudio: .granted])
    let required = await OnboardingViewModel.shouldOpen(
      permissions: missing, settings: environment.settings, defaults: defaults)
    XCTAssertTrue(required, "a configured install still reopens for a required permission")
  }
}
