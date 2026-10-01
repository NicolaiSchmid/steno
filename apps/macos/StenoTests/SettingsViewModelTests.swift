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
    XCTAssertEqual(model.retentionMode, .keepForever, "a fresh install keeps every recording")
    XCTAssertEqual(model.retentionDays, 30)
    XCTAssertTrue(model.footnote.hasPrefix("Recordings stay in the folder above"), model.footnote)
    XCTAssertEqual(
      AudioSettingsViewModel.RetentionMode.allCases,
      [.keepForever, .keepDays, .deleteAfterProcessing])

    await model.setInputDevice("mic-1")
    let folder = FileManager.default.temporaryDirectory.appendingPathComponent("rec")
    await model.setAudioFolder(folder)
    await model.setRetention(mode: .deleteAfterProcessing, days: 30)
    var settings = try await environment.settings.load()
    XCTAssertEqual(settings.inputDeviceUID, "mic-1")
    XCTAssertEqual(settings.audioFolder, folder)
    XCTAssertEqual(settings.defaultRetention, .deleteAfterProcessing)
    XCTAssertEqual(model.footnote, AudioRetention.deleteAfterProcessing.footnote)
    XCTAssertTrue(model.footnote.hasPrefix("Each recording is deleted as soon as"), model.footnote)

    await model.setRetention(mode: .keepDays, days: 0)
    settings = try await environment.settings.load()
    XCTAssertEqual(settings.defaultRetention, .keepDays(1), "days are clamped to at least one")
    XCTAssertTrue(model.footnote.contains("deleted 1 day after"), model.footnote)
    XCTAssertEqual(AudioSettingsViewModel.RetentionMode.keepDays.title(days: 1), "For 1 day")
    await model.setRetention(mode: .keepDays, days: 5000)
    settings = try await environment.settings.load()
    XCTAssertEqual(settings.defaultRetention, .keepDays(3650), "and to the stepper's maximum")
    await model.setRetention(mode: .keepDays, days: 7)
    settings = try await environment.settings.load()
    XCTAssertEqual(settings.defaultRetention, .keepDays(7))
    XCTAssertTrue(model.footnote.contains("deleted 7 days after"), model.footnote)
    XCTAssertEqual(AudioSettingsViewModel.RetentionMode.keepDays.title(days: 7), "For 7 days")
    let reloaded = AudioSettingsViewModel(environment: environment)
    reloaded.listInputs = { [] }
    await reloaded.load()
    XCTAssertEqual(reloaded.retentionMode, .keepDays)
    XCTAssertEqual(reloaded.retentionDays, 7)

    await model.setRetention(mode: .keepForever, days: 5)
    settings = try await environment.settings.load()
    XCTAssertEqual(settings.defaultRetention, .keepForever)
    XCTAssertEqual(model.keptForever, 0, "no recording on disk to keep")
    await model.setInputDevice(nil)
    settings = try await environment.settings.load()
    XCTAssertNil(settings.inputDeviceUID)
  }

  /// Switching to Forever keeps every recording whose master is on disk
  /// (rule and stamp together); a shorter rule changes nothing on disk or
  /// in the rows.
  func testAudioSwitchingToForeverKeepsRecordingsOnDisk() async throws {
    let environment = try await TestSupport.environment()
    try await environment.updateSettings { $0.defaultRetention = .keepDays(30) }
    let folder = try TestSupport.temporaryDirectory("steno-audio")
    defer { try? FileManager.default.removeItem(at: folder) }
    var meeting = SampleData.meeting()
    meeting.id = UUID()
    let master = folder.appendingPathComponent("master.caf")
    try Data([1, 2, 3]).write(to: master)
    let onDisk = AudioAsset(
      id: UUID(), meetingID: meeting.id, url: master, format: .caf48kFloat32, lanes: [.mixed],
      retention: .keepDays(30), expiresAt: TestSupport.now)
    try await environment.store.save(meeting, asset: onDisk)
    let model = AudioSettingsViewModel(environment: environment)
    model.listInputs = { [] }
    await model.load()
    XCTAssertEqual(model.retentionMode, .keepDays)

    await model.setRetention(mode: .keepDays, days: 7)
    XCTAssertNil(model.keptForever)
    let untouched = try await environment.store.asset(id: onDisk.id)
    XCTAssertEqual(untouched, onDisk, "a shorter rule applies to new recordings only")
    XCTAssertTrue(FileManager.default.fileExists(atPath: master.path))

    await model.setRetention(mode: .keepForever, days: 7)
    XCTAssertNil(model.error, model.error ?? "")
    XCTAssertEqual(model.keptForever, 1, "the seeded asset has no files and is not counted")
    let keptOptional = try await environment.store.asset(id: onDisk.id)
    let kept = try XCTUnwrap(keptOptional)
    XCTAssertEqual(kept.retention, .keepForever)
    XCTAssertNil(kept.expiresAt)
    let seededOptional = try await environment.store.asset(meetingID: SampleData.meetingID)
    let seeded = try XCTUnwrap(seededOptional)
    XCTAssertEqual(seeded.retention, .keepDays(30), "no file, no rewrite")
    XCTAssertNotNil(seeded.expiresAt)
    XCTAssertTrue(FileManager.default.fileExists(atPath: master.path), "nothing is deleted")
  }

  func testAudioMeasuresTheRecordingsFolder() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let folder = try TestSupport.temporaryDirectory("steno-usage")
    defer { try? FileManager.default.removeItem(at: folder) }
    try Data(repeating: 1, count: 1_500).write(to: folder.appendingPathComponent("a.caf"))
    let nested = folder.appendingPathComponent("meeting", isDirectory: true)
    try FileManager.default.createDirectory(at: nested, withIntermediateDirectories: true)
    try Data(repeating: 2, count: 500).write(to: nested.appendingPathComponent("mic.wav"))
    try Data(repeating: 3, count: 99).write(to: folder.appendingPathComponent(".DS_Store"))
    try await environment.updateSettings { $0.audioFolder = folder }
    let model = AudioSettingsViewModel(environment: environment)
    model.listInputs = { [] }
    XCTAssertEqual(model.folderUsage, .measuring)
    await model.load()
    XCTAssertEqual(model.folderUsage, .bytes(2_000))

    // A folder that does not exist yet holds no recordings.
    await model.setAudioFolder(folder.appendingPathComponent("not-yet", isDirectory: true))
    XCTAssertEqual(model.folderUsage, .bytes(0))
  }

  /// A folder that exists but cannot be read is "unavailable", not zero.
  func testAudioReportsAnUnreadableFolderAsUnavailable() async throws {
    // Root reads everything; the permission bits cannot make a folder
    // unreadable for it.
    try XCTSkipIf(getuid() == 0, "runs as root")
    let environment = try await TestSupport.environment(seed: false)
    let folder = try TestSupport.temporaryDirectory("steno-unreadable")
    defer {
      try? FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: folder.path)
      try? FileManager.default.removeItem(at: folder)
    }
    try Data(repeating: 1, count: 10).write(to: folder.appendingPathComponent("a.caf"))
    try FileManager.default.setAttributes([.posixPermissions: 0], ofItemAtPath: folder.path)
    try await environment.updateSettings { $0.audioFolder = folder }
    let model = AudioSettingsViewModel(environment: environment)
    model.listInputs = { [] }
    await model.load()
    XCTAssertEqual(model.folderUsage, .unavailable)
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
  }

  func testLLMTestReportsTheOutcome() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = LLMSettingsViewModel(environment: environment)
    await model.test()
    XCTAssertEqual(model.testResult, .failure("Enter a valid server address first."))
    model.baseURLText = "http://127.0.0.1:9/v1"
    model.model = "m"
    await model.test()
    guard case .failure(let message)? = model.testResult else {
      return XCTFail("an unreachable endpoint fails the test")
    }
    XCTAssertFalse(message.isEmpty)
  }

  func testLLMKeyNeverLandsInSettingsOrARenderedString() async throws {
    let key = "sk-live-9f8e7d6c5b4a-STENO-SECRET"
    let environment = try await TestSupport.environment(seed: false)
    let model = LLMSettingsViewModel(environment: environment)
    await model.load()
    model.baseURLText = "http://127.0.0.1:9/v1"
    model.model = "qwen"
    model.apiKey = key
    await model.save()
    XCTAssertNil(model.error, model.error ?? "")

    // Settings: neither the encoded value nor a reflection carries the key.
    let settings = try await environment.settings.load()
    let encoded = String(decoding: try JSONEncoder().encode(settings), as: UTF8.self)
    XCTAssertFalse(encoded.contains(key), "the key is not a Settings field")
    XCTAssertFalse(String(reflecting: settings).contains(key))
    XCTAssertFalse(encoded.contains("sk-"), encoded)
    let stored = try await environment.secrets.secret(for: .llmAPIKey)
    XCTAssertEqual(stored, key, "it lives in the secret store")

    // Rendered strings: the Test button's failure text, the probe's error
    // and everything the pane shows.
    await model.test()
    guard case .failure(let message)? = model.testResult else {
      return XCTFail("port 9 does not answer")
    }
    XCTAssertFalse(message.isEmpty)
    for text in [model.error, model.validationMessage, message].compactMap({ $0 }) {
      XCTAssertFalse(text.contains(key), text)
    }
    do {
      _ = try await LLMWiring.probe(
        settings: settings, apiKey: key, codexCredentials: environment.codexCredentials)
      XCTFail("port 9 does not answer")
    } catch {
      XCTAssertFalse(String(describing: error).contains(key))
      XCTAssertFalse(String(reflecting: error).contains(key))
      XCTAssertFalse(error.localizedDescription.contains(key))
    }

    // A second pane reads the key back from the secret store, not from Settings.
    let reloaded = LLMSettingsViewModel(environment: environment)
    await reloaded.load()
    XCTAssertEqual(reloaded.apiKey, key)
    XCTAssertTrue(reloaded.isConfigured)
  }

  func testLLMInvalidInputSavesNothing() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = LLMSettingsViewModel(environment: environment)
    await model.load()
    let before = try await environment.settings.load()
    let pipeline = environment.pipeline
    model.baseURLText = "ftp://nope"
    model.model = "m"
    model.apiKey = "sk-should-not-be-stored"
    await model.save()
    XCTAssertEqual(model.error, model.validationMessage)
    XCTAssertFalse(model.isConfigured)
    let after = try await environment.settings.load()
    XCTAssertEqual(after, before, "an invalid pane changes nothing")
    let stored = try await environment.secrets.secret(for: .llmAPIKey)
    XCTAssertNil(stored, "the key is not written either")
    XCTAssertTrue(pipeline === environment.pipeline, "no pipeline reload")
  }

  // MARK: ChatGPT (Codex)

  /// Picking ChatGPT stores nothing and configures nothing until the consent
  /// button; the sign-in file is looked at only for the account line.
  func testCodexPresetIsOffUntilConfirmed() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = LLMSettingsViewModel(environment: environment)
    await model.load()
    let before = try await environment.settings.load()
    let pipeline = environment.pipeline

    await model.selectPreset(.codex)
    XCTAssertEqual(model.preset, .codex)
    XCTAssertFalse(model.codexConfirmed)
    XCTAssertFalse(model.isConfigured)
    XCTAssertNil(model.validationMessage, "the endpoint fields do not apply")
    guard case .unavailable(let text) = model.codexStatus else {
      return XCTFail("no sign-in on a test machine: \(model.codexStatus)")
    }
    XCTAssertTrue(text.contains("codex login"), text)
    let after = try await environment.settings.load()
    XCTAssertEqual(after, before, "showing the card writes nothing")
    XCTAssertTrue(pipeline === environment.pipeline, "no pipeline reload")

    await model.test()
    XCTAssertEqual(model.testResult, .failure("Confirm the use of your ChatGPT account first."))

    // Back to a server: the endpoint fields are as they were.
    await model.selectPreset(.lmStudio)
    XCTAssertEqual(model.preset, .lmStudio)
    XCTAssertEqual(model.baseURLText, "http://127.0.0.1:1234/v1")
  }

  /// The pane closing (or focus leaving) with the consent card on screen
  /// commits like any other edit; that commit must store nothing, or the
  /// working endpoint would be switched off without a word.
  func testClosingThePaneWithTheConsentCardOnScreenStoresNothing() async throws {
    let environment = try await TestSupport.environment(seed: false)
    try await environment.updateSettings {
      $0.llmBaseURL = URL(string: "http://127.0.0.1:9/v1")
      $0.llmModel = "qwen"
    }
    let model = LLMSettingsViewModel(environment: environment)
    await model.load()
    XCTAssertTrue(model.isConfigured)
    let before = try await environment.settings.load()
    let pipeline = environment.pipeline
    await model.selectPreset(.codex)
    await model.commit()
    let after = try await environment.settings.load()
    XCTAssertEqual(after, before, "the card on screen is not a decision")
    XCTAssertEqual(after.llmProvider, .endpoint)
    XCTAssertTrue(after.llmConfigured, "the server setup keeps working")
    XCTAssertTrue(pipeline === environment.pipeline)
  }

  /// Confirming without a sign-in on this Mac stores the confirmation and
  /// the provider (the user said yes) but the endpoint stays off until a
  /// model can be picked, and the failure names the fix.
  func testConfirmingCodexWithoutASignInStaysUnconfigured() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = LLMSettingsViewModel(environment: environment)
    await model.load()
    await model.selectPreset(.codex)
    await model.confirmCodex()
    XCTAssertTrue(model.codexConfirmed)
    XCTAssertFalse(model.isConfigured, "no model list without a sign-in, so no model")
    let settings = try await environment.settings.load()
    XCTAssertEqual(settings.llmProvider, .codex)
    XCTAssertNotNil(settings.codexConfirmedAt)
    XCTAssertNil(settings.codexModel)
    XCTAssertFalse(settings.llmConfigured)
    guard case .unavailable(let text) = model.codexStatus else {
      return XCTFail("expected the missing sign-in to be reported: \(model.codexStatus)")
    }
    XCTAssertTrue(text.contains("codex login"), text)

    await model.stopUsingCodex()
    XCTAssertFalse(model.codexConfirmed)
    XCTAssertEqual(model.preset, .lmStudio)
    let reverted = try await environment.settings.load()
    XCTAssertEqual(reverted.llmProvider, .endpoint)
    XCTAssertNil(reverted.codexConfirmedAt)
  }

  /// A stored Codex configuration loads as such and switching the provider
  /// back and forth keeps both sides' fields.
  func testCodexConfigurationRoundTripsAndKeepsTheEndpointFields() async throws {
    let environment = try await TestSupport.environment(seed: false)
    try await environment.updateSettings {
      // Port 9 refuses, so the commit's probe fails fast without a network.
      $0.llmBaseURL = URL(string: "http://127.0.0.1:9/v1")
      $0.llmModel = "qwen"
      $0.llmProvider = .codex
      $0.codexModel = "gpt-5.6-terra"
      $0.codexContextTokens = 272_000
      $0.codexConfirmedAt = Date()
    }
    let model = LLMSettingsViewModel(environment: environment)
    await model.load()
    XCTAssertEqual(model.preset, .codex)
    XCTAssertTrue(model.codexConfirmed)
    XCTAssertTrue(model.isConfigured)
    XCTAssertEqual(model.codexModel, "gpt-5.6-terra")
    XCTAssertEqual(
      model.codexModelChoices.map(\.slug), ["gpt-5.6-terra"], "the stored slug stays pickable")
    XCTAssertEqual(model.baseURLText, "http://127.0.0.1:9/v1", "the endpoint side is kept")
    XCTAssertEqual(model.model, "qwen")
    guard case .unavailable = model.codexStatus else {
      return XCTFail("no sign-in on a test machine: \(model.codexStatus)")
    }

    await model.selectPreset(.custom)
    let switched = try await environment.settings.load()
    XCTAssertEqual(switched.llmProvider, .endpoint)
    XCTAssertEqual(switched.llmModel, "qwen")
    XCTAssertEqual(switched.codexModel, "gpt-5.6-terra", "the Codex side is kept too")
    XCTAssertNotNil(switched.codexConfirmedAt, "switching the service is not a revocation")
    XCTAssertTrue(switched.llmConfigured)
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

  func testObsidianDisabledSavesNilWithoutValidating() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let vault = try TestSupport.temporaryDirectory("steno-vault")
    defer { try? FileManager.default.removeItem(at: vault) }
    try await environment.updateSettings {
      $0.obsidian = ObsidianSettings(
        vaultPath: vault.path, peopleFolder: "People", includeAudio: false, taskTag: nil)
    }
    let model = ObsidianSettingsViewModel(environment: environment)
    await model.load()
    XCTAssertTrue(model.enabled)
    XCTAssertEqual(model.vaultPath, vault.path)
    XCTAssertEqual(model.peopleFolder, "People")
    XCTAssertEqual(model.draft?.peopleFolder, "People")

    model.enabled = false
    model.vaultPath = "/definitely/not/a/vault"
    await model.save()
    XCTAssertNil(model.validationMessage, "a disabled destination is not validated")
    XCTAssertTrue(model.saved)
    let after = try await environment.settings.load()
    XCTAssertNil(after.obsidian)
  }

  // MARK: Phones

  func testPhonesWithoutAHandoverServiceIsUnavailable() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = PhonesSettingsViewModel(environment: environment)
    XCTAssertFalse(model.isAvailable)
    await model.beginPairing()
    XCTAssertNil(model.pairing)
    XCTAssertNil(model.qrPNG)
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
    let clock = ManualClock()
    let model = PhonesSettingsViewModel(
      handover: handover, now: { TestSupport.now }, clock: clock)
    let observing = Task { await model.observe() }
    let observingReceipts = Task { await model.observeReceipts() }
    defer {
      observing.cancel()
      observingReceipts.cancel()
    }
    XCTAssertTrue(model.isAvailable)
    XCTAssertEqual(model.macID, identity.macID.uuidString)
    await model.load()
    XCTAssertTrue(model.devices.isEmpty)

    await model.beginPairing()
    XCTAssertNil(model.error, model.error ?? "")
    let payload = try XCTUnwrap(model.pairing)
    XCTAssertTrue(payload.urlString.hasPrefix("steno://"), payload.urlString)
    XCTAssertNotNil(model.qrPNG)
    XCTAssertTrue(model.pairingIsOpen)
    await TestSupport.waitUntil("listening") {
      if case .listening = model.listener { return true }
      return false
    }

    // The pairing poll runs on the injected clock: the phone shows up in
    // the store, the next tick closes the code, and the poll ends with it.
    let polling = Task { await model.observePairing() }
    _ = await clock.waitForSleepers(1)
    let device = PairedDevice(id: UUID(), name: "iPhone", pairedAt: TestSupport.now)
    try await store.save(device, tokenHash: Data(repeating: 1, count: 32))
    XCTAssertNotNil(model.pairing, "nothing before the tick")
    clock.advance(by: PhonesSettingsViewModel.pairingPoll)
    await TestSupport.waitUntil("a new device closes the pairing code") { model.pairing == nil }
    XCTAssertEqual(model.devices.map(\.name), ["iPhone"])
    await polling.value
    XCTAssertEqual(clock.pendingSleepers, 0, "the poll stopped with the code")

    await model.revoke(device.id)
    XCTAssertTrue(model.devices.isEmpty)
    await TestSupport.waitUntil("stopped after the last phone left") {
      model.listener == .stopped
    }
  }

  // MARK: Save-on-change, presets, permissions, overview

  func testLLMCommitSavesOnlyChangesThenProbes() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = LLMSettingsViewModel(environment: environment)
    await model.load()
    XCTAssertEqual(model.preset, .lmStudio, "a fresh install starts with the local preset")
    XCTAssertEqual(model.baseURLText, "http://127.0.0.1:1234/v1", "the address is pre-filled")
    XCTAssertEqual(model.status, .notConfigured)

    // Custom server on a port that refuses, so the probe answers at once.
    await model.selectPreset(.custom)
    XCTAssertEqual(model.baseURLText, "http://127.0.0.1:1234/v1", "Custom keeps what is typed")
    model.baseURLText = "http://127.0.0.1:9/v1"
    await model.commit()
    var settings = try await environment.settings.load()
    XCTAssertEqual(settings.llmBaseURL?.absoluteString, "http://127.0.0.1:9/v1")
    XCTAssertNil(settings.llmModel)
    XCTAssertFalse(model.isConfigured)
    XCTAssertNil(model.testResult, "no probe before the model is named")

    model.model = "qwen"
    await model.commit()
    settings = try await environment.settings.load()
    XCTAssertEqual(settings.llmModel, "qwen")
    XCTAssertTrue(model.isConfigured)
    guard case .failed(let report) = model.status else {
      return XCTFail("port 9 does not answer, so the commit's probe fails: \(model.status)")
    }
    XCTAssertFalse(report.isEmpty)

    let pipeline = environment.pipeline
    await model.commit()
    XCTAssertTrue(pipeline === environment.pipeline, "an unchanged form saves nothing")

    model.contextTokensText = "12"
    await model.commit()
    XCTAssertNotNil(model.validationMessage)
    XCTAssertNil(model.error, "invalid input is shown, not saved as an error")
    settings = try await environment.settings.load()
    XCTAssertEqual(settings.llmContextTokens, LLMSettingsViewModel.defaultContextTokens)
  }

  func testLLMSelectPresetFillsAndStoresTheAddress() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = LLMSettingsViewModel(environment: environment)
    await model.load()
    await model.selectPreset(.openAI)
    XCTAssertEqual(model.preset, .openAI)
    XCTAssertEqual(model.baseURLText, "https://api.openai.com/v1")
    let settings = try await environment.settings.load()
    XCTAssertEqual(settings.llmBaseURL?.absoluteString, "https://api.openai.com/v1")
    XCTAssertFalse(model.isConfigured, "no model yet")

    let reloaded = LLMSettingsViewModel(environment: environment)
    await reloaded.load()
    XCTAssertEqual(reloaded.preset, .openAI, "the preset is inferred from the stored address")
  }

  func testObsidianCommitWaitsForAVaultAndSavesOnChoice() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let vault = try TestSupport.temporaryDirectory("steno-vault")
    defer { try? FileManager.default.removeItem(at: vault) }
    let model = ObsidianSettingsViewModel(environment: environment)
    await model.load()
    XCTAssertFalse(model.enabled)
    XCTAssertEqual(model.vaultName, "")

    await model.setEnabled(true)
    XCTAssertTrue(model.needsVault)
    XCTAssertNil(model.validationMessage, "on without a folder is not an error")
    XCTAssertFalse(model.saved)
    var stored = try await environment.settings.load().obsidian
    XCTAssertNil(stored)

    await model.chooseVault(vault)
    XCTAssertTrue(model.saved)
    XCTAssertNil(model.validationMessage)
    XCTAssertEqual(model.vaultName, vault.lastPathComponent)
    stored = try await environment.settings.load().obsidian
    XCTAssertEqual(stored?.vaultPath, vault.path)

    model.saved = false
    await model.commit()
    XCTAssertFalse(model.saved, "an unchanged form saves nothing")

    await model.setIncludeAudio(true)
    XCTAssertTrue(model.saved)
    stored = try await environment.settings.load().obsidian
    XCTAssertEqual(stored?.includeAudio, true)

    await model.setEnabled(false)
    stored = try await environment.settings.load().obsidian
    XCTAssertNil(stored)
  }

  func testAudioShowsPermissionsAndFolderSize() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = AudioSettingsViewModel(environment: environment)
    model.listInputs = { [] }
    model.measureFolder = { _ in 4_200 }
    await model.load()
    XCTAssertEqual(model.state(of: .microphone), .granted)
    XCTAssertEqual(model.state(of: .systemAudio), .granted)
    XCTAssertTrue(model.allPermissionsGranted)
    XCTAssertEqual(model.folderUsage, .bytes(4_200))
    XCTAssertEqual(model.folderName, "audio")
    XCTAssertTrue(model.footnote.hasPrefix("Recordings stay in the folder above"), model.footnote)

    let permissions = try XCTUnwrap(environment.permissions as? FakePermissions)
    permissions.states[.systemAudio] = .unknown
    await model.refreshPermissions()
    XCTAssertFalse(model.allPermissionsGranted)
    await model.requestPermission(.systemAudio)
    XCTAssertEqual(permissions.requests, [.systemAudio])
    XCTAssertEqual(model.state(of: .systemAudio), .granted)
    model.openPermissionSettings(.microphone)
    XCTAssertEqual(permissions.openedPanes, [.microphone])

    model.measureFolder = { _ in throw CocoaError(.fileReadNoPermission) }
    await model.measureFolderUsage()
    XCTAssertEqual(model.folderUsage, .unavailable)
    XCTAssertNil(model.error)
  }

  func testGeneralShowsCalendarAndUpdates() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = GeneralSettingsViewModel(environment: environment)
    await model.load()
    XCTAssertEqual(model.calendarPermission, .granted)
    XCTAssertEqual(model.selectedTemplate?.id, "default")

    let permissions = try XCTUnwrap(environment.permissions as? FakePermissions)
    permissions.states[.calendar] = .unknown
    await model.load()
    XCTAssertEqual(model.calendarPermission, .unknown)
    await model.requestCalendar()
    XCTAssertEqual(permissions.requests, [.calendar])
    XCTAssertEqual(model.calendarPermission, .granted)

    let updater = try XCTUnwrap(environment.updater as? FakeUpdater)
    XCTAssertEqual(model.updateStatusText(now: TestSupport.now), "Not checked yet")
    updater.lastOutcome = .available("0.9.1")
    XCTAssertEqual(model.updateStatusText(now: TestSupport.now), "Update available: 0.9.1")
    updater.lastOutcome = .failed("SUSparkleErrorDomain 2001")
    XCTAssertEqual(model.updateStatusText(now: TestSupport.now), "Could not check for updates")
    XCTAssertEqual(model.updateFailureDetails, "SUSparkleErrorDomain 2001")
    model.checkForUpdates()
    XCTAssertEqual(updater.checks, 1)
    model.automaticallyDownloadsUpdates = true
    XCTAssertTrue(updater.automaticallyDownloadsUpdates)
    model.automaticallyChecksForUpdates = false
    XCTAssertFalse(updater.automaticallyChecksForUpdates)
  }

  func testErrorsAreSentencesWithDetailsApart() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = AudioSettingsViewModel(environment: environment)
    struct Boom: Error, CustomStringConvertible {
      var description: String { "CoreAudio kAudioHardwareUnknownPropertyError" }
    }
    model.listInputs = { throw Boom() }
    model.refreshDevices()
    XCTAssertEqual(model.error, "Microphones could not be listed.")
    XCTAssertEqual(model.errorDetails, "CoreAudio kAudioHardwareUnknownPropertyError")
  }
}
