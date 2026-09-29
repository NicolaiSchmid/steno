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
        .padding(.bottom, Theme.Space.titleGap)
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
      // While recording only: the recorder keeps the last reading through
      // `.stopping`, and a frozen bar under a spinner reads as live.
      if case .stop = stop, let levels = recorder.levels {
        LevelBars(levels: levels)
          .frame(width: Self.levelBarsWidth)
          .accessibilityElement(children: .contain)
          .accessibilityIdentifier("header-levels")
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
  /// has an entry). Ready says nothing: the content is the signal. A
  /// `.recording` row the recorder does not hold yet (the intake writes it
  /// while `.starting`) says nothing either: decision 3 keeps green for
  /// success, and the Stop takes over the moment the recorder holds the row.
  private func titleRow(_ meeting: Meeting, stop: HeaderStop?) -> some View {
    HStack(alignment: .center, spacing: Theme.Space.md) {
      Text(meeting.displayTitle())
        .font(.steno(Theme.TextSize.xl, weight: .semibold))
        .tracking(-0.2)
        .foregroundStyle(Color.stenoStrong)
        .textSelection(.enabled)
        .lineLimit(2)
        .fixedSize(horizontal: false, vertical: true)
        .accessibilityIdentifier("meeting-title")
      if meeting.isTitleDerived {
        // Decision 13: a derived title ("Monday 10:06") carries the source
        // as a neutral chip beside it, not as a word in it.
        StatusChip(text: meeting.source.label, style: .neutral)
      }
      Spacer(minLength: Theme.Space.sm)
      if let stop {
        StopButton(state: stop, surface: .header, id: "header-stop") {
          Task { await recorder.stop() }
        }
      } else if let entry = controller.progress.entry(for: meeting.id) {
        StatusChip(text: entry.title, color: Color.stenoInfo)
      } else {
        switch meeting.state {
        case .queued, .processing, .failed: StatusChip(meeting.state)
        case .recording, .ready: EmptyView()
        }
      }
    }
  }

  /// One 12 pt `muted` line, `metaFacts(for:)` joined by " · ".
  private func metaRow(_ meeting: Meeting) -> some View {
    Text(Self.metaFacts(for: meeting).joined(separator: " · "))
      .font(.steno(Theme.TextSize.xxs))
      .monospacedDigit()
      .foregroundStyle(Color.stenoMutedForeground)
      .textSelection(.enabled)
      .accessibilityIdentifier("meeting-meta")
  }

  /// The meta line's facts, in order: date and time and the source (unless
  /// the derived title and its chip already say them), duration once known,
  /// language. The token count stays in the data and off the screen: it is
  /// developer vocabulary, not a fact about the meeting. `DetailStatesTests`
  /// pins the list.
  nonisolated static func metaFacts(
    for meeting: Meeting, locale: Locale = .current, timeZone: TimeZone = .current
  ) -> [String] {
    var facts: [String] = []
    if !meeting.isTitleDerived {
      let when = Date.FormatStyle(locale: locale, timeZone: timeZone)
        .year().month().day().hour().minute()
      facts.append(meeting.startedAt.formatted(when))
      facts.append(meeting.source.label)
    }
    if meeting.duration > 0 { facts.append(meeting.duration.clockText) }
    if let language = meeting.language { facts.append(language.localizedName(in: locale)) }
    return facts
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
    // `.button` with a plain button style keeps the 28 pt face; the
    // borderless menu style draws only the bare glyph.
    .menuStyle(.button)
    .buttonStyle(.plain)
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

  /// The status line wraps to a second line at the pane minimum rather than
  /// truncating mid-word beside its button.
  private func footerText(_ text: String) -> some View {
    Text(text)
      .font(.steno(Theme.TextSize.xxs))
      .foregroundStyle(Color.stenoMutedForeground)
      .lineLimit(2)
      .fixedSize(horizontal: false, vertical: true)
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

/// "Add tag": the chip glyph (`chipGlyphSize`, one under the 12 pt text)
/// before 12 pt text, both in the ghost button's colour.
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

  /// The status word after the name: "Pending", "Exported 10:02", "Failed".
  private var status: String {
    switch delivery.status {
    case .pending: "Pending"
    case .delivered: exportedText
    case .failed: "Failed"
    }
  }

  private var chip: StatusChip {
    let text = "\(name) · \(status)"
    return switch delivery.status {
    case .pending: StatusChip(text: text, color: Color.stenoInfo, systemImage: "folder")
    case .delivered: StatusChip(text: text, style: .neutral, systemImage: "folder")
    case .failed: StatusChip(text: text, color: Color.stenoDestructive, systemImage: "folder")
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
      // The status stays the label, so VoiceOver hears "Obsidian, Exported
      // 10:02"; the click is the hint.
      .accessibilityLabel("\(name), \(status)")
      .accessibilityHint("Reveals the export in Finder")
    } else {
      chip.help(help)
    }
  }
}
