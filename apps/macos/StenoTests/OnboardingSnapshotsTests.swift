import Foundation
import StenoBridge
import StenoCore
import Testing

// The onboarding window's snapshot over the preview environment, and the
// bridge's command routing, hostless: no web view anywhere, a recording sink
// where `WebBridge` would be, a stub where the folder panel would be, fake
// permissions and a throwaway defaults suite. The model's rules stay pinned
// by `OnboardingViewModelTests`; here the wire shape the page renders, and
// that a command reaches the model and comes back as a publish.

@MainActor
private final class RecordingSink: BridgeEventSink {
  private(set) var events: [BridgeEvent] = []

  func emit(_ event: BridgeEvent) {
    events.append(event)
  }

  var last: JSONValue? { events.last?.payload }
}

/// Polls `condition` every 10 ms up to `timeout` and records an issue on
/// timeout. Used only where a main-actor hop must be given time to deliver.
@MainActor
private func eventually(
  _ description: String, timeout: Duration = .seconds(10), _ condition: @MainActor () -> Bool
) async {
  let clock = ContinuousClock()
  let deadline = clock.now + timeout
  while !condition() {
    if clock.now >= deadline {
      Issue.record("timed out waiting for \(description)")
      return
    }
    try? await Task.sleep(for: .milliseconds(10))
  }
}

private func request(_ id: String, _ method: String, _ params: Any = NSNull()) -> [String: Any] {
  ["id": id, "method": method, "params": params]
}

extension JSONValue {
  /// An array element, for reading one row of a published list.
  fileprivate subscript(index: Int) -> JSONValue? {
    guard case .array(let items) = self, items.indices.contains(index) else { return nil }
    return items[index]
  }
}

/// A fresh defaults suite per test, so the completed flag never leaks.
private func defaults() throws -> UserDefaults {
  try #require(UserDefaults(suiteName: "uno.schmid.steno.mac.tests.onboarding.\(UUID())"))
}

/// The model over the environment's settings with `permissions` instead of
/// the preview's all-granted ones.
@MainActor
private func model(
  _ environment: AppEnvironment, permissions: FakePermissions, defaults: UserDefaults
) -> OnboardingViewModel {
  OnboardingViewModel(
    permissions: permissions, settings: environment.settings, environment: environment,
    defaults: defaults)
}

// MARK: - Snapshot over the model

@Suite @MainActor struct OnboardingSnapshotsTests {
  @Test func snapshotSpellsThePagesStepsAndRows() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let permissions = FakePermissions(states: [.microphone: .granted])
    let onboarding = model(environment, permissions: permissions, defaults: try defaults())

    let before = OnboardingSnapshot(model: onboarding)
    #expect(before.page == .permissions)
    #expect(
      before.permissions.map(\.kind) == [.microphone, .systemAudio, .calendar, .localNetwork])
    #expect(before.permissions.map(\.isRequired) == [true, true, false, false])
    #expect(before.permissions.allSatisfy { $0.state == .unknown }, "nothing read before load")
    #expect(before.retentionSentence == nil)
    #expect(before.summaries == nil, "page 1 carries no Summaries form")

    await onboarding.load()
    let loaded = OnboardingSnapshot(model: onboarding)
    #expect(loaded.permissions.first?.state == .granted)
    #expect(loaded.permissions.allSatisfy { !$0.isRequesting && !$0.isSkipped })
    #expect(!loaded.permissionsComplete, "system audio is still unknown")
    #expect(loaded.setup.map(\.kind) == [.summaries, .vault])
    #expect(loaded.setup.allSatisfy { $0.state == .open && $0.savedLine == nil })
    #expect(!loaded.canSaveSummaries, "no model name yet")
    #expect(loaded.summaries == nil && loaded.vault == nil, "page 1 carries no forms")
    onboarding.advance()
    let setup = OnboardingSnapshot(model: onboarding)
    let summaries = try #require(setup.summaries)
    #expect(summaries.subtitle == "", "no sidebar to subtitle")
    #expect(summaries.presetID == LLMPreset.lmStudio.rawValue)
    #expect(summaries.baseURL == "http://127.0.0.1:1234/v1")
    let vault = try #require(setup.vault)
    #expect(vault.path == nil && vault.name == nil && vault.validationMessage == nil)
    #expect(loaded.retentionSentence?.hasPrefix("Recordings are kept forever in ") == true)
    #expect(!loaded.finished)

    onboarding.skip(.calendar)
    await onboarding.request(.systemAudio)
    let handled = OnboardingSnapshot(model: onboarding)
    #expect(handled.permissions[1].state == .granted)
    #expect(handled.permissions[2].isSkipped)
    #expect(handled.permissionsComplete)

    onboarding.advance()
    onboarding.skipSetup(.vault)
    onboarding.llm?.model = "qwen"
    let setup = OnboardingSnapshot(model: onboarding)
    #expect(setup.page == .setup)
    #expect(setup.setup[1].state == .skipped)
    #expect(setup.canSaveSummaries)
    #expect(!setup.finished, "the Summaries row is still open")

    onboarding.finish()
    #expect(OnboardingSnapshot(model: onboarding).finished)
  }

  @Test func vaultRowReadsTheObsidianModel() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let obsidian = ObsidianSettingsViewModel(environment: environment)
    await obsidian.load()
    #expect(OnboardingSnapshot.Vault(obsidian: obsidian).path == nil)
    obsidian.vaultPath = "/Users/nicolai/Notes/Work Vault"
    let chosen = OnboardingSnapshot.Vault(obsidian: obsidian)
    #expect(chosen.path == "/Users/nicolai/Notes/Work Vault")
    #expect(chosen.name == "Work Vault")
    #expect(chosen.validationMessage == nil)
  }
}

// MARK: - Command routing

@Suite @MainActor struct OnboardingBridgeTests {
  /// A host over a fresh controller with `permissions` and the folder panel
  /// answering `chosen`.
  private func bridge(
    _ environment: AppEnvironment, permissions: FakePermissions = FakePermissions(),
    chosen: URL? = nil
  ) throws -> OnboardingBridge {
    OnboardingBridge(
      controller: AppController(environment: environment),
      model: model(environment, permissions: permissions, defaults: try defaults()),
      chooseFolder: { _ in chosen })
  }

  /// `page.ready` publishes the topic once and nothing before; a permission
  /// command republishes, and page 1 moves on by itself once every step is
  /// handled.
  @Test func pageReadyPublishesAndPermissionCommandsRepublish() async throws {
    let permissions = FakePermissions(states: [.microphone: .granted])
    let host = try bridge(try await TestSupport.environment(seed: false), permissions: permissions)
    let sink = RecordingSink()
    host.attach(sink)
    host.start()
    await host.model.load()
    #expect(sink.events.isEmpty, "nothing is sent before the page is ready")

    let ready = await BridgeDispatcher.dispatch(request("r1", "page.ready"), host: host)
    #expect(ready == BridgeReply(id: "r1"))
    #expect(sink.events.map(\.topic) == [.onboarding])
    #expect(sink.last?["page"] == .string("permissions"))
    #expect(sink.last?["permissionsComplete"] == .bool(false))

    let skip = await BridgeDispatcher.dispatch(
      request("r2", "onboarding.skip", ["kind": "calendar"]), host: host)
    #expect(skip == BridgeReply(id: "r2"))
    #expect(host.model.skipped == [.calendar])
    await eventually("the skip republished") {
      sink.last?["permissions"]?[2]?["isSkipped"] == .bool(true)
    }

    let grant = await BridgeDispatcher.dispatch(
      request("r3", "onboarding.request", ["kind": "systemAudio"]), host: host)
    #expect(grant == BridgeReply(id: "r3"))
    #expect(permissions.requests == [.systemAudio])
    #expect(host.model.page == .permissions, "the local network step is still open")
    await eventually("the grant republished") {
      sink.last?["permissionsComplete"] == .bool(true)
    }

    let gotIt = await BridgeDispatcher.dispatch(
      request("r4", "onboarding.skip", ["kind": "localNetwork"]), host: host)
    #expect(gotIt == BridgeReply(id: "r4"))
    #expect(host.model.page == .setup, "every step handled: page 1 moves on by itself")
    await eventually("page 2 republished") { sink.last?["page"] == .string("setup") }

    let back = await BridgeDispatcher.dispatch(request("r5", "onboarding.back"), host: host)
    #expect(back == BridgeReply(id: "r5"))
    #expect(host.model.page == .permissions, "Back stays on page 1 although every step is handled")
  }

  /// The Summaries draft: `updateSummaries` changes the form without
  /// storing, `saveSummaries` stores and collapses the row; the vault
  /// chooser's answer is saved at once; both rows handled finishes.
  @Test func setupCommandsReachTheModelsAndFinish() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let vault = try TestSupport.temporaryDirectory("steno-vault")
    defer { try? FileManager.default.removeItem(at: vault) }
    let host = try bridge(environment, permissions: .allGranted(), chosen: vault)
    let sink = RecordingSink()
    host.attach(sink)
    host.start()
    await host.model.load()
    #expect(host.model.page == .setup, "every permission granted: straight to page 2")
    _ = await BridgeDispatcher.dispatch(request("r0", "page.ready"), host: host)

    let update = await BridgeDispatcher.dispatch(
      request(
        "r1", "settings.summaries.update",
        ["baseURL": "http://127.0.0.1:9/v1", "model": "qwen", "apiKey": "sk-typed"]),
      host: host)
    #expect(update == BridgeReply(id: "r1"))
    #expect(host.model.llm?.model == "qwen")
    #expect(try await environment.settings.load().llmModel == nil, "nothing stored before save")
    await eventually("the draft republished") {
      sink.last?["canSaveSummaries"] == .bool(true)
        && sink.last?["summaries"]?["model"] == .string("qwen")
    }
    let published = try #require(sink.last)
    let encoded = String(decoding: try BridgeJSON.encode(published), as: UTF8.self)
    #expect(!encoded.contains("sk-typed"), "the key stays off the wire")

    let save = await BridgeDispatcher.dispatch(
      request("r2", "onboarding.saveSummaries"), host: host)
    #expect(save == BridgeReply(id: "r2"))
    #expect(try await environment.settings.load().llmModel == "qwen")
    #expect(host.model.setupState(of: .summaries) == .saved("Saved: qwen at 127.0.0.1"))
    #expect(!host.model.finished, "the vault row is still open")
    await eventually("the saved row republished") {
      sink.last?["setup"]?[0]?["state"] == .string("saved")
    }

    let chosen = await BridgeDispatcher.dispatch(
      request("r3", "onboarding.chooseVault"), host: host)
    #expect(chosen.result?["path"] == .string(vault.path))
    #expect(host.model.obsidian?.vaultPath == vault.path)
    #expect(try await environment.settings.load().obsidian?.vaultPath == vault.path)
    #expect(host.model.setupState(of: .vault) == .saved("Saved: \(vault.lastPathComponent)"))
    #expect(host.model.finished, "both rows handled finishes onboarding")
    await eventually("finished republished") { sink.last?["finished"] == .bool(true) }
  }

  /// A cancelled panel changes nothing and replies no path; a skipped vault
  /// row collapses; Finish writes the flag the opener reads.
  @Test func cancelledChooserSkipAndFinish() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let host = try bridge(environment, permissions: .allGranted())
    host.start()
    await host.model.load()

    let declined = await BridgeDispatcher.dispatch(
      request("r1", "onboarding.chooseVault"), host: host)
    #expect(declined.error == nil)
    #expect((declined.result?["path"] ?? .null) == .null, "a cancelled panel replies no path")
    #expect(host.model.obsidian?.vaultPath == "")
    #expect(host.model.setupState(of: .vault) == .open)

    let skip = await BridgeDispatcher.dispatch(
      request("r2", "onboarding.skipSetup", ["step": "vault"]), host: host)
    #expect(skip == BridgeReply(id: "r2"))
    #expect(host.model.setupState(of: .vault) == .skipped)
    #expect(!host.model.finished)

    var closed = 0
    host.closeWindow = { closed += 1 }
    let finish = await BridgeDispatcher.dispatch(request("r3", "onboarding.finish"), host: host)
    #expect(finish == BridgeReply(id: "r3"))
    #expect(host.model.finished)
    let close = await BridgeDispatcher.dispatch(
      request("r4", "window.close", ["window": "onboarding"]), host: host)
    #expect(close == BridgeReply(id: "r4"))
    #expect(closed == 1, "the page's window.close runs the window's dismissal")
  }

  @Test func otherWindowsMethodsAndBadParamsAreTypedErrors() async throws {
    let host = try bridge(try await TestSupport.environment(seed: false))
    host.start()
    var opened: [WindowParams] = []
    host.openWindow = { opened.append($0) }

    let filter = await BridgeDispatcher.dispatch(
      request("r1", "meetings.setFilter", ["filter": "failed"]), host: host)
    #expect(filter.error?.code == .unknownMethod)
    let settings = await BridgeDispatcher.dispatch(
      request("r2", "settings.summaries.save"), host: host)
    #expect(settings.error?.code == .unknownMethod)
    let preset = await BridgeDispatcher.dispatch(
      request("r3", "settings.summaries.selectPreset", ["value": "nope"]), host: host)
    #expect(preset.error?.code == .invalidParams)
    let missing = await BridgeDispatcher.dispatch(request("r4", "onboarding.request"), host: host)
    #expect(missing.error?.code == .invalidParams)
    let other = await BridgeDispatcher.dispatch(
      request("r5", "window.close", ["window": "main"]), host: host)
    #expect(other.error?.code == .invalidParams, "the onboarding window closes only itself")
    let url = await BridgeDispatcher.dispatch(
      request("r6", "system.openURL", ["url": "file:///etc/hosts"]), host: host)
    #expect(url.error?.code == .invalidParams)

    let open = await BridgeDispatcher.dispatch(
      request("r7", "window.open", ["window": "settings", "section": "summaries"]), host: host)
    #expect(open == BridgeReply(id: "r7"))
    #expect(opened == [WindowParams(window: .settings, section: .summaries)])
  }
}
