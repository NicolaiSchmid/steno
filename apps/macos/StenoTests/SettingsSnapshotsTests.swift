import Foundation
import StenoBridge
import StenoCore
import StenoSpeech
import Testing

// The Settings window's snapshots over the preview environment, and the
// bridge's command routing, hostless: no web view anywhere, a recording sink
// where `WebBridge` would be and a stub where the folder panel would be. The
// view models' rules stay pinned by their own tests; here the wire shape the
// page renders, and that a command reaches its view model and comes back as
// a publish.

@MainActor
private final class RecordingSink: BridgeEventSink {
  private(set) var events: [BridgeEvent] = []

  func emit(_ event: BridgeEvent) {
    events.append(event)
  }

  func last(_ topic: BridgeTopic) -> JSONValue? {
    events.last { $0.topic == topic }?.payload
  }
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

// MARK: - Pure helpers

@Suite struct SettingsSnapshotHelperTests {
  @Test func receivedBytesCountWholeChunksAndNeverPassTheDeclaredSize() {
    let receipt = HandoverReceipt(
      recordingID: UUID(), deviceID: UUID(), state: .receiving, byteCount: 1_000,
      sha256: Data(count: 32), chunkSize: 250, receivedChunks: [0, 1], createdAt: .now,
      updatedAt: .now)
    #expect(SettingsSnapshots.receivedBytes(receipt) == 500)
    var full = receipt
    full.receivedChunks = [0, 1, 2, 3, 4]
    #expect(SettingsSnapshots.receivedBytes(full) == 1_000, "the last chunk is short")
  }

  @Test func permissionsAndLoginItemsMapByName() {
    for kind in PermissionKind.allCases {
      #expect(SettingsSnapshots.kind(kind).rawValue == kind.rawValue)
    }
    #expect(SettingsSnapshots.state(.granted) == .granted)
    #expect(SettingsSnapshots.state(.denied) == .denied)
    #expect(SettingsSnapshots.state(.unknown) == .unknown)
    #expect(SettingsSnapshots.loginItem(.requiresApproval) == .requiresApproval)
    #expect(SettingsSnapshots.loginItem(.notFound) == .notFound)
  }

  @Test func acknowledgementsListEverySpeechModelAndTheLibraries() {
    let acknowledgements = SettingsSnapshots.acknowledgements
    let models = acknowledgements.filter { $0.group == .speechModels }
    #expect(models.map(\.name) == ModelAsset.allCases.map(\.displayName))
    #expect(models.allSatisfy { !$0.licence.isEmpty && !$0.source.isEmpty })
    let libraries = acknowledgements.filter { $0.group == .libraries }
    #expect(libraries.contains { $0.name == "Sparkle" })
    #expect(libraries.allSatisfy { $0.source.hasPrefix("https://") })
  }
}

// MARK: - Snapshots over the preview environment

@Suite @MainActor struct SettingsSnapshotsTests {
  @Test func generalCarriesTemplatesPermissionsUpdatesAndAcknowledgements() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let general = GeneralSettingsViewModel(environment: environment)
    await general.load()
    let snapshot = GeneralSettingsSnapshot(general: general, subtitle: "Steno 0.0.0")
    #expect(snapshot.subtitle == "Steno 0.0.0")
    #expect(!snapshot.version.isEmpty)
    #expect(snapshot.detectionEnabled)
    #expect(snapshot.defaultTemplateID == SummaryTemplate.defaultID)
    #expect(snapshot.templates.map(\.id) == SummaryTemplate.bundledIDs)
    #expect(snapshot.templates.allSatisfy { !$0.name.isEmpty && !$0.description.isEmpty })
    #expect(snapshot.calendarPermission == .granted, "the preview grants everything")
    #expect(!snapshot.requestingCalendar)
    #expect(snapshot.updates.canCheck)
    #expect(snapshot.updates.outcome == .notChecked)
    #expect(snapshot.updates.detail == nil)
    #expect(snapshot.acknowledgements == SettingsSnapshots.acknowledgements)
    #expect(snapshot.error == nil)

    let updater = try #require(environment.updater as? FakeUpdater)
    updater.lastOutcome = .available("0.9.1")
    let available = GeneralSettingsSnapshot(general: general, subtitle: "")
    #expect(available.updates.outcome == .available)
    #expect(available.updates.detail == "0.9.1")
    updater.lastOutcome = .failed("offline")
    let failed = GeneralSettingsSnapshot(general: general, subtitle: "")
    #expect(failed.updates.outcome == .failed)
    #expect(failed.updates.detail == "offline")
  }

  @Test func recordingCarriesDevicesFolderRetentionAndPermissions() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let audio = AudioSettingsViewModel(environment: environment)
    audio.listInputs = { [] }
    audio.measureFolder = { _ in 4_200 }
    await audio.load()
    let snapshot = RecordingSettingsSnapshot(audio: audio, subtitle: "Ready")
    #expect(snapshot.devices.isEmpty)
    #expect(snapshot.inputDeviceUID == nil, "the system default")
    #expect(snapshot.audioFolderPath == audio.audioFolder.path)
    #expect(snapshot.audioFolderName == audio.folderName)
    #expect(snapshot.folderUsage == .measured)
    #expect(snapshot.folderUsageBytes == 4_200)
    #expect(snapshot.retention.mode.rawValue == audio.retentionMode.rawValue)
    #expect(snapshot.retention.days == audio.retentionDays)
    #expect(snapshot.retentionFootnote == audio.footnote)
    #expect(!snapshot.retentionFootnote.isEmpty)
    #expect(snapshot.keptForeverCount == nil)
    #expect(snapshot.permissions.map(\.kind) == [.microphone, .systemAudio])
    #expect(snapshot.permissions.allSatisfy { $0.state == .granted && !$0.isRequesting })

    await audio.setRetention(mode: .keepForever, days: 30)
    let forever = RecordingSettingsSnapshot(audio: audio, subtitle: "Ready")
    #expect(forever.retention.mode == .keepForever)
    #expect(forever.keptForeverCount == 0, "nothing on disk to keep, but the sweep ran")
  }

  @Test func transcriptionCarriesEnginesAndAssetStates() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let speech = SpeechSettingsViewModel(environment: environment)
    await speech.load()
    let snapshot = TranscriptionSettingsSnapshot(speech: speech, subtitle: "Download needed")
    #expect(snapshot.engineID == SpeechEngineID.parakeetV3.rawValue)
    #expect(snapshot.engines.map(\.id) == SpeechEngineID.userSelectable.map(\.rawValue))
    #expect(snapshot.showsEnginePicker == (SpeechEngineID.userSelectable.count > 1))
    #expect(
      snapshot.assets.map(\.id) == [
        ModelAsset.parakeetV3.rawValue, ModelAsset.offlineDiarizer.rawValue,
      ])
    #expect(snapshot.assets.map(\.name) == ["Speech recognition", "Speaker recognition"])
    #expect(snapshot.assets.allSatisfy { $0.state == .absent })
    #expect(snapshot.assets.allSatisfy { $0.detail.hasPrefix("Not downloaded") })
    #expect(!snapshot.allInstalled)

    try await environment.models.ensureInstalled(.offlineDiarizer)
    speech.refreshStates()
    let installed = TranscriptionSettingsSnapshot(speech: speech, subtitle: "")
    let diarizer = try #require(
      installed.assets.first { $0.id == ModelAsset.offlineDiarizer.rawValue })
    #expect(diarizer.state == .installed)
    #expect(diarizer.installedBytes != nil)
    #expect(diarizer.detail.hasPrefix("Installed"))
  }

  @Test func summariesCarryPresetsTheDraftAndTheCodexBlockOnlyForThatPreset() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let llm = LLMSettingsViewModel(environment: environment)
    await llm.load()
    let snapshot = SummariesSettingsSnapshot(llm: llm, subtitle: "Not set up")
    #expect(snapshot.presets.map(\.id) == LLMPreset.allCases.map(\.rawValue))
    #expect(snapshot.presetID == LLMPreset.lmStudio.rawValue)
    #expect(snapshot.baseURL == "http://127.0.0.1:1234/v1")
    #expect(snapshot.model == "")
    #expect(snapshot.contextTokens == "32000")
    #expect(snapshot.defaultContextTokens == LLMSettingsViewModel.defaultContextTokens)
    #expect(!snapshot.hasAPIKey)
    #expect(!snapshot.isConfigured)
    #expect(!snapshot.isTesting)
    #expect(snapshot.testResult == nil)
    #expect(snapshot.validationMessage == nil)
    #expect(snapshot.codex == nil, "no Codex block outside the ChatGPT preset")

    llm.apiKey = "sk-live-STENO-SECRET"
    llm.baseURLText = "ftp://nope"
    let invalid = SummariesSettingsSnapshot(llm: llm, subtitle: "")
    #expect(invalid.hasAPIKey, "only whether a key is there")
    #expect(invalid.validationMessage != nil)
    let encoded = String(decoding: try BridgeJSON.encode(invalid), as: UTF8.self)
    #expect(!encoded.contains("STENO-SECRET"), "the key never reaches the wire")

    await llm.selectPreset(.codex)
    let codex = try #require(SummariesSettingsSnapshot(llm: llm, subtitle: "").codex)
    #expect(!codex.confirmed)
    #expect(codex.signIn == .unavailable, "no Codex sign-in in the preview")
    #expect(codex.signInDetail != nil)
    #expect(codex.model == "")
    #expect(codex.models.isEmpty)
  }

  @Test func exportAndPhoneReadTheirModels() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let obsidian = ObsidianSettingsViewModel(environment: environment)
    await obsidian.load()
    let export = ExportSettingsSnapshot(obsidian: obsidian, subtitle: "Off")
    #expect(!export.enabled)
    #expect(export.vaultPath == nil)
    #expect(export.vaultName == nil)
    #expect(export.peopleFolder == "")
    #expect(!export.includeAudio)
    #expect(!export.saved)

    let phones = PhonesSettingsViewModel(environment: environment)
    await phones.load()
    let phone = PhoneSettingsSnapshot(phones: phones, subtitle: "Unavailable")
    #expect(phone.listener.state == .unavailable, "no handover service in the preview")
    #expect(phone.macID == nil)
    #expect(phone.devices.isEmpty)
    #expect(phone.pairing == nil)
    #expect(phone.receipts.isEmpty)
  }
}

// MARK: - Command routing

@Suite @MainActor struct SettingsBridgeTests {
  /// A host over a fresh controller, with the folder panel answering
  /// `chosen`, its microphones stubbed empty and its folder measured flat.
  private func bridge(_ environment: AppEnvironment, chosen: URL? = nil) -> SettingsBridge {
    let host = SettingsBridge(
      controller: AppController(environment: environment), chooseFolder: { _ in chosen })
    host.audio.listInputs = { [] }
    host.audio.measureFolder = { _ in 0 }
    return host
  }

  /// `page.ready` publishes every topic once, in order, and nothing before.
  @Test func pageReadyPublishesEveryTopicOnce() async throws {
    let host = bridge(try await TestSupport.environment(seed: false))
    let sink = RecordingSink()
    host.attach(sink)
    host.start()
    await host.load()
    #expect(sink.events.isEmpty, "nothing is sent before the page is ready")

    let ready = await BridgeDispatcher.dispatch(request("r1", "page.ready"), host: host)
    #expect(ready == BridgeReply(id: "r1"))
    #expect(sink.events.map(\.topic) == SettingsBridge.topics)
    #expect(sink.last(.settingsGeneral)?["subtitle"] != nil)
    #expect(sink.last(.settingsRecording)?["subtitle"] == .string("Ready"))
    #expect(sink.last(.settingsTranscription)?["subtitle"] == .string("Download needed"))
    #expect(sink.last(.settingsSummaries)?["subtitle"] == .string("Not set up"))
    #expect(sink.last(.settingsExport)?["subtitle"] == .string("Off"))
    #expect(sink.last(.settingsPhone)?["subtitle"] == .string("Unavailable"))
    #expect((sink.last(.app)?["requestedSettingsSection"] ?? .null) == .null)
  }

  /// A command reaches its view model, is stored, and comes back as a
  /// publish of its section.
  @Test func generalAndRecordingCommandsStoreAndRepublish() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let host = bridge(environment)
    let sink = RecordingSink()
    host.attach(sink)
    host.start()
    await host.load()
    _ = await BridgeDispatcher.dispatch(request("r0", "page.ready"), host: host)

    let detection = await BridgeDispatcher.dispatch(
      request("r1", "settings.general.setDetectionEnabled", ["value": false]), host: host)
    #expect(detection == BridgeReply(id: "r1"))
    #expect(!host.general.detectionEnabled)
    #expect(try await environment.settings.load().meetingDetectionEnabled == false)
    await eventually("general republished") {
      sink.last(.settingsGeneral)?["detectionEnabled"] == .bool(false)
    }

    let retention = await BridgeDispatcher.dispatch(
      request(
        "r2", "settings.recording.setRetention",
        ["retention": ["mode": "keepDays", "days": 14]]), host: host)
    #expect(retention == BridgeReply(id: "r2"))
    #expect(host.audio.retentionMode == .keepDays)
    #expect(host.audio.retentionDays == 14)
    #expect(try await environment.settings.load().defaultRetention == .keepDays(14))
    await eventually("recording republished") {
      sink.last(.settingsRecording)?["retention"]?["days"] == .number(14)
    }

    let template = await BridgeDispatcher.dispatch(
      request("r3", "settings.general.setDefaultTemplate", ["templateID": "nope"]), host: host)
    #expect(template == BridgeReply(id: "r3"), "an unknown template is ignored by the model")
    #expect(host.general.defaultTemplateID == SummaryTemplate.defaultID)
  }

  /// The folder panel's answer: nil leaves the folder and replies a null
  /// path; a folder becomes the recordings folder and is echoed back.
  @Test func chooseFolderAppliesThePanelsAnswer() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let cancelled = bridge(environment)
    cancelled.start()
    await cancelled.load()
    let before = cancelled.audio.audioFolder
    let declined = await BridgeDispatcher.dispatch(
      request("r1", "settings.recording.chooseFolder"), host: cancelled)
    #expect(declined.error == nil)
    #expect((declined.result?["path"] ?? .null) == .null, "a cancelled panel replies no path")
    #expect(cancelled.audio.audioFolder == before)

    let folder = try TestSupport.temporaryDirectory("steno-recordings")
    defer { try? FileManager.default.removeItem(at: folder) }
    let choosing = bridge(environment, chosen: folder)
    choosing.start()
    await choosing.load()
    let chosen = await BridgeDispatcher.dispatch(
      request("r2", "settings.recording.chooseFolder"), host: choosing)
    #expect(chosen.result?["path"] == .string(folder.path))
    #expect(choosing.audio.audioFolder == folder)
    #expect(try await environment.settings.load().audioFolder == folder)

    let vault = await BridgeDispatcher.dispatch(
      request("r3", "settings.export.chooseVault"), host: choosing)
    #expect(vault.result?["path"] == .string(folder.path))
    #expect(choosing.obsidian.vaultPath == folder.path, "the form shows the choice")
  }

  /// The deep link rides the `app` snapshot and is cleared when the page
  /// reports that section shown, not any other.
  @Test func showSectionConsumesTheMatchingDeepLink() async throws {
    let host = bridge(try await TestSupport.environment(seed: false))
    let sink = RecordingSink()
    host.attach(sink)
    host.controller.openSettings(.recording)
    host.start()
    await host.load()
    _ = await BridgeDispatcher.dispatch(request("r0", "page.ready"), host: host)
    #expect(sink.last(.app)?["requestedSettingsSection"] == .string("recording"))

    let other = await BridgeDispatcher.dispatch(
      request("r1", "settings.showSection", ["section": "general"]), host: host)
    #expect(other == BridgeReply(id: "r1"))
    #expect(host.controller.requestedSettingsSection == .recording, "another section leaves it")

    let shown = await BridgeDispatcher.dispatch(
      request("r2", "settings.showSection", ["section": "recording"]), host: host)
    #expect(shown == BridgeReply(id: "r2"))
    #expect(host.controller.requestedSettingsSection == nil)
    await eventually("app republished without the request") {
      (sink.last(.app)?["requestedSettingsSection"] ?? .null) == .null
    }
  }

  @Test func methodsOfOtherWindowsAndBadIdsAreTypedErrors() async throws {
    let host = bridge(try await TestSupport.environment(seed: false))
    host.start()
    await host.load()
    let filter = await BridgeDispatcher.dispatch(
      request("r1", "meetings.setFilter", ["filter": "failed"]), host: host)
    #expect(filter.error?.code == .unknownMethod)
    let engine = await BridgeDispatcher.dispatch(
      request("r2", "settings.transcription.setEngine", ["value": "nope"]), host: host)
    #expect(engine.error?.code == .invalidParams)
    let asset = await BridgeDispatcher.dispatch(
      request("r3", "settings.transcription.download", ["assetID": "nope"]), host: host)
    #expect(asset.error?.code == .invalidParams)
    let preset = await BridgeDispatcher.dispatch(
      request("r4", "settings.summaries.selectPreset", ["value": "nope"]), host: host)
    #expect(preset.error?.code == .invalidParams)
    let missing = await BridgeDispatcher.dispatch(
      request("r5", "settings.general.setLaunchAtLogin"), host: host)
    #expect(missing.error?.code == .invalidParams)
    let url = await BridgeDispatcher.dispatch(
      request("r6", "system.openURL", ["url": "file:///etc/hosts"]), host: host)
    #expect(url.error?.code == .invalidParams)
  }

  /// The Summaries draft: `update` changes the form without storing, `save`
  /// commits and probes; the Export form waits for a vault.
  @Test func summariesAndExportDraftsStoreOnSave() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let host = bridge(environment)
    let sink = RecordingSink()
    host.attach(sink)
    host.start()
    await host.load()
    _ = await BridgeDispatcher.dispatch(request("r0", "page.ready"), host: host)

    let update = await BridgeDispatcher.dispatch(
      request(
        "r1", "settings.summaries.update",
        ["baseURL": "http://127.0.0.1:9/v1", "model": "qwen", "apiKey": "sk-typed"]),
      host: host)
    #expect(update == BridgeReply(id: "r1"))
    #expect(host.llm.model == "qwen")
    #expect(try await environment.settings.load().llmModel == nil, "nothing stored before save")
    await eventually("the draft republished") {
      sink.last(.settingsSummaries)?["model"] == .string("qwen")
        && sink.last(.settingsSummaries)?["hasAPIKey"] == .bool(true)
    }
    let published = try #require(sink.last(.settingsSummaries))
    let encoded = String(decoding: try BridgeJSON.encode(published), as: UTF8.self)
    #expect(!encoded.contains("sk-typed"), "the key stays off the wire")

    let save = await BridgeDispatcher.dispatch(request("r2", "settings.summaries.save"), host: host)
    #expect(save == BridgeReply(id: "r2"))
    let stored = try await environment.settings.load()
    #expect(stored.llmBaseURL?.absoluteString == "http://127.0.0.1:9/v1")
    #expect(stored.llmModel == "qwen")
    #expect(try await environment.secrets.secret(for: .llmAPIKey) == "sk-typed")
    #expect(host.llm.isConfigured)
    #expect(host.llm.testResult != nil, "a configured endpoint is probed; port 9 refuses")
    await eventually("the result republished") {
      sink.last(.settingsSummaries)?["testResult"]?["ok"] == .bool(false)
    }

    let enable = await BridgeDispatcher.dispatch(
      request("r3", "settings.export.setEnabled", ["value": true]), host: host)
    #expect(enable == BridgeReply(id: "r3"))
    #expect(host.obsidian.enabled)
    #expect(host.obsidian.needsVault)
    #expect(
      try await environment.settings.load().obsidian == nil, "on without a vault stores nothing")
    let people = await BridgeDispatcher.dispatch(
      request("r4", "settings.export.update", ["peopleFolder": "Team"]), host: host)
    #expect(people == BridgeReply(id: "r4"))
    #expect(host.obsidian.peopleFolder == "Team")
    await eventually("export republished") {
      sink.last(.settingsExport)?["enabled"] == .bool(true)
        && sink.last(.settingsExport)?["peopleFolder"] == .string("Team")
    }
  }
}
