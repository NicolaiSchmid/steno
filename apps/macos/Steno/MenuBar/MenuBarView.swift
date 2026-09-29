import StenoAudio
import StenoCore
import SwiftUI

/// The menu bar item's window: record controls, the processing queue,
/// recent meetings, launch at login, and the app-level shortcuts.
struct MenuBarView: View {
  let controller: AppController
  @Environment(\.openWindow) private var openWindow
  @Environment(\.openSettings) private var openSettings
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  private var model: MenuBarViewModel { controller.menuBar }
  private var recorder: RecordingController { controller.recorder }

  /// The same state table the sidebar control and the Record menu render.
  private var presentation: RecordingControlPresentation {
    RecordingControlPresentation.make(
      state: recorder.recording, denied: recorder.deniedPermissions,
      autoStop: recorder.autoStop?.presentation)
  }

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
      RecordingMessages(warning: recorder.lastWarning, error: recorder.lastError ?? model.lastError)
      if let warning = controller.environment.startupWarnings.first {
        MessageRow(kind: .warning, text: warning)
      }
      Divider().overlay(Color.stenoBorder)
      footer
    }
    .padding(Theme.Space.lg)
    .frame(width: 320)
    .background(Color.stenoPopover)
    .onAppear { model.refreshLoginItem() }
  }

  private var recordingSection: some View {
    let presentation = self.presentation
    return VStack(alignment: .leading, spacing: Theme.Space.sm) {
      HStack(spacing: Theme.Space.sm) {
        StatusDot(color: recorder.isRecording ? Color.stenoDestructive : Color.stenoGhost)
        Text(recorder.statusText)
          .font(.steno(Theme.TextSize.sm, weight: .semibold))
          .foregroundStyle(Color.stenoStrong)
        Spacer()
        if case .recording(let since) = recorder.recording {
          ElapsedText(since: since)
            .font(.steno(Theme.TextSize.sm))
            .foregroundStyle(Color.stenoMutedForeground)
        }
      }
      if let levels = recorder.levels, case .recording = recorder.recording {
        LevelBars(levels: levels)
      }
      HStack(spacing: Theme.Space.sm) {
        switch recorder.recording {
        case .idle:
          Button {
            Task { await recorder.start(mode: .call) }
          } label: {
            HStack(spacing: Theme.Space.sm) {
              Image(systemName: "record.circle")
                .font(.system(size: Theme.TextSize.sm.size, weight: .medium))
                .accessibilityHidden(true)
              Text(presentation.label)
            }
          }
          .buttonStyle(StenoPrimaryButtonStyle())
          .disabled(!presentation.isEnabled)
          .accessibilityIdentifier("record-call")
          Button("Record in person") { Task { await recorder.start(mode: .inPerson) } }
            .buttonStyle(StenoSecondaryButtonStyle())
            .disabled(!presentation.offersInPerson)
            .accessibilityIdentifier("record-in-person")
        case .recording(let since):
          // The same destructive treatment as the sidebar control: `raised`
          // surface with a hairline, the `destructive` dot and label.
          Button {
            Task { await recorder.stop() }
          } label: {
            StopLabel(since: since)
          }
          .buttonStyle(StenoSecondaryButtonStyle())
          .accessibilityIdentifier("stop-recording")
        case .starting, .stopping:
          ProgressView().controlSize(.small)
        }
      }
      if let autoStop = presentation.autoStop {
        AutoStopRow(presentation: autoStop, identifier: "keep-recording") {
          recorder.keepRecording()
        }
      }
      if let reason = presentation.disabledReason {
        MessageRow(kind: .warning, text: reason)
      }
    }
  }

  private var queueSection: some View {
    PopoverSection(title: "Processing") {
      ForEach(model.queue) { item in
        PopoverRow {
          open(meeting: item.meeting.id)
        } content: {
          queueRow(item)
        }
      }
    }
  }

  /// Title, the progress model's title and remaining text, and the bar.
  /// The remaining text and the bar's value are sampled from
  /// `ProcessingPresentation` once a second from the entry's `since`, as
  /// the card samples them, so the row and the card never disagree. Before
  /// the run's first event (or before the model has seen the meeting) the
  /// row says "Waiting to process" over an empty bar.
  private func queueRow(_ item: MenuBarViewModel.QueueItem) -> some View {
    let entry = controller.progress.entry(for: item.id)
    return TimelineView(
      .periodic(from: entry?.since ?? controller.environment.now(), by: Motion.durationCountdown)
    ) { context in
      let state: ProcessingPresentation.State? = entry?.progress.map { progress in
        ProcessingPresentation.state(
          progress: progress,
          elapsed: .seconds(max(0, context.date.timeIntervalSince(entry?.since ?? context.date))),
          reduceMotion: reduceMotion)
      }
      VStack(alignment: .leading, spacing: Theme.Space.xs) {
        HStack(spacing: Theme.Space.sm) {
          Text(item.meeting.title)
            .font(.steno(Theme.TextSize.xs, weight: .medium))
            .foregroundStyle(Color.stenoForeground)
            .lineLimit(1)
          Spacer()
          Text(entry?.title ?? ProcessingProgressModel.Entry.waitingTitle)
            .font(.steno(Theme.TextSize.xxs))
            .foregroundStyle(Color.stenoFaint)
          if let state {
            Text(state.remainingText)
              .font(.steno(Theme.TextSize.xxs).monospacedDigit())
              .foregroundStyle(Color.stenoFaint)
          }
        }
        ProgressView(value: state?.fraction ?? 0)
          .progressViewStyle(.linear)
          .tint(Color.stenoStrong)
          .animation(
            ProcessingPresentation.tween(reduceMotion: reduceMotion), value: state?.fraction
          )
          .accessibilityValue("\(Int(((state?.fraction ?? 0) * 100).rounded())) percent")
      }
    }
  }

  private var recentSection: some View {
    PopoverSection(title: "Recent") {
      ForEach(model.recent) { meeting in
        PopoverRow {
          open(meeting: meeting.id)
        } content: {
          RecentLine(title: meeting.title, state: meeting.state)
        }
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
      .buttonStyle(StenoGhostButtonStyle(tint: Color.stenoMutedForeground))
    }
  }

  private func open(meeting id: UUID) {
    controller.requestedMeetingID = id
    openWindow(id: "main")
    NSApp.activate()
  }
}

/// A section of the popover: the label at the content edge over the rows
/// at 2 pt. The rows bleed their horizontal padding into the gutter so the
/// hover veil runs past the text while every text edge stays at 16 pt.
private struct PopoverSection<Rows: View>: View {
  let title: String
  @ViewBuilder var rows: () -> Rows

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.xs) {
      SectionLabel(text: title)
      VStack(spacing: Theme.Space.xxs, content: rows)
        .padding(.horizontal, -Theme.Space.sm)
    }
  }
}

/// A queue or recent row in the popover: padding 6 x 8, radius 6, the
/// `card` veil while hovered, the whole row hittable. The row owns no
/// content of its own; the caller passes the action and the line.
private struct PopoverRow<Content: View>: View {
  let action: () -> Void
  @ViewBuilder var content: () -> Content
  @State private var hovering = false
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  var body: some View {
    Button(action: action) {
      content()
        .modifier(PopoverRowBox(fill: hovering ? Color.stenoCard : Color.clear))
    }
    .buttonStyle(.plain)
    .onHover { hovering = $0 }
    .animation(Motion.swap(reduceMotion: reduceMotion), value: hovering)
  }
}

/// The row's box, separate from the hover state so the preview can render
/// the hovered veil statically.
private struct PopoverRowBox: ViewModifier {
  let fill: Color

  func body(content: Content) -> some View {
    content
      .padding(.vertical, Theme.Control.menuRowInset)
      .padding(.horizontal, Theme.Space.sm)
      .frame(maxWidth: .infinity, alignment: .leading)
      .background(Theme.Radius.sm.shape.fill(fill))
      .contentShape(Theme.Radius.sm.shape)
  }
}

/// A recent meeting's line: the title, one line, and its state chip.
private struct RecentLine: View {
  let title: String
  let state: MeetingState

  var body: some View {
    HStack(spacing: Theme.Space.sm) {
      Text(title)
        .font(.steno(Theme.TextSize.xs))
        .foregroundStyle(Color.stenoForeground)
        .lineLimit(1)
      Spacer()
      StatusChip(state)
    }
  }
}

#if DEBUG
  /// The recent rows without a controller: one at rest, one rendered with
  /// the hover veil, since a preview cannot hold the pointer.
  private struct PopoverRowsPreview: View {
    var body: some View {
      PopoverSection(title: "Recent") {
        PopoverRow {
        } content: {
          RecentLine(title: "Produktstrategie 90/10", state: .ready)
        }
        RecentLine(title: "Weekly sync", state: .failed(reason: "Timed out"))
          .modifier(PopoverRowBox(fill: Color.stenoCard))
      }
      .padding(Theme.Space.lg)
      .frame(width: 320)
      .background(Color.stenoPopover)
    }
  }

  #Preview("Popover rows") {
    PreviewPair { PopoverRowsPreview() }
      .frame(width: 800, height: 200)
  }
#endif
