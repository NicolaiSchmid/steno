import AppKit
import Foundation
import StenoBridge
import StenoCore
import StenoSpeech

/// The Settings window's host (plan Decisions 5 and 6). Owns the six section
/// view models and the sidebar overview, loads them when the window opens,
/// follows them with observation tracking and publishes one full snapshot
/// per section to the attached sink, coalesced to the next main-actor turn.
/// Commands map one to one onto the view models' public methods, so no rule
/// lives here. The folder choosers (`settings.recording.chooseFolder`,
/// `settings.export.chooseVault`) are the one native surface the page cannot
/// draw: an `NSOpenPanel`, answered with the chosen path or nil.
///
/// The `app` topic is published too, for the deep link: the page selects
/// `requestedSettingsSection` from the snapshot that carries it, and the
/// publish itself clears the request, as the main window's does for a
/// meeting.
@MainActor
final class SettingsBridge: BridgeHost {
  /// What the page asks the host to open; `SettingsWindow` installs the
  /// scene's `openWindow` here.
  typealias OpenWindow = @MainActor (WindowParams) -> Void
  /// The folder chooser: `NSOpenPanel` in the app, a stub in tests. Takes
  /// the folder to start in and returns the choice, nil when cancelled.
  typealias ChooseFolder = @MainActor (URL?) async -> URL?

  let controller: AppController
  let overview: SettingsOverviewViewModel
  let general: GeneralSettingsViewModel
  let audio: AudioSettingsViewModel
  let speech: SpeechSettingsViewModel
  let llm: LLMSettingsViewModel
  let obsidian: ObsidianSettingsViewModel
  let phones: PhonesSettingsViewModel
  var openWindow: OpenWindow = { _ in }

  private let chooseFolder: ChooseFolder
  private weak var events: (any BridgeEventSink)?
  /// True from `page.ready`: before it the page has no `window.steno` and
  /// an emit would be lost, so tracking is armed but nothing is sent.
  private var pageReady = false
  /// True between `start()` and `stop()`.
  private var running = false
  private var pending: Set<BridgeTopic> = []
  /// Polls the paired devices while a pairing code is shown.
  private var pairingPoll: Task<Void, Never>?

  /// The window's topics, in the order `page.ready` publishes them.
  static let topics: [BridgeTopic] = [
    .app, .settingsGeneral, .settingsRecording, .settingsTranscription, .settingsSummaries,
    .settingsExport, .settingsPhone,
  ]

  init(controller: AppController, chooseFolder: ChooseFolder? = nil) {
    self.controller = controller
    let environment = controller.environment
    overview = SettingsOverviewViewModel(environment: environment)
    general = GeneralSettingsViewModel(environment: environment)
    audio = AudioSettingsViewModel(environment: environment)
    speech = SpeechSettingsViewModel(environment: environment)
    llm = LLMSettingsViewModel(environment: environment)
    obsidian = ObsidianSettingsViewModel(environment: environment)
    phones = PhonesSettingsViewModel(environment: environment)
    let panel: ChooseFolder = { await Self.presentFolderPanel(startingAt: $0) }
    self.chooseFolder = chooseFolder ?? panel
  }

  /// Arms every topic's tracking and lets later changes publish.
  func start() {
    guard !running else { return }
    running = true
    for topic in Self.topics { flush(topic) }
  }

  /// Runs for the window's lifetime (`SettingsWindow`'s `.task`): loads
  /// every section, follows the phone listener and the transfers, and stays
  /// alive until cancelled, when it stops following.
  func run() async {
    start()
    await load()
    // The phone observers run for the window's lifetime; cancelling the
    // window's task cancels them and ends this.
    let phones = self.phones
    await withTaskGroup(of: Void.self) { group in
      group.addTask { await phones.observe() }
      group.addTask { await phones.observeReceipts() }
    }
    stop()
  }

  /// Every section's `load()`, then the sidebar subtitles.
  func load() async {
    // Side by side: the folder walk, the keychain read and the device list
    // are independent, and the window should not wait for them in turn.
    async let generalLoad: Void = general.load()
    async let audioLoad: Void = audio.load()
    async let speechLoad: Void = speech.load()
    async let llmLoad: Void = llm.load()
    async let obsidianLoad: Void = obsidian.load()
    async let phonesLoad: Void = phones.load()
    _ = await (generalLoad, audioLoad, speechLoad, llmLoad, obsidianLoad, phonesLoad)
    await overview.refresh()
  }

  func stop() {
    running = false
    pairingPoll?.cancel()
    pairingPoll = nil
  }

  func attach(_ events: any BridgeEventSink) {
    self.events = events
  }

  // MARK: - Publishing

  /// Asks for a publish of `topic` on a later main-actor turn; a second ask
  /// before that turn is folded into it.
  private func schedule(_ topic: BridgeTopic) {
    guard running, pending.insert(topic).inserted else { return }
    Task { @MainActor [weak self] in self?.flush(topic) }
  }

  /// Builds the topic's snapshot inside observation tracking, so the next
  /// change to anything it read schedules the next publish, and emits it
  /// once the page is ready.
  private func flush(_ topic: BridgeTopic) {
    guard running else { return }
    pending.remove(topic)
    var snapshot: (any Encodable)?
    withObservationTracking {
      snapshot = self.snapshot(for: topic)
    } onChange: { [weak self] in
      Task { @MainActor [weak self] in self?.schedule(topic) }
    }
    guard pageReady, let events, let snapshot else { return }
    events.emit(topic, snapshot: snapshot)
    // A deep link is consumed by the publish that carries it, as the main
    // window consumes its meeting request: the page shows the section from
    // this snapshot and the next `app` snapshot carries nil.
    if topic == .app { controller.requestedSettingsSection = nil }
  }

  /// The topic's snapshot from the view models as they stand; nil for the
  /// topics this window never publishes.
  private func snapshot(for topic: BridgeTopic) -> (any Encodable)? {
    switch topic {
    case .app:
      return AppSnapshot(controller: controller)
    case .settingsGeneral:
      return GeneralSettingsSnapshot(general: general, subtitle: subtitle(.general))
    case .settingsRecording:
      return RecordingSettingsSnapshot(audio: audio, subtitle: subtitle(.recording))
    case .settingsTranscription:
      return TranscriptionSettingsSnapshot(speech: speech, subtitle: subtitle(.transcription))
    case .settingsSummaries:
      return SummariesSettingsSnapshot(llm: llm, subtitle: subtitle(.summaries))
    case .settingsExport:
      return ExportSettingsSnapshot(obsidian: obsidian, subtitle: subtitle(.export))
    case .settingsPhone:
      return PhoneSettingsSnapshot(phones: phones, subtitle: subtitle(.iphone))
    default:
      return nil
    }
  }

  private func subtitle(_ section: SettingsSection) -> String {
    overview.subtitles[section] ?? ""
  }

  /// The sidebar subtitles follow every Settings command, as the SwiftUI
  /// window refreshed them on each selection change. Not awaited: the
  /// overview reads the store and the permissions, and the reply must not
  /// wait for it.
  private func refreshSubtitles() {
    let overview = self.overview
    Task { await overview.refresh() }
  }

  /// While a pairing code is shown, the view model polls the paired devices
  /// on the app's clock so the phone's arrival closes the code.
  private func followPairing() {
    pairingPoll?.cancel()
    pairingPoll = nil
    guard phones.pairing != nil else { return }
    let phones = self.phones
    pairingPoll = Task { await phones.observePairing() }
  }

  // MARK: - Commands

  func handle(_ request: BridgeRequest) async throws -> JSONValue? {
    let reply = try await route(request)
    if request.method.rawValue.hasPrefix("settings.") { refreshSubtitles() }
    return reply
  }

  private func route(_ request: BridgeRequest) async throws -> JSONValue? {
    switch request.method {
    case .pageReady:
      UITestDiagnostics.note("settings page ready")
      pageReady = true
      for topic in Self.topics { flush(topic) }
    case .pageLayout:
      // Validated and dropped: the window has one size.
      _ = try request.params(PageLayoutParams.self)
    case .settingsGeneralSetLaunchAtLogin:
      let enabled = try request.params(SetBoolParams.self).value
      await general.setLaunchAtLogin(enabled)
    case .settingsGeneralSetDetection:
      let enabled = try request.params(SetBoolParams.self).value
      await general.setDetectionEnabled(enabled)
    case .settingsGeneralSetDefaultTemplate:
      let templateID = try request.params(SetTemplateParams.self).templateID
      await general.setDefaultTemplate(templateID)
    case .settingsGeneralRequestCalendar:
      await general.requestCalendar()
    case .settingsGeneralSetAutomaticUpdates:
      let updates = try request.params(SetAutomaticUpdatesParams.self)
      general.automaticallyChecksForUpdates = updates.automaticallyChecks
      general.automaticallyDownloadsUpdates = updates.automaticallyDownloads
    case .settingsGeneralOpenLoginItems:
      general.openLoginItemSettings()

    case .settingsRecordingSetInputDevice:
      let uid = try request.params(SetStringParams.self).value
      await audio.setInputDevice(uid.isEmpty ? nil : uid)
    case .settingsRecordingRefreshDevices:
      audio.refreshDevices()
    case .settingsRecordingChooseFolder:
      let chosen = await chooseFolder(audio.audioFolder)
      if let chosen { await audio.setAudioFolder(chosen) }
      return try BridgeReplies.value(ChosenPathReply(path: chosen?.path))
    case .settingsRecordingRevealFolder:
      audio.revealFolder()
    case .settingsRecordingSetRetention:
      let retention = try request.params(SetRetentionParams.self).retention
      guard let mode = AudioSettingsViewModel.RetentionMode(rawValue: retention.mode.rawValue)
      else {
        throw BridgeError(
          code: .invalidParams, message: "Unknown retention rule \(retention.mode.rawValue).")
      }
      await audio.setRetention(mode: mode, days: retention.days)
    case .settingsRecordingRequestPermission:
      let kind = try request.params(PermissionKindParams.self).kind
      await audio.requestPermission(try BridgeSystemCommands.permissionKind(kind))

    case .settingsTranscriptionSetEngine:
      let id = try request.params(SetStringParams.self).value
      guard let engine = SpeechEngineID(rawValue: id) else {
        throw BridgeError(code: .invalidParams, message: "Unknown speech engine \(id).")
      }
      await speech.setEngine(engine)
    case .settingsTranscriptionDownload:
      speech.download(try Self.asset(request))
    case .settingsTranscriptionRemove:
      await speech.remove(try Self.asset(request))

    case .settingsSummariesSelectPreset:
      let id = try request.params(SetStringParams.self).value
      guard let preset = LLMPreset(rawValue: id) else {
        throw BridgeError(code: .invalidParams, message: "Unknown summaries service \(id).")
      }
      await llm.selectPreset(preset)
    case .settingsSummariesUpdate:
      // The page keeps the draft while typing and sends the fields on blur;
      // nothing is stored until `settings.summaries.save`.
      let update = try request.params(SummariesUpdateParams.self)
      if let baseURL = update.baseURL { llm.baseURLText = baseURL }
      if let model = update.model { llm.model = model }
      if let tokens = update.contextTokens { llm.contextTokensText = tokens }
      if let key = update.apiKey { llm.apiKey = key }
    case .settingsSummariesSave:
      await llm.commit()
    case .settingsSummariesTest:
      await llm.test()
    case .settingsSummariesConfirmCodex:
      await llm.confirmCodex()
    case .settingsSummariesRefreshCodexStatus:
      await llm.refreshCodexStatus()
    case .settingsSummariesRefreshCodexModels:
      await llm.refreshCodexModels()
    case .settingsSummariesSelectCodexModel:
      let slug = try request.params(SetStringParams.self).value
      await llm.selectCodexModel(slug)
    case .settingsSummariesStopUsingCodex:
      await llm.stopUsingCodex()

    case .settingsExportSetEnabled:
      let enabled = try request.params(SetBoolParams.self).value
      await obsidian.setEnabled(enabled)
    case .settingsExportChooseVault:
      let chosen = await chooseFolder(obsidian.vaultURL)
      if let chosen { await obsidian.chooseVault(chosen) }
      return try BridgeReplies.value(ChosenPathReply(path: chosen?.path))
    case .settingsExportUpdate:
      let update = try request.params(ExportUpdateParams.self)
      if let people = update.peopleFolder { obsidian.peopleFolder = people }
      if let tag = update.taskTag { obsidian.taskTag = tag }
      if let include = update.includeAudio { await obsidian.setIncludeAudio(include) }
    case .settingsExportSave:
      await obsidian.commit()

    case .settingsPhoneBeginPairing:
      await phones.beginPairing()
      followPairing()
    case .settingsPhoneCancelPairing:
      pairingPoll?.cancel()
      pairingPoll = nil
      await phones.cancelPairing()
    case .settingsPhoneRevoke:
      let deviceID = try request.params(DeviceIDParams.self).deviceID
      await phones.revoke(deviceID)

    case .updatesCheck:
      general.checkForUpdates()
    case .systemOpenURL:
      try BridgeSystemCommands.openURL(try request.params(OpenURLParams.self).url)
    case .systemOpenSystemSettings:
      let kind = try request.params(PermissionKindParams.self).kind
      controller.environment.permissions.openSystemSettings(
        for: try BridgeSystemCommands.permissionKind(kind))
    case .windowOpen:
      openWindow(try request.params(WindowParams.self))

    default:
      throw BridgeError(
        code: .unknownMethod,
        message: "The Settings window does not answer \(request.method.rawValue).")
    }
    return nil
  }

  private static func asset(_ request: BridgeRequest) throws -> ModelAsset {
    let id = try request.params(AssetIDParams.self).assetID
    guard let asset = ModelAsset(rawValue: id) else {
      throw BridgeError(code: .invalidParams, message: "Unknown model \(id).")
    }
    return asset
  }

  /// `NSOpenPanel` for a folder, as a sheet on the key window (else
  /// app-modal), starting in `folder` when it is set.
  static func presentFolderPanel(startingAt folder: URL?) async -> URL? {
    let panel = NSOpenPanel()
    panel.canChooseDirectories = true
    panel.canChooseFiles = false
    panel.canCreateDirectories = true
    panel.allowsMultipleSelection = false
    panel.directoryURL = folder
    panel.prompt = "Use folder"
    let response: NSApplication.ModalResponse
    if let window = NSApp.keyWindow ?? NSApp.mainWindow {
      response = await panel.beginSheetModal(for: window)
    } else {
      response = panel.runModal()
    }
    return response == .OK ? panel.url : nil
  }
}
