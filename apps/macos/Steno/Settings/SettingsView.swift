import AppKit
import StenoAudio
import StenoCore
import StenoSpeech
import SwiftUI

/// The Settings scene: seven tabs, one view model each, built once for the
/// scene's lifetime (a model created in `body` would be replaced on every
/// evaluation and its state lost).
struct SettingsView: View {
  let controller: AppController
  @State private var general: GeneralSettingsViewModel
  @State private var audio: AudioSettingsViewModel
  @State private var speech: SpeechSettingsViewModel
  @State private var llm: LLMSettingsViewModel
  @State private var obsidian: ObsidianSettingsViewModel
  @State private var phones: PhonesSettingsViewModel

  init(controller: AppController) {
    self.controller = controller
    let environment = controller.environment
    _general = State(initialValue: GeneralSettingsViewModel(environment: environment))
    _audio = State(initialValue: AudioSettingsViewModel(environment: environment))
    _speech = State(initialValue: SpeechSettingsViewModel(environment: environment))
    _llm = State(initialValue: LLMSettingsViewModel(environment: environment))
    _obsidian = State(initialValue: ObsidianSettingsViewModel(environment: environment))
    _phones = State(initialValue: PhonesSettingsViewModel(environment: environment))
  }

  var body: some View {
    TabView {
      GeneralSettingsView(model: general)
        .tabItem { Label("General", systemImage: "gearshape") }
      AudioSettingsView(model: audio)
        .tabItem { Label("Audio", systemImage: "mic") }
      SpeechSettingsView(model: speech)
        .tabItem { Label("Speech", systemImage: "waveform") }
      LLMSettingsView(model: llm)
        .tabItem { Label("LLM", systemImage: "brain") }
      ObsidianSettingsView(model: obsidian)
        .tabItem { Label("Obsidian", systemImage: "folder") }
      PhonesSettingsView(model: phones)
        .tabItem { Label("Phones", systemImage: "iphone") }
      UpdatesSettingsView(updater: controller.environment.updater)
        .tabItem { Label("Updates", systemImage: "arrow.down.circle") }
    }
    .frame(width: 560)
    .background(Color.stenoBackground)
  }
}

// MARK: - General

struct GeneralSettingsView: View {
  let model: GeneralSettingsViewModel

  var body: some View {
    Form {
      Section {
        Toggle(
          "Launch Steno at login",
          isOn: .action({ model.launchAtLogin }, model.setLaunchAtLogin))
        if model.loginItem == .requiresApproval {
          HStack {
            Text("Waiting for approval in System Settings > Login Items.")
              .font(.steno(Theme.TextSize.xs))
              .foregroundStyle(Color.stenoWarning)
            Button("Open Login Items") { model.openLoginItemSettings() }
          }
        }
        Toggle(
          "Offer to record when another app opens the microphone",
          isOn: .action({ model.detectionEnabled }, model.setDetectionEnabled))
      }
      Section("Summary") {
        Picker(
          "Default template",
          selection: .action({ model.defaultTemplateID }, model.setDefaultTemplate)
        ) {
          ForEach(model.templates) { template in
            Text(template.displayName).tag(template.id)
          }
        }
        if let template = model.templates.first(where: { $0.id == model.defaultTemplateID }) {
          Text(template.description)
            .font(.steno(Theme.TextSize.xs))
            .foregroundStyle(Color.stenoFaint)
        }
      }
      if let error = model.error { MessageRow(kind: .error, text: error) }
    }
    .formStyle(.grouped)
    .task { await model.load() }
  }
}

// MARK: - Audio

struct AudioSettingsView: View {
  let model: AudioSettingsViewModel

  var body: some View {
    Form {
      Section("Input") {
        Picker("Microphone", selection: .action({ model.inputDeviceUID }, model.setInputDevice)) {
          Text("System default").tag(String?.none)
          ForEach(model.devices) { device in
            Text(device.name).tag(String?.some(device.uid))
          }
        }
        Button("Refresh devices") { model.refreshDevices() }
      }
      Section("Recordings") {
        HStack {
          Text(model.audioFolder.path)
            .font(.steno(Theme.TextSize.xs))
            .foregroundStyle(Color.stenoMutedForeground)
            .lineLimit(1)
            .truncationMode(.middle)
          Spacer()
          Button("Choose…") { chooseFolder() }
        }
        Picker(
          "Keep audio",
          selection: .action(
            { model.retentionMode },
            { mode in await model.setRetention(mode: mode, days: model.retentionDays) })
        ) {
          ForEach(AudioSettingsViewModel.RetentionMode.allCases) { mode in
            Text(mode.title).tag(mode)
          }
        }
        if model.retentionMode == .keepDays {
          Stepper(
            "\(model.retentionDays) days",
            value: .action(
              { model.retentionDays },
              { days in await model.setRetention(mode: .keepDays, days: days) }),
            in: 1...3650)
        }
        Text("Speaker sample clips stay until the speaker is named, whatever the retention.")
          .font(.steno(Theme.TextSize.xs))
          .foregroundStyle(Color.stenoFaint)
      }
      if let error = model.error { MessageRow(kind: .error, text: error) }
    }
    .formStyle(.grouped)
    .task { await model.load() }
  }

  private func chooseFolder() {
    let panel = NSOpenPanel()
    panel.canChooseDirectories = true
    panel.canChooseFiles = false
    panel.canCreateDirectories = true
    panel.allowsMultipleSelection = false
    panel.directoryURL = model.audioFolder
    panel.prompt = "Use folder"
    if panel.runModal() == .OK, let url = panel.url {
      Task { await model.setAudioFolder(url) }
    }
  }
}

// MARK: - Speech

struct SpeechSettingsView: View {
  let model: SpeechSettingsViewModel

  var body: some View {
    Form {
      Section("Engine") {
        Picker("Speech engine", selection: .action({ model.engineID }, model.setEngine)) {
          ForEach(model.engines, id: \.self) { engine in
            Text(engine.asset.displayName).tag(engine)
          }
        }
        Text("Transcription and diarization run on this Mac. Models download once.")
          .font(.steno(Theme.TextSize.xs))
          .foregroundStyle(Color.stenoFaint)
      }
      Section("Models") {
        ForEach(model.assets, id: \.self) { asset in
          assetRow(asset)
        }
      }
      if let error = model.error { MessageRow(kind: .error, text: error) }
    }
    .formStyle(.grouped)
    .task { await model.load() }
  }

  private func assetRow(_ asset: ModelAsset) -> some View {
    VStack(alignment: .leading, spacing: Theme.Space.xs) {
      HStack {
        VStack(alignment: .leading, spacing: 2) {
          Text(asset.displayName)
            .font(.steno(Theme.TextSize.sm))
          Text(
            "\(SpeechSettingsViewModel.formatBytes(asset.approximateBytes)) · \(asset.licence) · \(asset.sourceRepo)"
          )
          .font(.steno(Theme.TextSize.xxs))
          .foregroundStyle(Color.stenoFaint)
        }
        Spacer()
        switch model.state(of: asset) {
        case .absent:
          Button("Download") { model.download(asset) }
        case .downloading:
          ProgressView().controlSize(.small)
        case .installed(let bytes):
          StatusChip(
            text: bytes.map { "Installed · \(SpeechSettingsViewModel.formatBytes($0))" }
              ?? "Installed",
            color: Color.stenoLive)
          Button("Remove") { Task { await model.remove(asset) } }
        case .failed:
          Button("Retry") { model.download(asset) }
        }
      }
      if case .downloading(let fraction, let phase) = model.state(of: asset) {
        ProgressView(value: fraction)
          .progressViewStyle(.linear)
        Text(phase)
          .font(.steno(Theme.TextSize.xxs))
          .foregroundStyle(Color.stenoFaint)
      }
      if case .failed(let message) = model.state(of: asset) {
        MessageRow(kind: .error, text: message)
      }
    }
  }
}

// MARK: - LLM

struct LLMSettingsView: View {
  @Bindable var model: LLMSettingsViewModel

  var body: some View {
    Form {
      Section("Endpoint") {
        TextField("Base URL", text: $model.baseURLText, prompt: Text("http://127.0.0.1:1234/v1"))
        TextField("Model", text: $model.model, prompt: Text("gpt-4.1-mini"))
        TextField("Context window (tokens)", text: $model.contextTokensText)
        SecureField("API key", text: $model.apiKey, prompt: Text("optional for local servers"))
        Text("Only transcript text is sent to the endpoint. The key is stored in your login keychain.")
          .font(.steno(Theme.TextSize.xs))
          .foregroundStyle(Color.stenoFaint)
        if let message = model.validationMessage {
          MessageRow(kind: .warning, text: message)
        }
      }
      Section {
        HStack {
          Button("Save") { Task { await model.save() } }
            .disabled(model.validationMessage != nil)
          Button("Test connection") { Task { await model.test() } }
            .disabled(model.isTesting || model.baseURL == nil)
          if model.isTesting { ProgressView().controlSize(.small) }
        }
        if !model.isConfigured {
          MessageRow(
            kind: .info,
            text:
              "No endpoint configured: summaries use a placeholder until a base URL and model are saved."
          )
        }
        switch model.testResult {
        case .success(let text): MessageRow(kind: .info, text: text)
        case .failure(let text): MessageRow(kind: .error, text: text)
        case nil: EmptyView()
        }
      }
      if let error = model.error { MessageRow(kind: .error, text: error) }
    }
    .formStyle(.grouped)
    .task { await model.load() }
  }
}

// MARK: - Obsidian

struct ObsidianSettingsView: View {
  @Bindable var model: ObsidianSettingsViewModel

  var body: some View {
    Form {
      Section {
        Toggle("Deliver meetings into an Obsidian vault", isOn: $model.enabled)
      }
      if model.enabled {
        Section("Vault") {
          HStack {
            TextField("Vault folder", text: $model.vaultPath)
            Button("Choose…") { chooseVault() }
          }
          TextField(
            "People folder (relative, optional)", text: $model.peopleFolder,
            prompt: Text("People"))
          TextField("Task tag (optional)", text: $model.taskTag, prompt: Text("task"))
          Toggle("Copy the audio into the vault", isOn: $model.includeAudio)
        }
      }
      Section {
        HStack {
          Button("Save") { Task { await model.save() } }
          if model.saved {
            Text("Saved. Takes effect on the next delivery.")
              .font(.steno(Theme.TextSize.xs))
              .foregroundStyle(Color.stenoLive)
          }
        }
        if let message = model.validationMessage {
          MessageRow(kind: .error, text: message)
        }
        Text(
          "Steno writes Meetings/<date>-<slug>/ with a folder note, transcript, tasks, VTT and JSON. Files it did not write are never touched."
        )
        .font(.steno(Theme.TextSize.xs))
        .foregroundStyle(Color.stenoFaint)
      }
      if let error = model.error { MessageRow(kind: .error, text: error) }
    }
    .formStyle(.grouped)
    .task { await model.load() }
  }

  private func chooseVault() {
    let panel = NSOpenPanel()
    panel.canChooseDirectories = true
    panel.canChooseFiles = false
    panel.allowsMultipleSelection = false
    panel.prompt = "Use vault"
    if panel.runModal() == .OK, let url = panel.url {
      model.vaultPath = url.path
    }
  }
}

// MARK: - Phones

struct PhonesSettingsView: View {
  let model: PhonesSettingsViewModel

  var body: some View {
    Form {
      if !model.isAvailable {
        Section {
          MessageRow(
            kind: .warning,
            text:
              "Phone handover is unavailable: the handover identity could not be created in the login keychain. Relaunch Steno to try again."
          )
        }
      } else {
        Section("Paired phones") {
          if model.devices.isEmpty {
            Text("No phone paired yet.")
              .font(.steno(Theme.TextSize.xs))
              .foregroundStyle(Color.stenoFaint)
          }
          ForEach(model.devices) { device in
            HStack {
              VStack(alignment: .leading, spacing: 2) {
                Text(device.name).font(.steno(Theme.TextSize.sm))
                Text(
                  "Paired \(device.pairedAt.formatted(date: .abbreviated, time: .shortened))"
                    + (device.lastSeenAt.map {
                      " · seen \($0.formatted(date: .abbreviated, time: .shortened))"
                    } ?? ""))
                  .font(.steno(Theme.TextSize.xxs))
                  .foregroundStyle(Color.stenoFaint)
              }
              Spacer()
              Button("Remove") { Task { await model.revoke(device.id) } }
                .accessibilityLabel("Remove \(device.name)")
            }
          }
        }
        Section("Pair a phone") {
          if let pairing = model.pairing, model.pairingIsOpen {
            VStack(alignment: .leading, spacing: Theme.Space.sm) {
              if let image = model.qrImage {
                Image(nsImage: image)
                  .interpolation(.none)
                  .resizable()
                  .frame(width: 200, height: 200)
                  .background(Color.white)
                  .padding(Theme.Space.sm)
                  .accessibilityLabel("Pairing code for the Steno iPhone app")
              }
              Text(
                "Scan with the Steno iPhone app. Expires \(pairing.expiresAt.formatted(date: .omitted, time: .standard))."
              )
              .font(.steno(Theme.TextSize.xs))
              .foregroundStyle(Color.stenoMutedForeground)
              Button("Cancel pairing") { Task { await model.cancelPairing() } }
            }
            // The phone's arrival closes the code; polled on the app's clock.
            .task(id: pairing.expiresAt) { await model.observePairing() }
          } else {
            Button("Show pairing code") { Task { await model.beginPairing() } }
            Text(
              "The first pairing asks for local network access. Recordings travel over your Wi-Fi only, encrypted to this Mac."
            )
            .font(.steno(Theme.TextSize.xs))
            .foregroundStyle(Color.stenoFaint)
          }
        }
        Section("Listener") {
          HStack {
            Circle()
              .fill(listenerColor)
              .frame(width: 8, height: 8)
            Text(model.listenerText).font(.steno(Theme.TextSize.xs))
            Spacer()
            if let macID = model.macID {
              Text(macID)
                .font(.steno(Theme.TextSize.xxxs).monospaced())
                .foregroundStyle(Color.stenoGhost)
            }
          }
          ForEach(
            model.receipts.filter { $0.state.kind == .receiving || $0.state.kind == .verifying }
          ) { receipt in
            VStack(alignment: .leading, spacing: 2) {
              Text("Receiving \(receipt.recordingID.uuidString.prefix(8))…")
                .font(.steno(Theme.TextSize.xxs))
              ProgressView(value: PhonesSettingsViewModel.progress(receipt))
                .progressViewStyle(.linear)
            }
          }
        }
      }
      if let error = model.error { MessageRow(kind: .error, text: error) }
    }
    .formStyle(.grouped)
    .task { await model.load() }
    .task { await model.observe() }
    .task { await model.observeReceipts() }
  }

  private var listenerColor: Color {
    switch model.listener {
    case .listening: Color.stenoLiveBright
    case .stopped: Color.stenoGhost
    case .failed: Color.stenoDestructive
    }
  }
}

// MARK: - Updates

/// Sparkle's automatic check, Check now, the last check and the running
/// version, straight off `UpdaterControlling`.
struct UpdatesSettingsView: View {
  let updater: any UpdaterControlling

  private var version: String {
    let info = Bundle.main.infoDictionary ?? [:]
    let marketing = info["CFBundleShortVersionString"] as? String ?? "0.0.0"
    return "\(marketing) (\(info["CFBundleVersion"] as? String ?? "0"))"
  }

  var body: some View {
    Form {
      Section {
        Toggle(
          "Check for updates automatically",
          isOn: Binding(
            get: { updater.automaticallyChecksForUpdates },
            set: { updater.automaticallyChecksForUpdates = $0 }))
        HStack {
          Button("Check now") { updater.checkForUpdates() }
            .disabled(!updater.canCheckForUpdates)
          if let last = updater.lastUpdateCheckDate {
            Text("Last checked \(last.formatted(date: .abbreviated, time: .shortened))")
              .font(.steno(Theme.TextSize.xs))
              .foregroundStyle(Color.stenoFaint)
          }
        }
      }
      Section {
        Text("Steno \(version)")
          .font(.steno(Theme.TextSize.xs))
          .foregroundStyle(Color.stenoMutedForeground)
        Text(
          "Updates are signed releases from github.com/NicolaiSchmid/steno, delivered by Sparkle."
        )
        .font(.steno(Theme.TextSize.xs))
        .foregroundStyle(Color.stenoFaint)
      }
    }
    .formStyle(.grouped)
  }
}
