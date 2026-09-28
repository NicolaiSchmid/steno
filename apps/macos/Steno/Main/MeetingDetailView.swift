import AppKit
import StenoAdapters
import StenoCore
import SwiftUI

/// Header (title, meta, the speakers row, tags, actions), the four tabs and
/// the delivery footer. Speakers are named from the header row's popover;
/// closing it re-exports when something changed. While the meeting is
/// queued or processing the header chip reads the progress model's title
/// and every tab shows the `ProcessingCard`; the chip is the header's only
/// processing signal, the card has the one bar.
struct MeetingDetailView: View {
  @Bindable var model: MeetingDetailViewModel
  let controller: AppController
  @State private var tagsText = ""
  @State private var editingTags = false
  @State private var showsSpeakers = false

  var body: some View {
    VStack(alignment: .leading, spacing: 0) {
      if let meeting = model.meeting {
        header(meeting)
        tabBar
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

  private var showsSpeakersRow: Bool {
    guard let export = model.export else { return false }
    return export.meeting.state == .ready && !export.speakers.isEmpty
  }

  private func header(_ meeting: Meeting) -> some View {
    VStack(alignment: .leading, spacing: Theme.Space.sm) {
      HStack(alignment: .firstTextBaseline) {
        Text(meeting.title)
          .font(.steno(Theme.TextSize.xl, weight: .semibold))
          .foregroundStyle(Color.stenoStrong)
          .textSelection(.enabled)
        Spacer()
        if let entry = controller.progress.entry(for: meeting.id) {
          StatusChip(text: entry.title, color: Color.stenoInfo)
        } else {
          StatusChip(meeting.state)
        }
      }
      HStack(spacing: Theme.Space.md) {
        Text(meeting.startedAt, format: .dateTime.year().month().day().hour().minute())
        if meeting.duration > 0 { Text(meeting.duration.clockText) }
        Text(meeting.source.label)
        if let language = meeting.language { Text(language.localizedName()) }
        if let usage = meeting.llmUsage {
          Text("\(usage.promptTokens + usage.completionTokens) tokens")
        }
      }
      .font(.steno(Theme.TextSize.xxs))
      .foregroundStyle(Color.stenoFaint)
      if let sentence = meeting.endReason?.sentence {
        MessageRow(kind: .info, text: sentence)
          .accessibilityIdentifier("end-reason")
      }
      if case .failed(let reason) = meeting.state {
        MessageRow(kind: .error, text: reason)
      }
      if model.recordingStatusText != nil || model.showsKeepToggle {
        recordingLine
      }
      if showsSpeakersRow {
        SpeakersRow(model: model.speakers, isPresented: $showsSpeakers)
          .popover(isPresented: $showsSpeakers, arrowEdge: .bottom) {
            SpeakersPopover(model: model.speakers)
          }
      }
      HStack(spacing: Theme.Space.sm) {
        tagsEditor(meeting)
        Spacer()
        Menu {
          Picker("Template", selection: .action({ meeting.templateID }, model.setTemplate)) {
            ForEach(model.templates) { template in
              Text(template.displayName).tag(template.id)
            }
          }
          Button("Re-run summary") { Task { await model.rerunSummary() } }
            .disabled(!model.canRerun)
          Button("Re-export") { Task { await model.reexport() } }
            .disabled(!model.canRerun)
          if let url = model.export?.audio?.url {
            Divider()
            Button("Reveal recording in Finder") {
              NSWorkspace.shared.activateFileViewerSelecting([url])
            }
            .disabled(!model.recordingFilesExist)
          }
        } label: {
          Label("Actions", systemImage: "ellipsis.circle")
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .disabled(model.isBusy)
      }
      if let error = model.error {
        MessageRow(kind: .error, text: error)
      }
    }
    .padding(Theme.Space.lg)
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

  private func tagsEditor(_ meeting: Meeting) -> some View {
    HStack(spacing: Theme.Space.xs) {
      if editingTags {
        StenoTextField("tags, comma separated", text: $tagsText)
          .frame(width: 240)
          .onSubmit {
            editingTags = false
            let text = tagsText
            Task { await model.setTags(text: text) }
          }
      } else {
        ForEach(meeting.tags, id: \.self) { tag in
          StatusChip(text: "#\(tag)", style: .neutral)
        }
        Button(meeting.tags.isEmpty ? "Add tags" : "Edit tags") {
          tagsText = meeting.tags.joined(separator: ", ")
          editingTags = true
        }
        .buttonStyle(.plain)
        .font(.steno(Theme.TextSize.xxs))
        .foregroundStyle(Color.stenoFaint)
      }
    }
  }

  private var tabBar: some View {
    HStack(spacing: Theme.Space.xs) {
      ForEach(MeetingDetailViewModel.Tab.allCases) { tab in
        Button {
          withAnimation(Motion.functional) { model.tab = tab }
        } label: {
          Text(tab.title)
            .font(.steno(Theme.TextSize.xs, weight: model.tab == tab ? .semibold : .regular))
            .foregroundStyle(model.tab == tab ? Color.stenoStrong : Color.stenoMutedForeground)
            .padding(.horizontal, Theme.Space.md)
            .padding(.vertical, Theme.Space.xs + 2)
            .background(
              Theme.Radius.sm.shape
                .fill(model.tab == tab ? Color.stenoSecondary : Color.clear))
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("tab-\(tab.rawValue)")
        .accessibilityAddTraits(model.tab == tab ? [.isSelected] : [])
      }
      Spacer()
    }
    .padding(.horizontal, Theme.Space.lg)
    .padding(.bottom, Theme.Space.sm)
  }

  @ViewBuilder
  private var content: some View {
    let progress = controller.progress.entry(for: model.id)
    Group {
      switch model.tab {
      case .summary: SummaryTab(model: model, progress: progress)
      case .transcript: TranscriptTab(model: model, progress: progress)
      case .tasks: TasksTab(model: model, progress: progress)
      case .scratchpad: ScratchpadTab(model: model, progress: progress)
      }
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity)
    .accessibilityElement(children: .contain)
    .accessibilityIdentifier("tab-content-\(model.tab.rawValue)")
  }

  private func footer(_ meeting: Meeting) -> some View {
    HStack(spacing: Theme.Space.md) {
      if model.deliveries.isEmpty {
        Text("Not delivered yet")
          .font(.steno(Theme.TextSize.xxs))
          .foregroundStyle(Color.stenoFaint)
      }
      ForEach(model.deliveries) { delivery in
        DeliveryBadge(delivery: delivery)
      }
      Spacer()
      if model.isBusy { ProgressView().controlSize(.small) }
    }
    .padding(.horizontal, Theme.Space.lg)
    .padding(.vertical, Theme.Space.sm)
  }
}

struct DeliveryBadge: View {
  let delivery: Delivery

  var body: some View {
    HStack(spacing: Theme.Space.xs) {
      switch delivery.status {
      case .pending:
        StatusChip(text: "\(delivery.destinationID): pending", color: Color.stenoInfo)
      case .delivered:
        StatusChip(text: "\(delivery.destinationID): delivered", color: Color.stenoLive)
      case .failed(let message):
        StatusChip(text: "\(delivery.destinationID): failed", color: Color.stenoDestructive)
          .help(message)
      }
      if let folder = delivery.receipt.map({ $0.folderURL }) {
        Button {
          NSWorkspace.shared.activateFileViewerSelecting([folder])
        } label: {
          Image(systemName: "folder")
        }
        .buttonStyle(.plain)
        .foregroundStyle(Color.stenoFaint)
        .help("Reveal in Finder")
        .accessibilityLabel("Reveal \(delivery.destinationID) folder in Finder")
      }
      if let at = delivery.lastAttemptAt {
        Text(at, format: .dateTime.hour().minute())
          .font(.steno(Theme.TextSize.xxxs))
          .foregroundStyle(Color.stenoGhost)
      }
    }
  }
}
