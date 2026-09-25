import StenoAudio
import SwiftUI

/// The menu bar item's window: record controls, the processing queue,
/// recent meetings, launch at login, and the app-level shortcuts.
struct MenuBarView: View {
  let controller: AppController
  @Environment(\.openWindow) private var openWindow
  @Environment(\.openSettings) private var openSettings

  private var model: MenuBarViewModel { controller.menuBar }
  private var recorder: RecordingController { controller.recorder }

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.md) {
      recordingSection
      if !model.queue.isEmpty {
        Divider().overlay(Color.stenoBorder)
        queueSection
      }
      if !model.recent.isEmpty {
        Divider().overlay(Color.stenoBorder)
        recentSection
      }
      if let warning = recorder.lastWarning {
        MessageRow(kind: .warning, text: warning)
      }
      if let warning = controller.environment.startupWarnings.first {
        MessageRow(kind: .warning, text: warning)
      }
      if let error = recorder.lastError ?? model.lastError {
        MessageRow(kind: .error, text: error)
      }
      Divider().overlay(Color.stenoBorder)
      footer
    }
    .padding(Theme.Space.md)
    .frame(width: 320)
    .background(Color.stenoPopover)
    .onAppear { model.refreshLoginItem() }
  }

  private var recordingSection: some View {
    VStack(alignment: .leading, spacing: Theme.Space.sm) {
      HStack(spacing: Theme.Space.sm) {
        Circle()
          .fill(recorder.isRecording ? Color.stenoDestructive : Color.stenoGhost)
          .frame(width: 8, height: 8)
        Text(recorder.statusText)
          .font(.steno(Theme.TextSize.sm, weight: .semibold))
          .foregroundStyle(Color.stenoStrong)
        Spacer()
        if case .recording(let since) = recorder.recording {
          TimelineView(.periodic(from: since, by: 1)) { context in
            Text(context.date.timeIntervalSince(since).clockText)
              .font(.steno(Theme.TextSize.sm).monospacedDigit())
              .foregroundStyle(Color.stenoMutedForeground)
          }
        }
      }
      if let levels = recorder.levels, recorder.isRecording {
        LevelBars(levels: levels)
      }
      HStack(spacing: Theme.Space.sm) {
        switch recorder.recording {
        case .idle:
          Button("Record call") { Task { await recorder.start(mode: .call) } }
            .buttonStyle(StenoPrimaryButtonStyle())
            .accessibilityIdentifier("record-call")
          Button("Record in person") { Task { await recorder.start(mode: .inPerson) } }
            .buttonStyle(StenoSecondaryButtonStyle())
            .accessibilityIdentifier("record-in-person")
        case .recording:
          Button("Stop") { Task { await recorder.stop() } }
            .buttonStyle(StenoPrimaryButtonStyle())
            .accessibilityIdentifier("stop-recording")
        case .starting, .stopping:
          ProgressView().controlSize(.small)
        }
      }
    }
  }

  private var queueSection: some View {
    VStack(alignment: .leading, spacing: Theme.Space.sm) {
      SectionLabel(text: "Processing")
      ForEach(model.queue) { item in
        Button {
          open(meeting: item.meeting.id)
        } label: {
          VStack(alignment: .leading, spacing: Theme.Space.xs) {
            HStack {
              Text(item.meeting.title)
                .font(.steno(Theme.TextSize.xs, weight: .medium))
                .foregroundStyle(Color.stenoForeground)
                .lineLimit(1)
              Spacer()
              Text(item.stage?.label ?? "Queued")
                .font(.steno(Theme.TextSize.xxs))
                .foregroundStyle(Color.stenoFaint)
            }
            ProgressView(value: item.fraction)
              .progressViewStyle(.linear)
              .tint(Color.stenoStrong)
          }
        }
        .buttonStyle(.plain)
      }
    }
  }

  private var recentSection: some View {
    VStack(alignment: .leading, spacing: Theme.Space.xs) {
      SectionLabel(text: "Recent")
      ForEach(model.recent) { meeting in
        Button {
          open(meeting: meeting.id)
        } label: {
          HStack {
            Text(meeting.title)
              .font(.steno(Theme.TextSize.xs))
              .foregroundStyle(Color.stenoForeground)
              .lineLimit(1)
            Spacer()
            StatusChip(meeting.state)
          }
          .padding(.vertical, 2)
        }
        .buttonStyle(.plain)
      }
    }
  }

  private var footer: some View {
    let launchAtLogin: Binding<Bool> = .action(
      { model.launchAtLogin.isOn }, model.setLaunchAtLogin)
    return VStack(alignment: .leading, spacing: Theme.Space.sm) {
      HStack {
        Toggle("Launch at login", isOn: launchAtLogin)
          .toggleStyle(.switch)
          .controlSize(.mini)
          .font(.steno(Theme.TextSize.xs))
          .foregroundStyle(Color.stenoForeground)
        if model.launchAtLogin == .requiresApproval {
          Button("Approve…") { model.openLoginItemSettings() }
            .buttonStyle(.plain)
            .font(.steno(Theme.TextSize.xxs))
            .foregroundStyle(Color.stenoWarning)
        }
      }
      HStack(spacing: Theme.Space.md) {
        Button("Open Steno") {
          openWindow(id: "main")
          NSApp.activate()
        }
        Button("Settings…") {
          openSettings()
          NSApp.activate()
        }
        Spacer()
        Button("Quit") { NSApp.terminate(nil) }
      }
      .buttonStyle(.plain)
      .font(.steno(Theme.TextSize.xs))
      .foregroundStyle(Color.stenoMutedForeground)
    }
  }

  private func open(meeting id: UUID) {
    controller.requestedMeetingID = id
    openWindow(id: "main")
    NSApp.activate()
  }
}

/// Two thin level bars, mic and system, from the 10 Hz `LaneLevels`.
struct LevelBars: View {
  let levels: LaneLevels

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.xs) {
      bar("Mic", levels.mic)
      if let system = levels.system {
        bar("System", system)
      }
    }
  }

  private func bar(_ label: String, _ level: LaneLevel) -> some View {
    HStack(spacing: Theme.Space.sm) {
      Text(label)
        .font(.steno(Theme.TextSize.xxxs))
        .foregroundStyle(Color.stenoFaint)
        .frame(width: 44, alignment: .leading)
      GeometryReader { proxy in
        ZStack(alignment: .leading) {
          Capsule().fill(Color.stenoSecondary)
          Capsule()
            .fill(Color.stenoLiveBright)
            .frame(width: proxy.size.width * Self.fraction(level.rms))
            .animation(Motion.functional, value: level.rms)
        }
      }
      .frame(height: 4)
    }
    .accessibilityElement(children: .ignore)
    .accessibilityLabel("\(label) level")
    .accessibilityValue("\(Int(Self.fraction(level.rms) * 100)) percent")
  }

  /// dBFS from -60 to 0 mapped onto 0...1.
  static func fraction(_ dbfs: Float) -> CGFloat {
    CGFloat(min(1, max(0, (dbfs + 60) / 60)))
  }
}
