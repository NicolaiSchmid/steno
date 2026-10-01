import Foundation
import StenoAdapters
import StenoCore
import StenoHandover
import StenoSpeech
import XCTest

/// What the Settings redesign promises beyond the happy paths in
/// `SettingsViewModelTests`: `commit()` neither saves nor probes over
/// invalid input, Custom keeps the typed address, the API key stays out of
/// every rendered string, a rejected vault leaves the stored one alone, the
/// sidebar counts paired phones from the store, the pairing sheet's copy
/// follows the injected clock, a folder change drops a stale measurement,
/// and the Transcription rows read in words.
@MainActor
final class SettingsRedesignTests: XCTestCase {
  // MARK: Summaries

  func testLLMCommitOverInvalidInputSavesNothingAndDoesNotProbe() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = LLMSettingsViewModel(environment: environment)
    await model.load()
    let before = try await environment.settings.load()
    let pipeline = environment.pipeline

    model.model = "qwen"
    model.apiKey = "sk-typed-early"
    model.baseURLText = "ftp://nope"
    await model.commit()
    XCTAssertNotNil(model.validationMessage)
    XCTAssertNil(model.error, "invalid input is shown inline, not as an error")
    XCTAssertNil(model.testResult, "nothing is probed")
    XCTAssertEqual(model.status, .notConfigured)
    XCTAssertFalse(model.isConfigured)
    let after = try await environment.settings.load()
    XCTAssertEqual(after, before, "nothing is stored")
    let rejectedKey = try await environment.secrets.secret(for: .llmAPIKey)
    XCTAssertNil(rejectedKey, "the key waits with the rest of the form")
    XCTAssertTrue(pipeline === environment.pipeline, "no pipeline reload")

    // A valid address without a model is stored, key included, but a
    // half-configured endpoint is never probed.
    model.baseURLText = "http://127.0.0.1:9/v1"
    model.model = ""
    await model.commit()
    XCTAssertNil(model.validationMessage)
    XCTAssertNil(model.error, model.error ?? "")
    XCTAssertFalse(model.isConfigured)
    XCTAssertNil(model.testResult, "no probe while the model is unnamed")
    XCTAssertEqual(model.status, .notConfigured)
    let stored = try await environment.settings.load()
    XCTAssertEqual(stored.llmBaseURL?.absoluteString, "http://127.0.0.1:9/v1")
    XCTAssertNil(stored.llmModel)
    let storedKey = try await environment.secrets.secret(for: .llmAPIKey)
    XCTAssertEqual(storedKey, "sk-typed-early")
  }

  func testCustomPresetKeepsTheTypedAddressAndClearsTheLastResult() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = LLMSettingsViewModel(environment: environment)
    await model.load()

    model.baseURLText = "http://127.0.0.1:9/v1"
    model.model = "qwen"
    await model.selectPreset(.custom)
    XCTAssertEqual(model.preset, .custom)
    XCTAssertEqual(model.baseURLText, "http://127.0.0.1:9/v1", "Custom keeps what is typed")
    var settings = try await environment.settings.load()
    XCTAssertEqual(settings.llmBaseURL?.absoluteString, "http://127.0.0.1:9/v1")
    guard case .failed = model.status else {
      return XCTFail("the commit probes a configured endpoint: \(model.status)")
    }

    // Choosing Custom again saves nothing (the form is unchanged) and
    // clears the stale result, so the status row does not keep an old
    // failure next to an edited address.
    let pipeline = environment.pipeline
    await model.selectPreset(.custom)
    XCTAssertEqual(model.status, .unchecked)
    XCTAssertTrue(pipeline === environment.pipeline, "an unchanged form saves nothing")

    // A preset with an address fills the field; Custom afterwards keeps it.
    model.model = ""
    await model.selectPreset(.ollama)
    XCTAssertEqual(model.baseURLText, "http://127.0.0.1:11434/v1")
    await model.selectPreset(.custom)
    XCTAssertEqual(
      model.baseURLText, "http://127.0.0.1:11434/v1", "Custom never rewrites the field")
    settings = try await environment.settings.load()
    XCTAssertEqual(settings.llmBaseURL?.absoluteString, "http://127.0.0.1:11434/v1")
    XCTAssertNil(model.testResult, "no probe without a model")

    let reloaded = LLMSettingsViewModel(environment: environment)
    await reloaded.load()
    XCTAssertEqual(
      reloaded.preset, .ollama, "the preset is inferred from the address, not remembered")
  }

  func testAPIKeyStaysOutOfTheStatusRowAndTheSidebar() async throws {
    let key = "sk-live-0123456789abcdef-STENO-SECRET"
    let environment = try await TestSupport.environment(seed: false)
    let model = LLMSettingsViewModel(environment: environment)
    await model.load()
    model.baseURLText = "http://127.0.0.1:9/v1"
    model.model = "qwen"
    model.apiKey = key
    await model.commit()
    guard case .failed(let report) = model.status else {
      return XCTFail("port 9 refuses, so the commit's probe fails: \(model.status)")
    }
    XCTAssertFalse(report.isEmpty)
    let rendered: [String?] = [
      report, model.error, model.errorDetails, model.validationMessage,
      String(reflecting: model.status), model.preset.title, model.preset.modelPlaceholder,
    ]
    for text in rendered.compactMap({ $0 }) {
      XCTAssertFalse(text.contains(key), text)
    }

    let overview = SettingsOverviewViewModel(environment: environment)
    await overview.refresh()
    XCTAssertEqual(overview.subtitles[.summaries], "qwen", "a custom server shows the model")
    for (section, subtitle) in overview.subtitles {
      XCTAssertFalse(subtitle.contains(key), "\(section.title): \(subtitle)")
    }
    let stored = try await environment.secrets.secret(for: .llmAPIKey)
    XCTAssertEqual(stored, key, "the key went to the secret store")
  }

  // MARK: Export

  func testObsidianRejectedVaultLeavesTheStoredOneAlone() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let vault = try TestSupport.temporaryDirectory("steno-vault")
    let other = try TestSupport.temporaryDirectory("steno-vault-other")
    defer {
      try? FileManager.default.removeItem(at: vault)
      try? FileManager.default.removeItem(at: other)
    }
    try await environment.updateSettings { $0.obsidian = ObsidianSettings(vaultPath: vault.path) }
    let model = ObsidianSettingsViewModel(environment: environment)
    await model.load()
    XCTAssertTrue(model.enabled)
    XCTAssertEqual(model.vaultName, vault.lastPathComponent)

    let missing = other.appendingPathComponent("gone", isDirectory: true)
    await model.chooseVault(missing)
    XCTAssertEqual(
      model.validationMessage, ObsidianError.vaultMissing(model.vaultPath).description)
    XCTAssertFalse(model.saved)
    XCTAssertNil(model.error, "a rejected folder is a validation message, not an error")
    XCTAssertEqual(model.vaultName, "gone", "the form shows what was chosen")
    var stored = try await environment.settings.load().obsidian
    XCTAssertEqual(stored?.vaultPath, vault.path, "the stored vault is untouched")

    await model.chooseVault(other)
    XCTAssertTrue(model.saved)
    XCTAssertNil(model.validationMessage)
    stored = try await environment.settings.load().obsidian
    XCTAssertEqual(stored?.vaultPath, other.path)
  }

  // MARK: Sidebar

  func testOverviewCountsPairedPhonesAndReadsTheStore() async throws {
    let store = try MeetingStore.inMemory()
    let inbox = try TestSupport.temporaryDirectory("steno-inbox")
    defer { try? FileManager.default.removeItem(at: inbox) }
    let identity = try TestSupport.testIdentity()
    let handover = HandoverService(
      configuration: HandoverConfiguration(
        serviceName: "Test Mac", advertise: false, inboxDirectory: inbox),
      store: store, intake: FakeHandoverIntake(), identity: identity, now: { TestSupport.now })
    let environment = try await TestSupport.environment(seed: false, handover: handover)
    let overview = SettingsOverviewViewModel(environment: environment)
    await overview.refresh()
    XCTAssertEqual(overview.subtitles[.iphone], "No iPhone paired")
    XCTAssertEqual(overview.subtitles[.transcription], "Download needed")
    XCTAssertEqual(overview.subtitles[.recording], "Ready")

    try await store.save(
      PairedDevice(id: UUID(), name: "iPhone", pairedAt: TestSupport.now),
      tokenHash: Data(repeating: 1, count: 32))
    await overview.refresh()
    XCTAssertEqual(overview.subtitles[.iphone], "1 iPhone paired")

    try await store.save(
      PairedDevice(id: UUID(), name: "iPad", pairedAt: TestSupport.now),
      tokenHash: Data(repeating: 2, count: 32))
    try await environment.updateSettings {
      $0.llmBaseURL = URL(string: "https://api.openai.com/v1")
      $0.llmModel = "gpt-4.1-mini"
      $0.obsidian = ObsidianSettings(vaultPath: "/Users/me/Notes/Work Vault")
    }
    try await environment.models.ensureInstalled(.parakeetV3)
    try await environment.models.ensureInstalled(.offlineDiarizer)
    let permissions = try XCTUnwrap(environment.permissions as? FakePermissions)
    permissions.states[.systemAudio] = .denied
    let updater = try XCTUnwrap(environment.updater as? FakeUpdater)
    updater.lastOutcome = .available("0.9.1")
    await overview.refresh()
    XCTAssertEqual(overview.subtitles[.iphone], "2 iPhones paired")
    XCTAssertEqual(overview.subtitles[.summaries], "OpenAI")
    XCTAssertEqual(overview.subtitles[.export], "Work Vault")
    XCTAssertEqual(overview.subtitles[.transcription], "Ready")
    XCTAssertEqual(overview.subtitles[.recording], "Permission needed")
    XCTAssertEqual(overview.subtitles[.general], "Update available: 0.9.1")
  }

  // MARK: iPhone

  func testPhonesPairingWindowFollowsTheClock() async throws {
    let store = try MeetingStore.inMemory()
    let inbox = try TestSupport.temporaryDirectory("steno-inbox")
    defer { try? FileManager.default.removeItem(at: inbox) }
    let now = MutableNow(TestSupport.now)
    let identity = try TestSupport.testIdentity()
    let handover = HandoverService(
      configuration: HandoverConfiguration(
        serviceName: "Test Mac", advertise: false, inboxDirectory: inbox,
        pairingWindow: .seconds(240)),
      store: store, intake: FakeHandoverIntake(), identity: identity, now: { now.date })
    let model = PhonesSettingsViewModel(
      handover: handover, now: { now.date }, clock: ManualClock())
    XCTAssertFalse(model.pairingIsOpen, "no code, no window")

    await model.beginPairing()
    XCTAssertNil(model.error, model.error ?? "")
    XCTAssertNotNil(model.qrPNGBase64, "the code is encoded once for the page")
    XCTAssertTrue(model.pairingIsOpen)
    now.advance(by: 270)
    XCTAssertFalse(model.pairingIsOpen, "past the window the code is closed")
    await model.cancelPairing()
    XCTAssertNil(model.qrPNGBase64)
    XCTAssertEqual(handover.state, .stopped, "no phone paired, so the listener stops")
  }

  // MARK: Recording

  func testAudioFolderChangeDropsAStaleMeasurement() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = AudioSettingsViewModel(environment: environment)
    model.listInputs = { [] }
    model.measureFolder = { _ in 4_200 }
    await model.load()
    XCTAssertEqual(model.folderUsage, .bytes(4_200))
    let original = model.audioFolder
    let replacement = try TestSupport.temporaryDirectory("steno-recordings")
    defer { try? FileManager.default.removeItem(at: replacement) }

    // The old folder's measurement is held open (off the main actor) until
    // the test releases it, so it lands after the folder has changed.
    let latch = Latch()
    model.measureFolder = { folder in
      guard folder == original else { return 7 }
      latch.wait()
      return 4_200
    }
    let stale = Task { await model.measureFolderUsage() }
    await TestSupport.settle()
    XCTAssertEqual(model.folderUsage, .measuring)

    await model.setAudioFolder(replacement)
    XCTAssertEqual(model.folderUsage, .bytes(7), "the new folder is measured right away")
    XCTAssertEqual(model.folderName, replacement.lastPathComponent)
    latch.open()
    await stale.value
    XCTAssertEqual(model.folderUsage, .bytes(7), "the old folder's late result is dropped")
    let settings = try await environment.settings.load()
    XCTAssertEqual(settings.audioFolder, replacement)
    XCTAssertNil(model.error)
  }

  // MARK: Transcription

  func testTranscriptionStatusReadsInWords() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = SpeechSettingsViewModel(environment: environment)
    await model.load()
    let size = ByteCountFormatter.string(
      fromByteCount: ModelAsset.offlineDiarizer.approximateBytes, countStyle: .file)
    XCTAssertEqual(model.statusText(of: .offlineDiarizer), "Not downloaded · \(size)")

    model.download(.offlineDiarizer)
    XCTAssertEqual(
      model.statusText(of: .offlineDiarizer), "Downloading…", "no percentage before the first byte")
    await TestSupport.waitUntil("installed") {
      if case .installed = model.state(of: .offlineDiarizer) { return true }
      return false
    }
    let installed = model.statusText(of: .offlineDiarizer)
    XCTAssertTrue(installed.hasPrefix("Installed · "), installed)
    XCTAssertFalse(model.allInstalled, "the speech model is still absent")

    for asset in ModelAsset.allCases {
      let text = model.statusText(of: asset)
      XCTAssertFalse(text.contains("/"), "\(text) looks like a repository")
      for word in ["CC-BY", "MIT", "Apache", "pyannote", "FluidInference", "argmaxinc", "coreml"] {
        XCTAssertFalse(text.contains(word), text)
      }
    }
  }
}

/// A `now` the test moves. `Sendable` so the handover service and the view
/// model read the same instant from any isolation.
private final class MutableNow: @unchecked Sendable {
  private let lock = NSLock()
  private var current: Date

  init(_ date: Date) {
    current = date
  }

  var date: Date {
    lock.withLock { current }
  }

  func advance(by seconds: TimeInterval) {
    lock.withLock { current = current.addingTimeInterval(seconds) }
  }
}

/// Blocks `wait()` callers until `open()`; used off the main actor only.
private final class Latch: @unchecked Sendable {
  private let semaphore = DispatchSemaphore(value: 0)

  func wait() {
    semaphore.wait()
  }

  func open() {
    semaphore.signal()
  }
}
