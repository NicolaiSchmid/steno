import AppKit
import StenoAdapters
import StenoCore
import SwiftUI

/// Header (title, meta, actions), the four tabs and the delivery footer.
struct MeetingDetailView: View {
  @Bindable var model: MeetingDetailViewModel
  let controller: AppController
  @State private var defaultRetention: AudioRetention = .keepDays(30)
  @State private var tagsText = ""
  @State private var editingTags = false

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
    .task(id: model.id) {
      if let settings = try? await controller.environment.settings.load() {
        defaultRetention = settings.defaultRetention
      }
    }
    .sheet(isPresented: $model.showsSpeakerReview) {
      if let export = model.export {
        SpeakerReviewSheet(
          model: SpeakerReviewViewModel(export: export, environment: controller.environment),
          onFinish: {
            controller.reviewCompleted(meetingID: model.id)
            model.showsSpeakerReview = false
          })
      }
    }
    .onChange(of: controller.pendingReviews[model.id] != nil, initial: true) { _, pending in
      if pending, NSApp.isActive, !model.unconfirmedSpeakers.isEmpty {
        model.showsSpeakerReview = true
      }
    }
  }

  private func header(_ meeting: Meeting) -> some View {
    VStack(alignment: .leading, spacing: Theme.Space.sm) {
      HStack(alignment: .firstTextBaseline) {
        Text(meeting.title)
          .font(.steno(Theme.TextSize.xl, weight: .semibold))
          .foregroundStyle(Color.stenoStrong)
          .textSelection(.enabled)
        Spacer()
        MeetingStateLabel.chip(for: MeetingStateLabel(meeting.state))
      }
      HStack(spacing: Theme.Space.md) {
        Text(meeting.startedAt, format: .dateTime.year().month().day().hour().minute())
        if meeting.duration > 0 { Text(meeting.duration.clockText) }
        Text(meeting.source.label)
        if let language = meeting.language { Text(language.rawValue.uppercased()) }
        if let usage = meeting.llmUsage {
          Text("\(usage.promptTokens + usage.completionTokens) tokens")
        }
      }
      .font(.steno(Theme.TextSize.xxs))
      .foregroundStyle(Color.stenoFaint)
      if case .failed(let reason) = meeting.state {
        MessageRow(kind: .error, text: reason)
      }
      HStack(spacing: Theme.Space.sm) {
        tagsEditor(meeting)
        Spacer()
        if !model.unconfirmedSpeakers.isEmpty {
          Button("Review speakers (\(model.unconfirmedSpeakers.count))") {
            model.showsSpeakerReview = true
          }
          .buttonStyle(StenoPrimaryButtonStyle())
          .accessibilityIdentifier("review-speakers")
        }
        Menu {
          Picker(
            "Template",
            selection: Binding(
              get: { meeting.templateID },
              set: { id in Task { await model.setTemplate(id) } })
          ) {
            ForEach(model.templates) { template in
              Text(template.displayName).tag(template.id)
            }
          }
          Button("Re-run summary") { Task { await model.rerunSummary() } }
            .disabled(!model.canRerun)
          Button("Re-export") { Task { await model.reexport() } }
            .disabled(!model.canRerun)
          Divider()
          Toggle(
            "Keep audio",
            isOn: Binding(
              get: { model.keepsAudio },
              set: { keep in
                Task { await model.setKeepAudio(keep, defaultRetention: defaultRetention) }
              }))
          if let url = model.export?.audio?.url {
            Button("Reveal recording in Finder") {
              NSWorkspace.shared.activateFileViewerSelecting([url])
            }
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

  private func tagsEditor(_ meeting: Meeting) -> some View {
    HStack(spacing: Theme.Space.xs) {
      if editingTags {
        TextField("tags, comma separated", text: $tagsText)
          .textFieldStyle(.roundedBorder)
          .frame(width: 240)
          .onSubmit {
            let tags = tagsText.split(separator: ",")
              .map { $0.trimmingCharacters(in: .whitespaces).lowercased() }
              .filter { !$0.isEmpty }
            editingTags = false
            Task { await model.setTags(Array(Set(tags)).sorted()) }
          }
      } else {
        ForEach(meeting.tags, id: \.self) { tag in
          StatusChip(text: "#\(tag)", color: Color.stenoMutedForeground)
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
              RoundedRectangle(cornerRadius: Theme.Space.radiusSmall, style: .continuous)
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
    Group {
      switch model.tab {
      case .summary: SummaryTab(model: model)
      case .transcript: TranscriptTab(model: model)
      case .tasks: TasksTab(model: model)
      case .scratchpad: ScratchpadTab(model: model)
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
      }
      if let at = delivery.lastAttemptAt {
        Text(at, format: .dateTime.hour().minute())
          .font(.steno(Theme.TextSize.xxxs))
          .foregroundStyle(Color.stenoGhost)
      }
    }
  }
}
