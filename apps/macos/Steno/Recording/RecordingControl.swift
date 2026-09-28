import AppKit
import Combine
import StenoAudio
import SwiftUI

/// The primary control at the top of the sidebar: starts a call recording
/// (in person one click away in the chevron menu), turns into Stop with the
/// elapsed time and the level bars while recording, and is disabled with a
/// reason while a required permission is denied. Drives the one recorder
/// through `AppController.startRecordingFromWindow`, so the live row is
/// selected; the menu bar item drives the same recorder and never moves the
/// selection.
struct RecordingControl: View {
  let controller: AppController
  @Environment(\.openWindow) private var openWindow

  private static let disabledOpacity: Double = 0.5

  private var recorder: RecordingController { controller.recorder }

  private var presentation: RecordingControlPresentation {
    RecordingControlPresentation.make(
      state: recorder.recording, denied: recorder.deniedPermissions)
  }

  var body: some View {
    VStack(spacing: 0) {
      VStack(alignment: .leading, spacing: Theme.Space.sm) {
        control
          .frame(maxWidth: .infinity)
        if let levels = recorder.levels, case .recording = recorder.recording {
          LevelBars(levels: levels)
        }
        if let reason = presentation.disabledReason {
          MessageRow(kind: .warning, text: reason)
          Button("Fix permissions…") { openWindow(id: "onboarding") }
            .buttonStyle(StenoSecondaryButtonStyle())
            .accessibilityIdentifier("sidebar-fix-permissions")
        }
        RecordingMessages(recorder: recorder)
      }
      .padding(Theme.Space.md)
      .frame(maxWidth: .infinity)
      Divider().overlay(Color.stenoBorder)
    }
    .task { await recorder.refreshPermissions() }
    .onReceive(
      NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)
    ) { _ in
      Task { await recorder.refreshPermissions() }
    }
  }

  @ViewBuilder
  private var control: some View {
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
        .opacity(presentation.isEnabled ? 1 : Self.disabledOpacity)
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
          .accessibilityIdentifier("sidebar-record-in-person")
        }
      }
      .fixedSize(horizontal: false, vertical: true)
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
      Button {
        Task { await recorder.stop() }
      } label: {
        StopLabel(since: since)
          .frame(maxWidth: .infinity)
      }
      .buttonStyle(StenoSecondaryButtonStyle())
      .help("Stop recording (⌘⇧R)")
      .accessibilityIdentifier("sidebar-stop")
    case .stopping:
      Button(action: {}) {
        ProgressView()
          .controlSize(.small)
          .frame(maxWidth: .infinity)
      }
      .buttonStyle(StenoSecondaryButtonStyle())
      .disabled(true)
      .accessibilityLabel(presentation.label)
    }
  }

  private func start(_ mode: CaptureMode) {
    Task { await controller.startRecordingFromWindow(mode: mode) }
  }
}
