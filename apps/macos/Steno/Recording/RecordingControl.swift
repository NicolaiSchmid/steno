import StenoAudio
import SwiftUI

/// The primary control at the top of the nav column: starts a call recording
/// (in person one click away in the chevron segment), turns into Stop with
/// the elapsed time and the level meter while recording, and is disabled
/// with a reason while a required permission is denied. Drives the one
/// recorder through `AppController.startRecordingFromWindow`, so the live
/// row is selected; the menu bar item drives the same recorder and never
/// moves the selection. The permission report refreshes when the control
/// appears; `AppController` refreshes it again whenever the app becomes
/// active. Presentation, ids and actions are the start-recording plan's;
/// the box (40 pt, radius 12, the glyph well, the `raised` Stop) is the
/// redesign's.
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
    VStack(alignment: .leading, spacing: Theme.Space.sm) {
      control(presentation)
        .frame(maxWidth: .infinity)
      if let autoStop = presentation.autoStop {
        AutoStopRow(presentation: autoStop, identifier: "sidebar-keep-recording") {
          recorder.keepRecording()
        }
      }
      if let reason = presentation.disabledReason {
        MessageRow(kind: .warning, text: reason)
        Button("Fix permissions…") { openWindow(id: "onboarding") }
          .buttonStyle(StenoSecondaryButtonStyle())
          .accessibilityIdentifier("sidebar-fix-permissions")
      }
      RecordingMessages(warning: recorder.lastWarning, error: recorder.lastError)
    }
    .frame(maxWidth: .infinity)
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
          HStack(spacing: Theme.Space.sm) {
            Image(systemName: "record.circle")
              .font(.system(size: Theme.TextSize.base.size, weight: .medium))
              .frame(width: Theme.Control.ctaWellSize, height: Theme.Control.ctaWellSize)
              .background(Theme.Radius.md.shape.fill(Color.stenoOnAccent.opacity(0.12)))
              .accessibilityHidden(true)
            Text(presentation.label)
            Spacer(minLength: 0)
          }
          .padding(.leading, Theme.Space.sm)
          .padding(.trailing, Theme.Control.buttonInset)
        }
        .buttonStyle(CTASegmentStyle())
        .disabled(!presentation.isEnabled)
        .help("Record a call (⌘⇧R)")
        .accessibilityIdentifier("sidebar-record")
        if presentation.offersInPerson {
          Rectangle()
            .fill(Color.stenoOnAccent.opacity(0.2))
            .frame(width: Theme.Space.hairline)
          Menu {
            Button("Record in person") { start(.inPerson) }
          } label: {
            Image(systemName: "chevron.down")
              .font(.steno(Theme.TextSize.xxs, weight: .semibold))
              .frame(width: Theme.Control.ctaMenuWidth)
          }
          .menuStyle(.button)
          .buttonStyle(CTASegmentStyle())
          .menuIndicator(.hidden)
          .fixedSize(horizontal: true, vertical: false)
          .help("Record in person")
          .accessibilityLabel("Record in person")
          .accessibilityIdentifier("sidebar-record-in-person")
        }
      }
      .modifier(CTABox())
      .opacity(presentation.isEnabled ? 1 : Motion.disabledOpacity)
    // The busy states keep the box, and the button, while a spinner stands
    // in for the label, so VoiceOver reads one disabled control through the
    // transition.
    case .starting:
      Button(action: {}) {
        ProgressView()
          .controlSize(.small)
          .tint(Color.stenoOnAccent)
          .frame(maxWidth: .infinity)
      }
      .buttonStyle(CTASegmentStyle())
      .modifier(CTABox())
      .disabled(true)
      .accessibilityLabel(presentation.label)
    case .recording(let since):
      StopButton(
        state: .stop(since: since), surface: .sidebar(levels: recorder.levels), id: "sidebar-stop"
      ) {
        Task { await recorder.stop() }
      }
    case .stopping:
      StopButton(state: .stopping, surface: .sidebar(levels: nil), id: "sidebar-stop") {}
    }
  }

  private func start(_ mode: CaptureMode) {
    Task { await controller.startRecordingFromWindow(mode: mode) }
  }
}

/// The CTA's box: 40 pt tall, radius 12, the accent gradient, clipped so the
/// two segments share one shape.
private struct CTABox: ViewModifier {
  func body(content: Content) -> some View {
    content
      .frame(height: Theme.Control.ctaHeight)
      .background(Theme.Radius.lg.shape.fill(LinearGradient.stenoAccent))
      .clipShape(Theme.Radius.lg.shape)
  }
}

/// One segment of the CTA: 14 pt medium `on-accent` on a transparent box
/// the container fills and clips; pressed dims 5 % over `Motion.functional`.
/// The disabled dim is the container's, so both segments fade together.
private struct CTASegmentStyle: ButtonStyle {
  func makeBody(configuration: Configuration) -> some View {
    Segment(configuration: configuration)
  }

  private struct Segment: View {
    let configuration: Configuration
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
      configuration.label
        .font(.steno(Theme.TextSize.sm, weight: .medium))
        .foregroundStyle(Color.stenoOnAccent)
        .frame(maxHeight: .infinity)
        .contentShape(Rectangle())
        .opacity(configuration.isPressed ? Motion.controlPressOpacity : 1)
        .animation(Motion.swap(reduceMotion: reduceMotion), value: configuration.isPressed)
    }
  }
}
