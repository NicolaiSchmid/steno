import StenoAudio
import SwiftUI

/// The menu bar item's window: record controls, the processing queue,
/// recent meetings, launch at login, and the app-level shortcuts.
struct MenuBarView: View {
  let controller: AppController
  @Environment(\.openWindow) private var openWindow
  @Environment(\.openSettings) private var openSettings

  private var model: MenuBarViewModel { controller.menuBar }

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
      if let warning = model.lastWarning {
        MessageRow(kind: .warning, text: warning)
      }
      if let error = model.lastError {
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
          .fill(model.isRecording ? Color.stenoDestructive : Color.stenoGhost)
          .frame(width: 8, height: 8)
        Text(statusText)
          .font(.steno(Theme.TextSize.sm, weight: .semibold))
          .foregroundStyle(Color.stenoStrong)
        Spacer()
        if case .recording(let since) = model.recording {
          TimelineView(.periodic(from: since, by: 1)) { context in
            Text(context.date.timeIntervalSince(since).clockText)
              .font(.steno(Theme.TextSize.sm).monospacedDigit())
              .foregroundStyle(Color.stenoMutedForeground)
          }
        }
      }
      if let levels = model.levels, model.isRecording {
        LevelBars(levels: levels)
      }
      HStack(spacing: Theme.Space.sm) {
        switch model.recording {
        case .idle:
          Button("Record call") { Task { await model.start(mode: .call) } }
            .buttonStyle(StenoPrimaryButtonStyle())
            .accessibilityIdentifier("record-call")
          Button("Record in person") { Task { await model.start(mode: .inPerson) } }
            .buttonStyle(StenoSecondaryButtonStyle())
            .accessibilityIdentifier("record-in-person")
        case .recording:
          Button("Stop") { Task { await model.stop() } }
            .buttonStyle(StenoPrimaryButtonStyle())
            .accessibilityIdentifier("stop-recording")
        case .starting, .stopping:
          ProgressView().controlSize(.small)
        }
      }
    }
  }

  private var statusText: String {
    switch model.recording {
    case .idle: "Not recording"
    case .starting: "Starting…"
    case .recording: "Recording"
    case .stopping: "Finishing…"
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
              Text(item.stage.map { $0.rawValue } ?? "queued")
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
            MeetingStateLabel.chip(for: MeetingStateLabel(meeting.state))
          }
          .padding(.vertical, 2)
        }
        .buttonStyle(.plain)
      }
    }
  }

  private var footer: some View {
    VStack(alignment: .leading, spacing: Theme.Space.sm) {
      HStack {
        Toggle(
          "Launch at login",
          isOn: Binding(
            get: { model.launchAtLogin == .enabled || model.launchAtLogin == .requiresApproval },
            set: { enabled in Task { await model.setLaunchAtLogin(enabled) } }))
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
            .frame(width: proxy.size.width * fraction(level.rms))
            .animation(Motion.functional, value: level.rms)
        }
      }
      .frame(height: 4)
    }
  }

  /// dBFS from -60 to 0 mapped onto 0...1.
  private func fraction(_ dbfs: Float) -> CGFloat {
    CGFloat(min(1, max(0, (dbfs + 60) / 60)))
  }
}
