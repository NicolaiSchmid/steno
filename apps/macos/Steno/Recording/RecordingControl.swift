import StenoAudio
import SwiftUI

/// The primary control at the top of the sidebar: starts a call recording
/// (in person one click away in the chevron menu), turns into Stop with the
/// elapsed time and the level bars while recording, and is disabled with a
/// reason while a required permission is denied. Drives the one recorder
/// through `AppController.startRecordingFromWindow`, so the live row is
/// selected; the menu bar item drives the same recorder and never moves the
/// selection. The permission report refreshes when the control appears;
/// `AppController` refreshes it again whenever the app becomes active.
struct RecordingControl: View {
  let controller: AppController
  @Environment(\.openWindow) private var openWindow

  private var recorder: RecordingController { controller.recorder }

  private var presentation: RecordingControlPresentation {
    RecordingControlPresentation.make(
      state: recorder.recording, denied: recorder.deniedPermissions,
      autoStop: recorder.autoStop?.presentation)
  }

  var body: some View {
    let presentation = self.presentation
    VStack(spacing: 0) {
      VStack(alignment: .leading, spacing: Theme.Space.sm) {
        control(presentation)
          .frame(maxWidth: .infinity)
        if let autoStop = presentation.autoStop {
          AutoStopRow(presentation: autoStop, identifier: "sidebar-keep-recording") {
            recorder.keepRecording()
          }
        }
        if let levels = recorder.levels, case .recording = recorder.recording {
          LevelBars(levels: levels)
        }
        if let reason = presentation.disabledReason {
          MessageRow(kind: .warning, text: reason)
          Button("Fix permissions…") { openWindow(id: "onboarding") }
            .buttonStyle(StenoSecondaryButtonStyle())
            .accessibilityIdentifier("sidebar-fix-permissions")
        }
        RecordingMessages(warning: recorder.lastWarning, error: recorder.lastError)
      }
      .padding(Theme.Space.md)
      .frame(maxWidth: .infinity)
      Divider().overlay(Color.stenoBorder)
    }
    .task { await recorder.refreshPermissions() }
  }

  @ViewBuilder
  private func control(_ presentation: RecordingControlPresentation) -> some View {
    switch recorder.recording {
    case .idle:
      HStack(spacing: 0) {
        Button {
          start(.call)
        } label: {
          Text(presentation.label)
            .frame(maxWidth: .infinity)
        }
        .buttonStyle(StenoPrimaryButtonStyle())
        .disabled(!presentation.isEnabled)
        .help("Record a call (⌘⇧R)")
        .accessibilityIdentifier("sidebar-record")
        if presentation.offersInPerson {
          Rectangle()
            .fill(Color.stenoPrimaryForeground.opacity(0.24))
            .frame(width: Theme.Space.hairline)
          Menu {
            Button("Record in person") { start(.inPerson) }
          } label: {
            Image(systemName: "chevron.down")
              .font(.steno(Theme.TextSize.xxs, weight: .semibold))
          }
          .menuStyle(.button)
          .buttonStyle(StenoPrimaryButtonStyle())
          .menuIndicator(.hidden)
          .fixedSize()
          .help("Record in person")
          .accessibilityLabel("Record in person")
          .accessibilityIdentifier("sidebar-record-in-person")
        }
      }
      .fixedSize(horizontal: false, vertical: true)
    // The busy states are a disabled button so the control keeps its box
    // while the spinner stands in for the label; the redesign owns the
    // spinner size.
    case .starting:
      Button(action: {}) {
        ProgressView()
          .controlSize(.small)
          .tint(Color.stenoPrimaryForeground)
          .frame(maxWidth: .infinity)
      }
      .buttonStyle(StenoPrimaryButtonStyle())
      .disabled(true)
      .accessibilityLabel(presentation.label)
    case .recording(let since):
      StopButton(state: .stop(since: since), fillsWidth: true, id: "sidebar-stop") {
        Task { await recorder.stop() }
      }
    case .stopping:
      StopButton(state: .stopping, fillsWidth: true, id: "sidebar-stop") {}
    }
  }

  private func start(_ mode: CaptureMode) {
    Task { await controller.startRecordingFromWindow(mode: mode) }
  }
}
