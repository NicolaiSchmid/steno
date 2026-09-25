import StenoAdapters
import StenoAudio
import StenoCore
import StenoHandover
import StenoSpeech
import XCTest

@MainActor
final class SettingsViewModelTests: XCTestCase {
  // MARK: General

  func testGeneralSavesTemplateDetectionAndLoginItem() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = GeneralSettingsViewModel(environment: environment)
    await model.load()
    XCTAssertEqual(model.defaultTemplateID, "default")
    XCTAssertTrue(model.detectionEnabled)
    XCTAssertFalse(model.launchAtLogin)

    await model.setDefaultTemplate("daily-standup")
    await model.setDefaultTemplate("nope")
    await model.setDetectionEnabled(false)
    await model.setLaunchAtLogin(true)
    let settings = try await environment.settings.load()
    XCTAssertEqual(settings.defaultTemplateID, "daily-standup")
    XCTAssertFalse(settings.meetingDetectionEnabled)
    XCTAssertTrue(settings.launchAtLogin)
    XCTAssertEqual(model.loginItem, .enabled)
    XCTAssertNil(model.error)
  }

  // MARK: Audio

  func testAudioSavesDeviceFolderAndRetention() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = AudioSettingsViewModel(environment: environment)
    // `AudioDeviceInfo` has no public initialiser; the list comes from Core
    // Audio (possibly empty on a headless runner) and must not fail.
    model.listInputs = { [] }
    await model.load()
    XCTAssertNil(model.error)
    XCTAssertEqual(model.retentionMode, .keepDays)
    XCTAssertEqual(model.retentionDays, 30)

    await model.setInputDevice("mic-1")
    let folder = FileManager.default.temporaryDirectory.appendingPathComponent("rec")
    await model.setAudioFolder(folder)
    await model.setRetention(mode: .deleteAfterProcessing, days: 30)
    var settings = try await environment.settings.load()
    XCTAssertEqual(settings.inputDeviceUID, "mic-1")
    XCTAssertEqual(settings.audioFolder, folder)
    XCTAssertEqual(settings.defaultRetention, .deleteAfterProcessing)

    await model.setRetention(mode: .keepDays, days: 0)
    settings = try await environment.settings.load()
    XCTAssertEqual(settings.defaultRetention, .keepDays(1), "days are clamped to at least one")
    await model.setRetention(mode: .keepForever, days: 5)
    settings = try await environment.settings.load()
    XCTAssertEqual(settings.defaultRetention, .keepForever)
    await model.setInputDevice(nil)
    settings = try await environment.settings.load()
    XCTAssertNil(settings.inputDeviceUID)
  }

  // MARK: Speech

  func testSpeechEngineChangeReloadsThePipeline() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = SpeechSettingsViewModel(environment: environment)
    await model.load()
    XCTAssertEqual(model.engineID, .parakeetV3)
    XCTAssertEqual(model.assets, [.parakeetV3, .offlineDiarizer])
    let before = environment.pipeline
    await model.setEngine(.whisperKitLargeV3Turbo)
    XCTAssertNil(model.error, model.error ?? "")
    XCTAssertEqual(model.assets, [.whisperLargeV3Turbo, .offlineDiarizer])
    let settings = try await environment.settings.load()
    XCTAssertEqual(settings.speechEngineID, "whisperkit-large-v3-turbo")
    XCTAssertFalse(before === environment.pipeline)
  }

  func testSpeechDownloadStreamsProgressToInstalled() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = SpeechSettingsViewModel(environment: environment)
    await model.load()
    XCTAssertEqual(model.state(of: .offlineDiarizer), .absent)
    model.download(.offlineDiarizer)
    await TestSupport.waitUntil("installed") {
      if case .installed = model.state(of: .offlineDiarizer) { return true }
      return false
    }
    XCTAssertTrue(environment.models.isInstalled(.offlineDiarizer))
    await model.remove(.offlineDiarizer)
    XCTAssertEqual(model.state(of: .offlineDiarizer), .absent)
    XCTAssertFalse(environment.models.isInstalled(.offlineDiarizer))
  }

  // MARK: LLM

  func testLLMValidatesAndSavesSettingsAndKey() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = LLMSettingsViewModel(environment: environment)
    await model.load()
    XCTAssertFalse(model.isConfigured)

    model.baseURLText = "not a url"
    XCTAssertNotNil(model.validationMessage)
    model.baseURLText = "http://127.0.0.1:1234/v1"
    model.contextTokensText = "12"
    XCTAssertNotNil(model.validationMessage)
    model.contextTokensText = "8000"
    XCTAssertNil(model.validationMessage)
    model.model = "qwen"
    model.apiKey = "sk-test"
    let before = environment.pipeline
    await model.save()
    XCTAssertNil(model.error, model.error ?? "")
    XCTAssertTrue(model.isConfigured)
    let settings = try await environment.settings.load()
    XCTAssertEqual(settings.llmBaseURL?.absoluteString, "http://127.0.0.1:1234/v1")
    XCTAssertEqual(settings.llmModel, "qwen")
    XCTAssertEqual(settings.llmContextTokens, 8000)
    let savedKey = try await environment.secrets.secret(for: .llmAPIKey)
    XCTAssertEqual(savedKey, "sk-test")
    XCTAssertFalse(before === environment.pipeline, "saving rebuilt the pipeline")

    model.apiKey = ""
    await model.save()
    let clearedKey = try await environment.secrets.secret(for: .llmAPIKey)
    XCTAssertNil(clearedKey, "an empty key removes it")
    XCTAssertFalse(model.savedKeyPresent)
  }

  func testLLMTestReportsTheOutcome() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = LLMSettingsViewModel(environment: environment)
    await model.test()
    XCTAssertEqual(model.testResult, .failure("Enter a valid base URL first."))
    model.baseURLText = "http://127.0.0.1:9/v1"
    model.model = "m"
    await model.test()
    guard case .failure(let message)? = model.testResult else {
      return XCTFail("an unreachable endpoint fails the test")
    }
    XCTAssertFalse(message.isEmpty)
  }

  // MARK: Obsidian

  func testObsidianValidationSurfacesTheDestinationMessageVerbatim() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = ObsidianSettingsViewModel(environment: environment)
    await model.load()
    XCTAssertFalse(model.enabled)
    model.enabled = true
    model.vaultPath = "/definitely/not/a/vault"
    await model.save()
    XCTAssertEqual(
      model.validationMessage, ObsidianError.vaultMissing("/definitely/not/a/vault").description)
    XCTAssertFalse(model.saved)
    let afterInvalid = try await environment.settings.load()
    XCTAssertNil(afterInvalid.obsidian)

    let vault = FileManager.default.temporaryDirectory
      .appendingPathComponent("steno-vault-\(UUID().uuidString)", isDirectory: true)
    try FileManager.default.createDirectory(at: vault, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: vault) }
    model.vaultPath = vault.path
    model.peopleFolder = "../People"
    await model.save()
    XCTAssertEqual(
      model.validationMessage, ObsidianError.peopleFolderInvalid("../People").description)

    model.peopleFolder = " People "
    model.taskTag = "task"
    model.includeAudio = true
    await model.save()
    XCTAssertNil(model.validationMessage)
    XCTAssertTrue(model.saved)
    let storedOptional = try await environment.settings.load().obsidian
    let stored = try XCTUnwrap(storedOptional)
    XCTAssertEqual(stored.vaultPath, vault.path)
    XCTAssertEqual(stored.peopleFolder, "People")
    XCTAssertEqual(stored.taskTag, "task")
    XCTAssertTrue(stored.includeAudio)

    model.enabled = false
    await model.save()
    let afterDisable = try await environment.settings.load()
    XCTAssertNil(afterDisable.obsidian)
  }

  // MARK: Phones

  func testPhonesWithoutAHandoverServiceIsUnavailable() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = PhonesSettingsViewModel(environment: environment)
    XCTAssertFalse(model.isAvailable)
    await model.beginPairing()
    XCTAssertNil(model.pairing)
    XCTAssertNil(model.qrImage)
  }

  func testPhonesPairingListsDevicesAndRevokes() async throws {
    let store = try MeetingStore.inMemory()
    let inbox = FileManager.default.temporaryDirectory
      .appendingPathComponent("steno-inbox-\(UUID().uuidString)", isDirectory: true)
    defer { try? FileManager.default.removeItem(at: inbox) }
    let identity = try TestSupport.testIdentity()
    let handover = HandoverService(
      configuration: HandoverConfiguration(
        serviceName: "Test Mac", advertise: false, inboxDirectory: inbox),
      store: store, intake: FakeHandoverIntake(meetingID: UUID()), identity: identity,
      now: { TestSupport.now })
    let model = PhonesSettingsViewModel(handover: handover, now: { TestSupport.now })
    XCTAssertTrue(model.isAvailable)
    XCTAssertEqual(model.macID, identity.macID.uuidString)
    await model.load()
    XCTAssertTrue(model.devices.isEmpty)

    await model.beginPairing()
    XCTAssertNil(model.error, model.error ?? "")
    let payload = try XCTUnwrap(model.pairing)
    XCTAssertTrue(payload.urlString.hasPrefix("steno://"), payload.urlString)
    XCTAssertNotNil(model.qrImage)
    XCTAssertTrue(model.pairingIsOpen)
    await TestSupport.waitUntil("listening") {
      if case .listening = model.listener { return true }
      return false
    }

    let device = PairedDevice(id: UUID(), name: "iPhone", pairedAt: TestSupport.now)
    try await store.save(device, tokenHash: Data(repeating: 1, count: 32))
    await model.refreshAfterPairing()
    XCTAssertEqual(model.devices.map(\.name), ["iPhone"])
    XCTAssertNil(model.pairing, "a new device closes the pairing code")

    await model.revoke(device.id)
    XCTAssertTrue(model.devices.isEmpty)
    await TestSupport.waitUntil("stopped after the last phone left") {
      model.listener == .stopped
    }
  }

  // MARK: Updates

  func testUpdatesBindsToTheUpdater() {
    let updater = FakeUpdater()
    let model = UpdatesSettingsViewModel(updater: updater)
    XCTAssertTrue(model.automaticallyChecks)
    model.automaticallyChecks = false
    XCTAssertFalse(updater.automaticallyChecksForUpdates)
    model.checkNow()
    XCTAssertEqual(updater.checks, 1)
    XCTAssertFalse(model.version.isEmpty)
  }
}
