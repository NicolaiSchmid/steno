import AppKit
import SwiftUI

/// The onboarding window, two pages. Page 1: one row per permission, the
/// current one expanded with its action; Later or Done (and every step
/// handled) advance to page 2. Page 2, "Summaries and export": the LLM
/// endpoint and the Obsidian vault rows over the Settings tabs' view
/// models; Back returns, Finish or both rows handled set the model's
/// `finished`, which dismisses the window. The window's own close button
/// marks onboarding completed too (`onDisappear`). The look is the
/// redesign plan's: a hidden title bar (the H1 is the only title), 560 pt
/// of content at 40 pt top and 32 pt sides and bottom, raised cards 12 pt
/// apart, neutral chips, and a subtitle that wraps instead of truncating.
struct OnboardingView: View {
  @State var model: OnboardingViewModel
  let onFinished: () -> Void

  static let contentWidth: CGFloat = 560
  /// The row glyph's frame and size: a 24 pt box holding an 18 pt symbol,
  /// so every state glyph shares one optical size beside the 14 pt title.
  static let glyphFrame: CGFloat = 24
  static let glyphSize: CGFloat = 18
  /// The explanation's leading, 13/17.
  private static let explanationLeading = Theme.TextSize.xs.lineHeight - Theme.TextSize.xs.size

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.xl) {
      switch model.page {
      case .permissions: permissionsPage
      case .setup: setupPage
      }
    }
    .padding(.top, Theme.Space.xxl + Theme.Space.sm)
    .padding([.horizontal, .bottom], Theme.Space.xxl)
    .frame(width: Self.contentWidth)
    .background(Color.stenoBackground)
    .task { await model.load() }
    .onChange(of: model.permissionsHandled) { _, handled in
      if handled, model.page == .permissions { model.advance() }
    }
    .onChange(of: model.finished) { _, finished in
      if finished { onFinished() }
    }
    .onDisappear { model.markCompleted() }
  }

  /// Step caption 12 `faint`, 4 pt, H1 26 semibold with the ladder's
  /// tracking, 6 pt, the subtitle 14 `muted` wrapping to the content width.
  private func heading(step: Int, title: String, intro: String) -> some View {
    VStack(alignment: .leading, spacing: 0) {
      Text("Step \(step) of 2")
        .font(.steno(Theme.TextSize.xxs))
        .foregroundStyle(Color.stenoFaint)
        .accessibilityIdentifier("onboarding-step")
        .padding(.bottom, Theme.Space.xs)
      Text(title)
        .font(.steno(Theme.TextSize.xxl, weight: .semibold))
        .tracking(-0.3)
        .foregroundStyle(Color.stenoStrong)
        .accessibilityIdentifier("onboarding-title")
        .padding(.bottom, Theme.Space.sm - Theme.Space.xxs)
      Text(intro)
        .font(.steno(Theme.TextSize.sm))
        .foregroundStyle(Color.stenoMutedForeground)
        .frame(maxWidth: .infinity, alignment: .leading)
        .fixedSize(horizontal: false, vertical: true)
        .accessibilityIdentifier("onboarding-intro")
    }
  }

  // MARK: - Page 1

  @ViewBuilder
  private var permissionsPage: some View {
    VStack(alignment: .leading, spacing: Theme.Space.xs) {
      heading(
        step: 1, title: "Welcome to Steno",
        intro:
          "A few permissions, then where summaries come from and where meetings go. Audio never leaves this Mac."
      )
      if let sentence = model.retentionSentence {
        Text(sentence)
          .font(.steno(Theme.TextSize.xs))
          .foregroundStyle(Color.stenoFaint)
          .frame(maxWidth: .infinity, alignment: .leading)
          .fixedSize(horizontal: false, vertical: true)
          .accessibilityIdentifier("onboarding-retention")
      }
    }
    VStack(spacing: Theme.Space.md) {
      ForEach(model.steps) { step in
        stepRow(step)
      }
    }
    HStack {
      Spacer()
      if model.isComplete {
        Button("Done") { model.advance() }
          .buttonStyle(StenoPrimaryButtonStyle())
          .keyboardShortcut(.defaultAction)
          .accessibilityIdentifier("onboarding-done")
      } else {
        Button("Later") { model.advance() }
          .buttonStyle(StenoSecondaryButtonStyle())
          .keyboardShortcut(.cancelAction)
          .accessibilityIdentifier("onboarding-later")
      }
    }
  }

  /// A permission row: the 24 pt glyph frame, the title, the neutral
  /// "Optional" chip and "Skipped" trailing; expanded, the explanation at
  /// 13/17 and the action row 12 pt below each.
  private func stepRow(_ step: OnboardingViewModel.Step) -> some View {
    let isCurrent = model.current == step.kind && step.state != .granted
    return Card {
      VStack(alignment: .leading, spacing: Theme.Space.md) {
        HStack(spacing: Theme.Space.sm) {
          stateIcon(step)
          Text(step.kind.title)
            .font(.steno(Theme.TextSize.sm, weight: .semibold))
            .foregroundStyle(Color.stenoStrong)
            .accessibilityIdentifier("onboarding-step-\(step.kind.rawValue)")
          if !step.isRequired {
            StatusChip(text: "Optional", style: .neutral)
          }
          Spacer()
          if model.skipped.contains(step.kind) {
            Text("Skipped").font(.steno(Theme.TextSize.xxs)).foregroundStyle(Color.stenoFaint)
          }
        }
        if isCurrent || step.state == .denied {
          Text(step.kind.explanation)
            .font(.steno(Theme.TextSize.xs))
            .lineSpacing(Self.explanationLeading)
            .foregroundStyle(Color.stenoMutedForeground)
            .fixedSize(horizontal: false, vertical: true)
          HStack(spacing: Theme.Space.sm) {
            if step.state == .denied {
              Button("Open System Settings") { model.openSystemSettings(step.kind) }
                .buttonStyle(StenoPrimaryButtonStyle())
              Button("Check again") { Task { await model.load() } }
                .buttonStyle(StenoSecondaryButtonStyle())
            } else if step.kind == .localNetwork {
              Button("Got it") { model.skip(step.kind) }
                .buttonStyle(StenoSecondaryButtonStyle())
            } else {
              Button(step.kind == .systemAudio ? "Run the test recording" : "Allow") {
                Task { await model.request(step.kind) }
              }
              .buttonStyle(StenoPrimaryButtonStyle())
              .disabled(model.requesting != nil)
              if model.requesting == step.kind {
                ProgressView().controlSize(.small)
                if step.kind == .systemAudio {
                  Text("Listening for the test tone, up to 30 seconds…")
                    .font(.steno(Theme.TextSize.xxs))
                    .foregroundStyle(Color.stenoFaint)
                }
              }
            }
            if !step.isRequired, step.kind != .localNetwork {
              Button("Skip") { model.skip(step.kind) }
                .buttonStyle(StenoGhostButtonStyle())
            }
          }
        }
      }
    }
  }

  private func stateIcon(_ step: OnboardingViewModel.Step) -> some View {
    switch step.state {
    case .granted: glyph("checkmark.circle.fill", Color.stenoLiveBright)
    case .denied: glyph("xmark.circle.fill", Color.stenoDestructive)
    case .unknown: glyph("circle", Color.stenoGhost)
    }
  }

  /// An 18 pt state symbol in a 24 pt frame.
  private func glyph(_ systemName: String, _ color: Color) -> some View {
    Image(systemName: systemName)
      .font(.system(size: Self.glyphSize))
      .foregroundStyle(color)
      .frame(width: Self.glyphFrame, height: Self.glyphFrame)
      .accessibilityHidden(true)
  }

  // MARK: - Page 2

  @ViewBuilder
  private var setupPage: some View {
    heading(
      step: 2, title: "Summaries and export",
      intro: "Optional. Steno works as a local transcript recorder without either.")
    VStack(spacing: Theme.Space.md) {
      ForEach(OnboardingViewModel.SetupStep.allCases) { step in
        setupRow(step)
      }
    }
    HStack(spacing: Theme.Space.sm) {
      Spacer()
      Button("Back") { model.back() }
        .buttonStyle(StenoSecondaryButtonStyle())
        .accessibilityIdentifier("onboarding-back")
      Button("Finish") { model.finish() }
        .buttonStyle(StenoPrimaryButtonStyle())
        .keyboardShortcut(.defaultAction)
        .accessibilityIdentifier("onboarding-finish")
    }
  }

  private func setupRow(_ step: OnboardingViewModel.SetupStep) -> some View {
    let state = model.setupState(of: step)
    return Card {
      VStack(alignment: .leading, spacing: Theme.Space.md) {
        HStack(spacing: Theme.Space.sm) {
          setupIcon(state)
          Text(step.title)
            .font(.steno(Theme.TextSize.sm, weight: .semibold))
            .foregroundStyle(Color.stenoStrong)
            .accessibilityIdentifier("onboarding-setup-\(step.id)")
          StatusChip(text: "Optional", style: .neutral)
          Spacer()
          switch state {
          case .saved(let line):
            Text(line)
              .font(.steno(Theme.TextSize.xxs))
              .foregroundStyle(Color.stenoFaint)
              .lineLimit(1)
              .truncationMode(.middle)
          case .skipped:
            Text("Skipped").font(.steno(Theme.TextSize.xxs)).foregroundStyle(Color.stenoFaint)
          case .open:
            EmptyView()
          }
        }
        if state == .open {
          Text(step.explanation)
            .font(.steno(Theme.TextSize.xs))
            .lineSpacing(Self.explanationLeading)
            .foregroundStyle(Color.stenoMutedForeground)
            .fixedSize(horizontal: false, vertical: true)
          switch step {
          case .summaries:
            if let llm = model.llm {
              SummariesSetupFields(model: model, llm: llm)
            }
          case .vault:
            if let obsidian = model.obsidian {
              VaultSetupFields(model: model, obsidian: obsidian)
            }
          }
          Text(step.footnote)
            .font(.steno(Theme.TextSize.xxs))
            .foregroundStyle(Color.stenoFaint)
        }
      }
    }
  }

  private func setupIcon(_ state: OnboardingViewModel.SetupState) -> some View {
    switch state {
    case .saved: glyph("checkmark.circle.fill", Color.stenoLiveBright)
    case .open, .skipped: glyph("circle", Color.stenoGhost)
    }
  }
}

/// The Summaries row's fields: the LLM tab's base URL, model and API key,
/// Test connection, Save and Skip, over the LLM tab's own view model.
private struct SummariesSetupFields: View {
  let model: OnboardingViewModel
  @Bindable var llm: LLMSettingsViewModel

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.sm) {
      StenoTextField("http://127.0.0.1:1234/v1", text: $llm.baseURLText)
        .accessibilityLabel("Base URL")
        .accessibilityIdentifier("onboarding-llm-url")
      StenoTextField("gpt-4.1-mini", text: $llm.model)
        .accessibilityLabel("Model")
        .accessibilityIdentifier("onboarding-llm-model")
      StenoSecureField("optional for local servers", text: $llm.apiKey)
        .accessibilityLabel("API key")
        .accessibilityIdentifier("onboarding-llm-key")
      if let message = llm.validationMessage {
        MessageRow(kind: .warning, text: message)
      }
      HStack(spacing: Theme.Space.sm) {
        Button("Test connection") { Task { await llm.test() } }
          .buttonStyle(StenoSecondaryButtonStyle())
          .disabled(llm.isTesting || llm.baseURL == nil)
        Button("Save") { Task { await model.saveSummaries() } }
          .buttonStyle(StenoPrimaryButtonStyle())
          .disabled(!model.canSaveSummaries)
          .accessibilityIdentifier("onboarding-llm-save")
        Button("Skip") { model.skipSetup(.summaries) }
          .buttonStyle(StenoGhostButtonStyle())
        if llm.isTesting { ProgressView().controlSize(.small) }
      }
      switch llm.testResult {
      case .success(let text): MessageRow(kind: .info, text: text)
      case .failure(let text): MessageRow(kind: .error, text: text)
      case nil: EmptyView()
      }
      if let error = llm.error { MessageRow(kind: .error, text: error) }
    }
  }
}

/// The Obsidian vault row's controls: the vault path with Choose…, Save and
/// Skip, over the Obsidian tab's own view model.
private struct VaultSetupFields: View {
  let model: OnboardingViewModel
  @Bindable var obsidian: ObsidianSettingsViewModel

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.sm) {
      HStack(spacing: Theme.Space.sm) {
        StenoTextField("Vault folder", text: $obsidian.vaultPath)
          .accessibilityIdentifier("onboarding-vault-path")
        Button("Choose…") { chooseVault() }
          .buttonStyle(StenoSecondaryButtonStyle())
      }
      if let message = obsidian.validationMessage {
        MessageRow(kind: .error, text: message)
      }
      HStack(spacing: Theme.Space.sm) {
        Button("Save") { Task { await model.saveVault() } }
          .buttonStyle(StenoPrimaryButtonStyle())
          .disabled(obsidian.vaultPath.trimmingCharacters(in: .whitespaces).isEmpty)
          .accessibilityIdentifier("onboarding-vault-save")
        Button("Skip") { model.skipSetup(.vault) }
          .buttonStyle(StenoGhostButtonStyle())
      }
      if let error = obsidian.error { MessageRow(kind: .error, text: error) }
    }
  }

  private func chooseVault() {
    let panel = NSOpenPanel()
    panel.canChooseDirectories = true
    panel.canChooseFiles = false
    panel.allowsMultipleSelection = false
    panel.prompt = "Use vault"
    if panel.runModal() == .OK, let url = panel.url {
      obsidian.vaultPath = url.path
    }
  }
}
