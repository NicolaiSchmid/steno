import AppKit
import StenoCore
import SwiftUI

/// The detail pane: the header stack of the redesign plan's decision 17
/// (title row, meta row, messages, end reason, retention line, level bars,
/// speakers, tags and actions), the segmented tabs, the tab content and the
/// export footer. Speakers are named from the header row's popover; closing
/// it re-exports when something changed. While the recorder holds this
/// meeting the title row carries the Stop control (id `header-stop`) that
/// calls the shared `RecordingController.stop()`, the same call the sidebar
/// control, the menu bar item and the bubble make, and the level bars sit
/// under the meta line. While the meeting is queued or processing the
/// header chip reads the progress model's title and every tab shows the
/// `ProcessingCard`; the chip is the header's only processing signal, the
/// card has the one bar.
struct MeetingDetailView: View {
  @Bindable var model: MeetingDetailViewModel
  let controller: AppController
  @State private var tagsText = ""
  @State private var editingTags = false
  @State private var showsSpeakers = false
  @State private var hoveringActions = false
  @Environment(\.openSettings) private var openSettings
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  /// The level bars' width in the header; the plan's 240 pt.
  private static let levelBarsWidth: CGFloat = 240
  /// The tags field's width while editing; the plan's 240 pt.
  private static let tagsFieldWidth: CGFloat = 240

  var body: some View {
    VStack(alignment: .leading, spacing: 0) {
      if let meeting = model.meeting {
        header(meeting)
        SegmentedTabs(MeetingDetailViewModel.Tab.allCases, selection: $model.tab) { $0.title }
          .padding(.horizontal, Theme.Space.xxl)
          .padding(.bottom, Theme.Space.lg)
        Divider().overlay(Color.stenoBorder)
        content
        Divider().overlay(Color.stenoBorder)
        footer(meeting)
      } else {
        ProgressView().controlSize(.small).frame(maxWidth: .infinity, maxHeight: .infinity)
      }
    }
    .background(Color.stenoBackground)
    .task { await model.observe() }
    .task { await model.observeDeliveries() }
    .task { await model.observeSettings() }
    .confirmationDialog(
      "Delete this recording now?", isPresented: $model.confirmsDeleteNow, titleVisibility: .visible
    ) {
      Button("Delete recording", role: .destructive) { Task { await model.setKeepAudio(false) } }
      Button("Cancel", role: .cancel) {}
    } message: {
      Text("The recording is deleted shortly. The transcript, summary and exports stay.")
    }
    .onChange(of: showsSpeakers) { _, shown in
      if !shown { Task { await model.pickerClosed() } }
    }
    .onDisappear { model.viewDisappeared() }
  }

  private var recorder: RecordingController { controller.recorder }

  private var showsSpeakersRow: Bool {
    guard let export = model.export else { return false }
    return export.meeting.state == .ready && !export.speakers.isEmpty
  }

  /// The Stop control's state for this meeting; nil unless the recorder
  /// holds it.
  private func headerStop(_ meeting: Meeting) -> HeaderStop? {
    HeaderStop.make(
      meetingID: meeting.id, recording: recorder.recording,
      activeMeetingID: recorder.activeMeetingID)
  }

  // MARK: - Header

  /// Decision 17's stack at 32 pt sides and 24 pt top. Gaps are the plan's:
  /// 6 under the title, 12 under the meta line, 8 under each message row,
  /// 12 under the level bars, 16 under the tags row.
  private func header(_ meeting: Meeting) -> some View {
    let stop = headerStop(meeting)
    return VStack(alignment: .leading, spacing: 0) {
      titleRow(meeting, stop: stop)
        .padding(.bottom, Theme.Space.sm - Theme.Space.xxs)
      metaRow(meeting)
        .padding(.bottom, Theme.Space.md)
      if case .failed(let reason) = meeting.state {
        MessageRow(kind: .error, text: reason)
          .padding(.bottom, Theme.Space.sm)
      }
      if let error = model.error {
        MessageRow(kind: .error, text: error)
          .padding(.bottom, Theme.Space.sm)
      }
      if let sentence = meeting.endReason?.sentence {
        MessageRow(kind: .info, text: sentence)
          .accessibilityIdentifier("end-reason")
          .padding(.bottom, Theme.Space.sm)
      }
      if model.recordingStatusText != nil || model.showsKeepToggle {
        recordingLine
          .padding(.bottom, Theme.Space.sm)
      }
      if stop != nil, let levels = recorder.levels {
        LevelBars(levels: levels)
          .frame(width: Self.levelBarsWidth)
          .padding(.bottom, Theme.Space.md)
      }
      if showsSpeakersRow {
        SpeakersRow(model: model.speakers, isPresented: $showsSpeakers)
          .popover(isPresented: $showsSpeakers, arrowEdge: .bottom) {
            SpeakersPopover(model: model.speakers)
          }
          .padding(.bottom, Theme.Space.md)
      }
      tagsRow(meeting)
    }
    .padding(.horizontal, Theme.Space.xxl)
    .padding(.top, Theme.Space.xl)
    .padding(.bottom, Theme.Space.lg)
    .animation(Motion.swap(reduceMotion: reduceMotion), value: stop)
  }

  /// The title at 21 semibold with the ladder's tracking; trailing, the Stop
  /// control while the recorder holds the meeting, else a status chip only
  /// for queued, processing and failed (the progress model's title while it
  /// has an entry). Ready says nothing: the content is the signal.
  private func titleRow(_ meeting: Meeting, stop: HeaderStop?) -> some View {
    HStack(alignment: .center, spacing: Theme.Space.md) {
      Text(meeting.title)
        .font(.steno(Theme.TextSize.xl, weight: .semibold))
        .tracking(-0.2)
        .foregroundStyle(Color.stenoStrong)
        .textSelection(.enabled)
        .lineLimit(2)
        .fixedSize(horizontal: false, vertical: true)
      Spacer(minLength: Theme.Space.sm)
      if let stop {
        stopControl(stop)
      } else if let entry = controller.progress.entry(for: meeting.id) {
        StatusChip(text: entry.title, color: Color.stenoInfo)
      } else {
        switch meeting.state {
        case .recording, .queued, .processing, .failed: StatusChip(meeting.state)
        case .ready: EmptyView()
        }
      }
    }
  }

  /// The plan's Stop: a secondary button carrying the start plan's
  /// `StopLabel` (static dot, "Stop", the ticking elapsed time); disabled
  /// with a spinner while the stop is finishing.
  @ViewBuilder
  private func stopControl(_ stop: HeaderStop) -> some View {
    switch stop {
    case .stop(let since):
      Button {
        Task { await recorder.stop() }
      } label: {
        StopLabel(since: since)
      }
      .buttonStyle(StenoSecondaryButtonStyle())
      .help("Stop recording (⌘⇧R)")
      .accessibilityIdentifier("header-stop")
    case .stopping:
      Button(action: {}) {
        ProgressView().controlSize(.small)
      }
      .buttonStyle(StenoSecondaryButtonStyle())
      .disabled(true)
      .accessibilityLabel(RecordingState.stopping.label)
    }
  }

  /// One 12 pt `muted` line, facts joined by " · ": date and time, source,
  /// duration once known, language, token count.
  private func metaRow(_ meeting: Meeting) -> some View {
    var facts: [String] = [
      meeting.startedAt.formatted(.dateTime.year().month().day().hour().minute()),
      meeting.source.label,
    ]
    if meeting.duration > 0 { facts.append(meeting.duration.clockText) }
    if let language = meeting.language { facts.append(language.localizedName()) }
    if let usage = meeting.llmUsage {
      facts.append("\(usage.promptTokens + usage.completionTokens) tokens")
    }
    return Text(facts.joined(separator: " · "))
      .font(.steno(Theme.TextSize.xxs))
      .monospacedDigit()
      .foregroundStyle(Color.stenoMutedForeground)
      .textSelection(.enabled)
      .accessibilityIdentifier("meeting-meta")
  }

  /// The retention row of the header: what happens to the audio file when
  /// the default rule does not say it all, and the per-meeting keep when
  /// the default is not Forever.
  private var recordingLine: some View {
    HStack(spacing: Theme.Space.md) {
      if let status = model.recordingStatusText {
        Text(status)
          .font(.steno(Theme.TextSize.xxs))
          .foregroundStyle(Color.stenoMutedForeground)
          .accessibilityIdentifier("recording-status")
      }
      if model.showsKeepToggle {
        Toggle("Keep this recording", isOn: .action({ model.keepsAudio }, model.toggleKeepAudio))
          .toggleStyle(.checkbox)
          .font(.steno(Theme.TextSize.xxs))
          .foregroundStyle(Color.stenoMutedForeground)
          .accessibilityIdentifier("keep-recording")
      }
    }
  }

  /// Neutral tag chips, the "Add tag" ghost button (a hairline field while
  /// editing), and the Actions menu as an icon button trailing.
  private func tagsRow(_ meeting: Meeting) -> some View {
    HStack(alignment: .center, spacing: Theme.Space.sm) {
      if editingTags {
        StenoTextField("tags, comma separated", text: $tagsText)
          .frame(width: Self.tagsFieldWidth)
          .accessibilityIdentifier("tags-field")
          .onSubmit {
            editingTags = false
            let text = tagsText
            Task { await model.setTags(text: text) }
          }
          .onExitCommand { editingTags = false }
      } else {
        ForEach(meeting.tags, id: \.self) { tag in
          StatusChip(text: "#\(tag)", style: .neutral)
        }
        Button {
          tagsText = meeting.tags.joined(separator: ", ")
          editingTags = true
        } label: {
          Label(meeting.tags.isEmpty ? "Add tag" : "Edit tags", systemImage: "plus")
            .labelStyle(TagButtonLabelStyle())
        }
        .buttonStyle(StenoGhostButtonStyle())
        .accessibilityIdentifier("edit-tags")
      }
      Spacer(minLength: Theme.Space.sm)
      actionsMenu(meeting)
    }
  }

  /// The existing Actions menu behind an icon button face: template,
  /// re-run, re-export, reveal the recording.
  private func actionsMenu(_ meeting: Meeting) -> some View {
    Menu {
      Picker("Template", selection: .action({ meeting.templateID }, model.setTemplate)) {
        ForEach(model.templates) { template in
          Text(template.displayName).tag(template.id)
        }
      }
      .disabled(!model.canRerunSummary)
      .help(model.llmConfigured ? "" : SetupCopy.rerunHelp)
      Button("Re-run summary") { Task { await model.rerunSummary() } }
        .disabled(!model.canRerunSummary)
        .help(model.llmConfigured ? "" : SetupCopy.rerunHelp)
      Button("Re-export") { Task { await model.reexport() } }
        .disabled(!model.canReexport)
        .help(model.vaultConfigured ? "" : SetupCopy.reexportHelp)
      if let url = model.export?.audio?.url {
        Divider()
        Button("Reveal recording in Finder") {
          NSWorkspace.shared.activateFileViewerSelecting([url])
        }
        .disabled(!model.recordingFilesExist)
      }
    } label: {
      IconButton.Glyph(systemName: "ellipsis", hovering: hoveringActions)
    }
    .menuStyle(.borderlessButton)
    .menuIndicator(.hidden)
    .fixedSize()
    .onHover { hoveringActions = $0 }
    .animation(Motion.swap(reduceMotion: reduceMotion), value: hoveringActions)
    .disabled(model.isBusy)
    .help("Actions")
    .accessibilityLabel("Actions")
    .accessibilityIdentifier("meeting-actions")
  }

  // MARK: - Content

  @ViewBuilder
  private var content: some View {
    let progress = controller.progress.entry(for: model.id)
    Group {
      switch model.tab {
      case .summary: SummaryTab(model: model, controller: controller, progress: progress)
      case .transcript: TranscriptTab(model: model, progress: progress)
      case .tasks: TasksTab(model: model, controller: controller, progress: progress)
      case .scratchpad: ScratchpadTab(model: model, progress: progress)
      }
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity)
    .accessibilityElement(children: .contain)
    .accessibilityIdentifier("tab-content-\(model.tab.rawValue)")
  }

  // MARK: - Footer

  /// Selected by `exportStatus`: no vault, not exported yet, or one chip per
  /// delivery. 32 pt sides, 12 pt vertical. Ids `footer-choose-vault` and
  /// `footer-export-now`. The word is "export" everywhere.
  private func footer(_ meeting: Meeting) -> some View {
    HStack(spacing: Theme.Space.md) {
      switch model.exportStatus {
      case .noVault:
        footerText(SetupCopy.notExportedNoVault)
        footerButton(SetupCopy.chooseVault, id: "footer-choose-vault") {
          controller.openSettings(.export, with: openSettings)
        }
      case .notExported:
        footerText(SetupCopy.notExportedYet)
        footerButton(SetupCopy.exportNow, id: "footer-export-now") {
          Task { await model.reexport() }
        }
        .disabled(model.isBusy || !model.canReexport)
      case .exported(let deliveries):
        ForEach(deliveries) { delivery in
          DeliveryBadge(delivery: delivery)
        }
      }
      Spacer()
      if model.isBusy { ProgressView().controlSize(.small) }
    }
    .padding(.horizontal, Theme.Space.xxl)
    .padding(.vertical, Theme.Space.md)
  }

  private func footerText(_ text: String) -> some View {
    Text(text)
      .font(.steno(Theme.TextSize.xxs))
      .foregroundStyle(Color.stenoMutedForeground)
      .accessibilityIdentifier("footer-export-status")
  }

  /// The footer's one ghost action beside the status text.
  private func footerButton(_ title: String, id: String, action: @escaping () -> Void)
    -> some View
  {
    Button(title, action: action)
      .buttonStyle(StenoGhostButtonStyle())
      .accessibilityIdentifier(id)
  }
}

/// "Add tag" as the plan draws it: a 10 pt `plus` before 12 pt text, both in
/// the ghost button's colour.
private struct TagButtonLabelStyle: LabelStyle {
  func makeBody(configuration: Configuration) -> some View {
    HStack(spacing: Theme.Space.xs) {
      configuration.icon
        .font(.system(size: Theme.Control.chipGlyphSize, weight: .medium))
      configuration.title
        .font(.steno(Theme.TextSize.xxs))
    }
  }
}

/// One delivery in the footer: a chip with the `folder` glyph, the
/// destination's display name and "Exported 10:02"; `info` while pending,
/// `destructive` "Failed" with the message as help. Clicking a chip with a
/// receipt reveals the exported folder in Finder. The raw destination id
/// never reaches the chip.
struct DeliveryBadge: View {
  let delivery: Delivery

  private var name: String { delivery.destinationDisplayName }

  private var chip: StatusChip {
    switch delivery.status {
    case .pending:
      StatusChip(text: "\(name) · Pending", color: Color.stenoInfo, systemImage: "folder")
    case .delivered:
      StatusChip(text: "\(name) · \(exportedText)", style: .neutral, systemImage: "folder")
    case .failed:
      StatusChip(text: "\(name) · Failed", color: Color.stenoDestructive, systemImage: "folder")
    }
  }

  /// "Exported 10:02", the hour following the locale's clock; "Exported"
  /// alone when the attempt time was not recorded.
  private var exportedText: String {
    guard let at = delivery.lastAttemptAt else { return "Exported" }
    return "Exported \(at.formatted(.dateTime.hour(.defaultDigits(amPM: .abbreviated)).minute()))"
  }

  private var help: String {
    if case .failed(let message) = delivery.status { return message }
    return delivery.receipt == nil ? "" : "Reveal in Finder"
  }

  var body: some View {
    if let folder = delivery.receipt?.folderURL {
      Button {
        NSWorkspace.shared.activateFileViewerSelecting([folder])
      } label: {
        chip
      }
      .buttonStyle(.plain)
      .help(help)
      .accessibilityLabel("Reveal the \(name) export in Finder")
    } else {
      chip.help(help)
    }
  }
}
