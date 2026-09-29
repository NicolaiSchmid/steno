import AppKit
import StenoAudio
import StenoCore
import StenoSpeech
import SwiftUI

/// The Settings scene: a sidebar of six sections with a status subtitle
/// each, one grouped form on the right. One view model per section, built
/// once for the scene's lifetime (a model created in `body` would be
/// replaced on every evaluation and its state lost). Deep links arrive as
/// `controller.requestedSettingsSection`.
struct SettingsView: View {
  static let width: CGFloat = 760
  static let height: CGFloat = 520
  static let sidebarWidth: CGFloat = 200

  let controller: AppController
  @State private var selection: SettingsSection = .general
  @State private var overview: SettingsOverviewViewModel
  @State private var general: GeneralSettingsViewModel
  @State private var audio: AudioSettingsViewModel
  @State private var speech: SpeechSettingsViewModel
  @State private var llm: LLMSettingsViewModel
  @State private var obsidian: ObsidianSettingsViewModel
  @State private var phones: PhonesSettingsViewModel

  init(controller: AppController) {
    self.controller = controller
    let environment = controller.environment
    _overview = State(initialValue: SettingsOverviewViewModel(environment: environment))
    _general = State(initialValue: GeneralSettingsViewModel(environment: environment))
    _audio = State(initialValue: AudioSettingsViewModel(environment: environment))
    _speech = State(initialValue: SpeechSettingsViewModel(environment: environment))
    _llm = State(initialValue: LLMSettingsViewModel(environment: environment))
    _obsidian = State(initialValue: ObsidianSettingsViewModel(environment: environment))
    _phones = State(initialValue: PhonesSettingsViewModel(environment: environment))
  }

  var body: some View {
    NavigationSplitView {
      List(SettingsSection.allCases, selection: $selection) { section in
        SettingsSidebarRow(
          section: section, subtitle: overview.subtitles[section],
          isSelected: section == selection
        )
        .tag(section)
      }
      .listStyle(.sidebar)
      .navigationSplitViewColumnWidth(Self.sidebarWidth)
    } detail: {
      // The detail keeps a fixed height. On macOS 26 the split view sizes
      // itself from the detail's ideal height, and a flexible detail (min,
      // ideal or max) makes it the width of the screen instead, so both
      // columns overflow the window and read as empty. On macOS 15 the
      // column is this tall anyway.
      detail
        .frame(maxWidth: .infinity, alignment: .top)
        .frame(height: Self.height, alignment: .top)
        .background(Color.stenoBackground)
    }
    .navigationSplitViewStyle(.balanced)
    .toolbar(removing: .sidebarToggle)
    .navigationTitle(selection.title)
    .frame(width: Self.width, height: Self.height)
    .task(id: selection) { await overview.refresh() }
    .onChange(of: controller.requestedSettingsSection, initial: true) { _, requested in
      guard let requested else { return }
      selection = requested
      controller.requestedSettingsSection = nil
    }
  }

  @ViewBuilder
  private var detail: some View {
    switch selection {
    case .general: GeneralSettingsView(model: general)
    case .recording: RecordingSettingsView(model: audio)
    case .transcription: TranscriptionSettingsView(model: speech)
    case .summaries: SummariesSettingsView(model: llm)
    case .export: ExportSettingsView(model: obsidian)
    case .iphone: PhoneSettingsView(model: phones)
    }
  }
}

/// Header plus grouped form, the shape every section shares. The page loads
/// its model on appear and ends with the model's error row, so no section
/// repeats either.
private struct SectionPage<Content: View>: View {
  let section: SettingsSection
  let model: any SettingsSectionModel
  @ViewBuilder let content: () -> Content

  var body: some View {
    VStack(spacing: 0) {
      SettingsHeader(section: section)
      Form {
        content()
        if let error = model.error {
          SettingsErrorRow(message: error, details: model.errorDetails)
        }
      }
      .formStyle(.grouped)
      .scrollContentBackground(.hidden)
    }
    .task { await model.load() }
  }
}

// MARK: - General

struct GeneralSettingsView: View {
  @Bindable var model: GeneralSettingsViewModel
  @State private var showingAcknowledgements = false

  var body: some View {
    SectionPage(section: .general, model: model) {
      Section {
        Toggle(
          "Open Steno at login",
          isOn: .action({ model.launchAtLogin }, model.setLaunchAtLogin))
        if model.loginItem == .requiresApproval {
          HStack(alignment: .top) {
            MessageRow(
              kind: .warning,
              text: "Waiting for your approval in System Settings › Login Items.")
            Spacer()
            Button("Open Login Items") { model.openLoginItemSettings() }
          }
        }
        Toggle(
          "Offer to record when a call starts",
          isOn: .action({ model.detectionEnabled }, model.setDetectionEnabled))
        Footnote("Steno notices when another app opens the microphone and asks before recording.")
      }
      Section("Meetings") {
        PermissionRow(
          kind: .calendar, state: model.calendarPermission, isRequesting: model.requestingCalendar,
          request: { Task { await model.requestCalendar() } },
          openSystemSettings: { model.openCalendarSettings() })
        Picker(
          "Summary template",
          selection: .action({ model.defaultTemplateID }, model.setDefaultTemplate)
        ) {
          ForEach(model.templates) { template in
            Text(template.displayName).tag(template.id)
          }
        }
        if let template = model.selectedTemplate {
          Footnote(template.description)
        }
      }
      Section("Updates") {
        HStack(alignment: .top) {
          SettingsRowLabel(
            title: "Steno \(model.version)", subtitle: model.updateStatusText(now: Date()))
          Spacer()
          Button("Check for Updates") { model.checkForUpdates() }
            .disabled(!model.canCheckForUpdates)
        }
        if let details = model.updateFailureDetails {
          SettingsErrorRow(message: "The last update check did not succeed.", details: details)
        }
        Toggle("Check for updates automatically", isOn: $model.automaticallyChecksForUpdates)
        Toggle("Install updates automatically", isOn: $model.automaticallyDownloadsUpdates)
        Footnote("Updates are checked once a day and installed when you relaunch Steno.")
        Button("Acknowledgements…") { showingAcknowledgements = true }
          .buttonStyle(.plain)
          .font(.steno(Theme.TextSize.xs))
          .foregroundStyle(Color.stenoMutedForeground)
      }
    }
    .sheet(isPresented: $showingAcknowledgements) {
      AcknowledgementsView { showingAcknowledgements = false }
    }
  }
}

// MARK: - Recording

struct RecordingSettingsView: View {
  let model: AudioSettingsViewModel

  var body: some View {
    SectionPage(section: .recording, model: model) {
      Section("Permissions") {
        ForEach(AudioSettingsViewModel.recordingPermissions) { kind in
          PermissionRow(
            kind: kind, state: model.state(of: kind), isRequesting: model.requesting == kind,
            request: { Task { await model.requestPermission(kind) } },
            openSystemSettings: { model.openPermissionSettings(kind) })
        }
        if model.allPermissionsGranted {
          Footnote("Steno can record your microphone and the audio of your calls.")
        }
      }
      Section("Microphone") {
        HStack {
          Picker(
            "Microphone", selection: .action({ model.inputDeviceUID }, model.setInputDevice)
          ) {
            Text("System default").tag(String?.none)
            ForEach(model.devices) { device in
              Text(device.name).tag(String?.some(device.uid))
            }
          }
          Button {
            model.refreshDevices()
          } label: {
            Image(systemName: "arrow.clockwise")
              .foregroundStyle(Color.stenoMutedForeground)
          }
          .buttonStyle(.borderless)
          .help("Refresh microphones")
          .accessibilityLabel("Refresh microphones")
        }
      }
      Section("Recordings") {
        FolderRow(
          label: "Folder", url: model.audioFolder,
          choose: { url in Task { await model.setAudioFolder(url) } },
          reveal: { model.revealFolder() })
        Footnote(model.folderUsage.text)
        HStack {
          Picker(
            "Keep recordings",
            selection: .action(
              { model.retentionMode },
              { mode in await model.setRetention(mode: mode, days: model.retentionDays) })
          ) {
            ForEach(AudioSettingsViewModel.RetentionMode.allCases) { mode in
              Text(mode.title(days: model.retentionDays)).tag(mode)
            }
          }
          if model.retentionMode == .keepDays {
            Stepper(
              "Days",
              value: .action(
                { model.retentionDays },
                { days in await model.setRetention(mode: .keepDays, days: days) }),
              in: AudioSettingsViewModel.dayRange
            )
            .labelsHidden()
          }
        }
        Footnote(model.footnote + " Short voice samples stay until you have named the speaker.")
      }
    }
  }
}

// MARK: - Transcription

struct TranscriptionSettingsView: View {
  let model: SpeechSettingsViewModel

  var body: some View {
    SectionPage(section: .transcription, model: model) {
      if model.showsEnginePicker {
        Section("Language model") {
          Picker("Model", selection: .action({ model.engineID }, model.setEngine)) {
            ForEach(model.engines, id: \.self) { engine in
              Text(SpeechSettingsViewModel.engineTitle(engine)).tag(engine)
            }
          }
          Footnote("Both run on this Mac. Parakeet is quicker; Whisper understands more languages.")
        }
      }
      Section("On this Mac") {
        ForEach(model.assets, id: \.self) { asset in
          componentRow(asset)
        }
        Footnote(
          model.allInstalled
            ? "Everything needed for transcription is installed."
            : "Downloads happen once and are kept for later meetings.")
      }
    }
  }

  private func componentRow(_ asset: ModelAsset) -> some View {
    let state = model.state(of: asset)
    return VStack(alignment: .leading, spacing: Theme.Space.xs) {
      HStack {
        SettingsRowLabel(
          title: SpeechSettingsViewModel.componentTitle(asset),
          subtitle: model.statusText(of: asset))
        Spacer()
        switch state {
        case .absent:
          Button("Download") { model.download(asset) }
        case .downloading:
          ProgressView().controlSize(.small)
        case .installed:
          Button("Remove") { Task { await model.remove(asset) } }
        case .failed:
          Button("Retry") { model.download(asset) }
        }
      }
      if case .downloading(let fraction, _) = state, fraction > 0 {
        ProgressView(value: fraction)
          .progressViewStyle(.linear)
      }
      if case .failed(let message) = state {
        SettingsErrorRow(message: "The download did not finish.", details: message)
      }
    }
  }
}

// MARK: - Summaries

struct SummariesSettingsView: View {
  private enum Field: Hashable {
    case server
    case model
    case key
    case context
  }

  @Bindable var model: LLMSettingsViewModel
  @FocusState private var focus: Field?

  var body: some View {
    SectionPage(section: .summaries, model: model) {
      Section {
        Picker("Service", selection: .action({ model.preset }, model.selectPreset)) {
          ForEach(LLMPreset.allCases) { preset in
            Text(preset.title).tag(preset)
          }
        }
        if model.preset.showsServerField {
          TextField(
            "Server address", text: $model.baseURLText,
            prompt: Text(model.preset.baseURL?.absoluteString ?? "https://example.com/v1")
          )
          .focused($focus, equals: .server)
        }
        TextField("Model", text: $model.model, prompt: Text(model.preset.modelPlaceholder))
          .focused($focus, equals: .model)
        SecureField("API key", text: $model.apiKey, prompt: Text(keyPrompt))
          .focused($focus, equals: .key)
        Footnote("Stored in your login keychain and sent only to the server above.")
        if let message = model.validationMessage {
          MessageRow(kind: .warning, text: message)
        }
      }
      Section {
        DisclosureGroup("Advanced") {
          TextField("Context size", text: $model.contextTokensText)
            .focused($focus, equals: .context)
          Footnote(
            "How much text the model can read at once. Leave the default of \(LLMSettingsViewModel.defaultContextTokens.formatted()) unless the service reports a shorter limit."
          )
        }
      }
      Section {
        HStack(alignment: .top) {
          statusRow
          Spacer()
          if model.isConfigured {
            Button("Test again") { Task { await model.test() } }
              .disabled(model.isTesting)
          }
        }
      }
    }
    .commitsFields(focus: focus) { Task { await model.commit() } }
  }

  private var keyPrompt: String {
    model.preset.needsAPIKey
      ? "Paste the key from your \(model.preset.title) account" : "Only if the server needs one"
  }

  @ViewBuilder
  private var statusRow: some View {
    switch model.status {
    case .notConfigured:
      MessageRow(
        kind: .info,
        text: "Not set up. Choose a service and enter a model name; summaries stay off until then.")
    case .unchecked:
      MessageRow(kind: .info, text: "Saved. The connection has not been checked yet.")
    case .checking:
      HStack(spacing: Theme.Space.sm) {
        ProgressView().controlSize(.small)
        Text("Checking the connection…")
          .font(.steno(Theme.TextSize.xs))
          .foregroundStyle(Color.stenoForeground)
      }
    case .connected(let report):
      SettingsStatusRow(kind: .success, message: "Connected.", details: report)
    case .failed(let report):
      SettingsStatusRow(kind: .error, message: "Could not connect to the service.", details: report)
    }
  }
}

// MARK: - Export

struct ExportSettingsView: View {
  private enum Field: Hashable {
    case people
    case tag
  }

  @Bindable var model: ObsidianSettingsViewModel
  @FocusState private var focus: Field?

  var body: some View {
    SectionPage(section: .export, model: model) {
      Section {
        Toggle("Export to Obsidian", isOn: .action({ model.enabled }, model.setEnabled))
        if model.enabled {
          FolderRow(
            label: "Vault", url: model.vaultURL,
            choose: { url in Task { await model.chooseVault(url) } })
          Footnote(
            "Each meeting becomes a folder with a note, the transcript and the tasks. Steno never edits files it did not create."
          )
        }
      }
      if model.enabled {
        Section {
          DisclosureGroup("Advanced") {
            TextField("People folder", text: $model.peopleFolder, prompt: Text("People"))
              .focused($focus, equals: .people)
            Footnote("One page per person, inside the vault. Leave empty for none.")
            TextField("Tag for tasks", text: $model.taskTag, prompt: Text("task"))
              .focused($focus, equals: .tag)
            Toggle(
              "Copy the recording into the vault",
              isOn: .action({ model.includeAudio }, model.setIncludeAudio))
          }
        }
        if let status {
          Section {
            MessageRow(kind: status.kind, text: status.text)
          }
        }
      }
    }
    .commitsFields(focus: focus) { Task { await model.commit() } }
  }

  /// What exporting amounts to right now; nil while the page's own error
  /// row already says why nothing was saved.
  private var status: (kind: MessageRow.Kind, text: String)? {
    if model.needsVault {
      return (.info, "Choose a vault folder to start exporting.")
    }
    if let message = model.validationMessage {
      return (.error, message)
    }
    guard model.error == nil else { return nil }
    return (
      .success, "Exporting to \(model.vaultName). New meetings are written there when they finish."
    )
  }
}

// MARK: - iPhone

struct PhoneSettingsView: View {
  let model: PhonesSettingsViewModel
  @State private var showingPairing = false

  var body: some View {
    SectionPage(section: .iphone, model: model) {
      if !model.isAvailable {
        Section {
          SettingsStatusRow(
            kind: .warning,
            message: "Pairing is unavailable right now. Relaunch Steno to try again.")
        }
      } else {
        Section("Paired phones") {
          if model.devices.isEmpty {
            Footnote("No iPhone paired yet.")
          }
          ForEach(model.devices) { device in
            HStack {
              SettingsRowLabel(title: device.name, subtitle: model.pairedText(device))
              Spacer()
              Button("Remove") { Task { await model.revoke(device.id) } }
                .accessibilityLabel("Remove \(device.name)")
            }
          }
          HStack {
            Spacer()
            Button("Pair an iPhone…") {
              Task {
                await model.beginPairing()
                showingPairing = model.pairing != nil
              }
            }
          }
          Footnote(
            "The first pairing asks for local network access. Recordings travel over your Wi-Fi only, encrypted to this Mac."
          )
        }
        if !model.activeReceipts.isEmpty {
          Section("Receiving") {
            ForEach(model.activeReceipts) { receipt in
              receiptRow(receipt)
            }
          }
        }
        if let failure = model.listenerFailure {
          Section {
            SettingsStatusRow(
              kind: .error, message: "Steno cannot receive recordings right now.", details: failure)
          }
        }
      }
    }
    .task { await model.observe() }
    .task { await model.observeReceipts() }
    .onChange(of: model.pairing == nil) { _, closed in
      if closed { showingPairing = false }
    }
    .sheet(
      isPresented: $showingPairing,
      onDismiss: {
        if model.pairing != nil { Task { await model.cancelPairing() } }
      },
      content: {
        PairingSheet(model: model) { showingPairing = false }
      })
  }

  private func receiptRow(_ receipt: HandoverReceipt) -> some View {
    let progress = PhonesSettingsViewModel.progress(receipt)
    return VStack(alignment: .leading, spacing: Theme.Space.xs) {
      HStack {
        Text("Receiving from \(model.deviceName(for: receipt))…")
          .font(.steno(Theme.TextSize.xs))
        Spacer()
        Text(progress.formatted(.percent.precision(.fractionLength(0))))
          .font(.steno(Theme.TextSize.xxs))
          .monospacedDigit()
          .foregroundStyle(Color.stenoFaint)
      }
      ProgressView(value: progress)
        .progressViewStyle(.linear)
    }
  }
}

/// The QR code for the iPhone app, with its expiry and a Cancel.
struct PairingSheet: View {
  static let width: CGFloat = 360

  let model: PhonesSettingsViewModel
  let dismiss: @MainActor () -> Void

  var body: some View {
    SettingsSheet(title: "Pair an iPhone", dismissal: .cancel, width: Self.width, dismiss: dismiss)
    {
      VStack(spacing: Theme.Space.lg) {
        if let image = model.qrImage {
          Image(nsImage: image)
            .interpolation(.none)
            .resizable()
            .frame(width: SettingsMetrics.qrCodeSize, height: SettingsMetrics.qrCodeSize)
            .padding(Theme.Space.sm)
            // A scanner needs a white quiet zone around the code in both
            // appearances, so this is the one surface that is not a veil.
            .background(Color.white)
            .clipShape(Theme.Radius.md.shape)
            .overlay(
              Theme.Radius.md.shape.hairline()
            )
            .accessibilityLabel("Pairing code for the Steno iPhone app")
        }
        Text(caption)
          .font(.steno(Theme.TextSize.xs))
          .foregroundStyle(Color.stenoMutedForeground)
          .multilineTextAlignment(.center)
          .fixedSize(horizontal: false, vertical: true)
      }
      .frame(maxWidth: .infinity)
    }
    // The phone's arrival closes the code; polled on the app's clock.
    .task(id: model.pairing?.expiresAt) { await model.observePairing() }
  }

  private var caption: String {
    ["Scan this code with the Steno app on your iPhone.", model.pairingExpiryText]
      .compactMap { $0 }
      .joined(separator: " ")
  }
}

extension AppController {
  /// The one way a button lands on a Settings section: record the request,
  /// open the scene through the view's `openSettings` environment action and
  /// bring the app to the front. Used by the setup banner, the detail rows
  /// and the footer. Lives here, not in `AppController.swift`, because
  /// `OpenSettingsAction` needs SwiftUI, whose `Settings` scene would shadow
  /// the model type the controller names.
  func openSettings(_ section: SettingsSection, with open: OpenSettingsAction) {
    openSettings(section)
    open()
    NSApp.activate()
  }
}
