import SwiftUI

/// The onboarding window: one row per permission, the current one expanded
/// with its action. Closes itself when the required permissions are granted
/// and the optional ones were answered or skipped.
struct OnboardingView: View {
  @State var model: OnboardingViewModel
  let onFinished: () -> Void

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.lg) {
      VStack(alignment: .leading, spacing: Theme.Space.xs) {
        Text("Welcome to Steno")
          .font(.steno(Theme.TextSize.xxl, weight: .semibold))
          .foregroundStyle(Color.stenoStrong)
        Text("A few permissions before the first recording. Audio never leaves this Mac.")
          .font(.steno(Theme.TextSize.sm))
          .foregroundStyle(Color.stenoMutedForeground)
      }
      VStack(spacing: Theme.Space.sm) {
        ForEach(model.steps) { step in
          stepRow(step)
        }
      }
      HStack {
        Spacer()
        if model.isComplete {
          Button("Done") { onFinished() }
            .buttonStyle(StenoPrimaryButtonStyle())
            .keyboardShortcut(.defaultAction)
        } else {
          Button("Later") { onFinished() }
            .buttonStyle(StenoSecondaryButtonStyle())
            .keyboardShortcut(.cancelAction)
        }
      }
    }
    .padding(Theme.Space.xl)
    .frame(width: 520)
    .background(Color.stenoBackground)
    .task { await model.load() }
    .onChange(of: model.isFinished) { _, finished in
      if finished { onFinished() }
    }
  }

  private func stepRow(_ step: OnboardingViewModel.Step) -> some View {
    let isCurrent = model.current == step.kind && step.state != .granted
    return Card {
      VStack(alignment: .leading, spacing: Theme.Space.sm) {
        HStack(spacing: Theme.Space.sm) {
          stateIcon(step)
          Text(step.kind.title)
            .font(.steno(Theme.TextSize.sm, weight: .semibold))
            .foregroundStyle(Color.stenoStrong)
          if !step.isRequired {
            StatusChip(text: "Optional", color: Color.stenoFaint)
          }
          Spacer()
          if model.skipped.contains(step.kind) {
            Text("Skipped").font(.steno(Theme.TextSize.xxs)).foregroundStyle(Color.stenoFaint)
          }
        }
        if isCurrent || step.state == .denied {
          Text(step.kind.explanation)
            .font(.steno(Theme.TextSize.xs))
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
                .buttonStyle(.plain)
                .font(.steno(Theme.TextSize.xs))
                .foregroundStyle(Color.stenoFaint)
            }
          }
        }
      }
    }
  }

  @ViewBuilder
  private func stateIcon(_ step: OnboardingViewModel.Step) -> some View {
    switch step.state {
    case .granted:
      Image(systemName: "checkmark.circle.fill").foregroundStyle(Color.stenoLiveBright)
    case .denied:
      Image(systemName: "xmark.circle.fill").foregroundStyle(Color.stenoDestructive)
    case .unknown:
      Image(systemName: "circle").foregroundStyle(Color.stenoGhost)
    }
  }
}
