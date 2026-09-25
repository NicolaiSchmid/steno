import Foundation
import StenoCore

/// One meeting's detail: the export from `observeMeeting(id:)`, deliveries
/// from `observeDeliveries`, the bundled templates and the selected tab.
/// Summary, Transcript and Tasks are display only; the scratchpad is the
/// one editable text and saves once after a debounce on the injected clock.
@MainActor
@Observable
final class MeetingDetailViewModel: Identifiable {
  enum Tab: String, CaseIterable, Identifiable, Sendable {
    case summary
    case transcript
    case tasks
    case scratchpad

    var id: String { rawValue }

    var title: String {
      switch self {
      case .summary: "Summary"
      case .transcript: "Transcript"
      case .tasks: "Tasks"
      case .scratchpad: "Scratchpad"
      }
    }
  }

  let id: UUID
  private(set) var export: MeetingExport?
  private(set) var deliveries: [Delivery] = []
  private(set) var error: String?
  private(set) var isBusy = false
  private(set) var scratchpadSaves = 0
  var tab: Tab = .summary
  var showsSpeakerReview = false
  let templates = SummaryTemplate.bundled
  static let scratchpadDebounce: Duration = .seconds(1)

  private let store: MeetingStore
  private let pipeline: () -> ProcessingPipeline
  private let clock: any Clock<Duration>
  private let now: @Sendable () -> Date
  private var observers: [Task<Void, Never>] = []
  private var scratchpadTask: Task<Void, Never>?
  private var pendingScratchpad: String?

  init(
    meetingID: UUID, store: MeetingStore, pipeline: @escaping () -> ProcessingPipeline,
    clock: any Clock<Duration>, now: @escaping @Sendable () -> Date
  ) {
    self.id = meetingID
    self.store = store
    self.pipeline = pipeline
    self.clock = clock
    self.now = now
    observers.append(
      Task { [weak self, store] in
        do {
          for try await export in store.observeMeeting(id: meetingID) {
            guard let self else { return }
            self.export = export
          }
        } catch {
          self?.error = "Meeting could not be loaded: \(error)"
        }
      })
    observers.append(
      Task { [weak self, store] in
        do {
          for try await deliveries in store.observeDeliveries(meetingID: meetingID) {
            guard let self else { return }
            self.deliveries = deliveries
          }
        } catch {
          self?.error = "Deliveries could not be loaded: \(error)"
        }
      })
  }

  convenience init(meetingID: UUID, environment: AppEnvironment) {
    self.init(
      meetingID: meetingID, store: environment.store, pipeline: { environment.pipeline },
      clock: environment.clock, now: environment.now)
  }

  var meeting: Meeting? { export?.meeting }

  /// `SummaryMarkdown.render` over the current export, so a renamed speaker
  /// shows without an LLM re-run.
  var summaryMarkdown: String {
    export.map(SummaryMarkdown.render) ?? ""
  }

  var unconfirmedSpeakers: [Speaker] {
    export?.speakers.filter { !$0.assignment.isConfirmed } ?? []
  }

  var canRerun: Bool {
    guard let meeting else { return false }
    return meeting.state == .ready || meeting.state.isFailed
  }

  func displayName(forSpeaker id: UUID?) -> String {
    guard let id, let export else { return "Unknown" }
    return export.displayName(forSpeaker: id)
  }

  // MARK: - Actions

  func setTags(_ tags: [String]) async {
    await update("Tags") { $0.tags = tags }
  }

  func setTitle(_ title: String) async {
    let trimmed = title.trimmingCharacters(in: .whitespaces)
    guard !trimmed.isEmpty else { return }
    await update("Title") { $0.title = trimmed }
  }

  /// Stores the template and re-runs summarize plus deliver with it.
  func setTemplate(_ templateID: String) async {
    guard SummaryTemplate.bundled(id: templateID) != nil else { return }
    await update("Template") { $0.templateID = templateID }
    await rerunSummary(templateID: templateID)
  }

  func rerunSummary() async {
    guard let meeting else { return }
    await rerunSummary(templateID: meeting.templateID)
  }

  private func rerunSummary(templateID: String) async {
    let meetingID = id
    await run("Summary re-run") {
      try await self.pipeline().rerunSummary(meetingID: meetingID, templateID: templateID)
    }
  }

  /// The only re-export entry point.
  func reexport() async {
    await run("Re-export") { try await self.pipeline().redeliver(meetingID: self.id) }
  }

  /// `keep` sets `.keepForever` and clears `expiresAt`; off restores the
  /// stored default retention with a fresh expiry from now.
  func setKeepAudio(_ keep: Bool, defaultRetention: AudioRetention) async {
    guard var asset = try? await store.asset(meetingID: id) else { return }
    if keep {
      asset.retention = .keepForever
      asset.expiresAt = nil
    } else {
      asset.retention = defaultRetention
      asset.expiresAt = defaultRetention.expiry(from: now())
    }
    do {
      try await store.save(asset)
    } catch {
      self.error = "Retention could not be changed: \(error)"
    }
  }

  var keepsAudio: Bool {
    export?.audio?.retention == .keepForever
  }

  /// Debounced on the injected clock: several edits within a second save
  /// once, with the last text.
  func saveScratchpad(_ text: String) {
    pendingScratchpad = text
    scratchpadTask?.cancel()
    let clock = self.clock
    scratchpadTask = Task { [weak self] in
      do {
        try await clock.sleep(for: Self.scratchpadDebounce)
      } catch {
        return
      }
      guard let self else { return }
      // Clear the handle first: `flushScratchpad` cancels a pending task,
      // and cancelling this one would abort the GRDB write inside it.
      self.scratchpadTask = nil
      await self.flushScratchpad()
    }
  }

  /// Saves the pending text now (the view going away, a selection change).
  func flushScratchpad() async {
    scratchpadTask?.cancel()
    scratchpadTask = nil
    guard let text = pendingScratchpad else { return }
    pendingScratchpad = nil
    do {
      try await store.update(meetingID: id, now: now()) { $0.scratchpad = text }
      scratchpadSaves += 1
    } catch {
      self.error = "Scratchpad could not be saved: \(error)"
    }
  }

  private func update(_ what: String, _ mutate: @escaping @Sendable (inout Meeting) -> Void) async
  {
    do {
      try await store.update(meetingID: id, now: now(), mutate)
    } catch {
      self.error = "\(what) could not be saved: \(error)"
    }
  }

  private func run(_ what: String, _ operation: @escaping () async throws -> Void) async {
    isBusy = true
    defer { isBusy = false }
    do {
      try await operation()
      error = nil
    } catch {
      self.error = "\(what) failed: \(error)"
    }
  }
}
