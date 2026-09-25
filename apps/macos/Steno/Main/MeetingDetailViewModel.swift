import Foundation
import StenoCore

/// One meeting's detail: the export from `observeMeeting(id:)`, deliveries
/// from `observeDeliveries`, the bundled templates and the selected tab.
/// Summary, Transcript and Tasks are display only; the scratchpad is the
/// one editable text and saves once after a debounce on the injected clock.
/// `observe()` runs from the view's `.task`, so SwiftUI ends the store
/// observations when the selection changes.
@MainActor
@Observable
final class MeetingDetailViewModel: Identifiable {
  enum Tab: String, CaseIterable, Identifiable, Sendable {
    case summary
    case transcript
    case tasks
    case scratchpad

    var id: String { rawValue }
    var title: String { rawValue.capitalized }
  }

  let id: UUID
  private(set) var export: MeetingExport?
  private(set) var deliveries: [Delivery] = []
  private(set) var error: String?
  private(set) var isBusy = false
  var tab: Tab = .summary
  var showsSpeakerReview = false
  let templates = SummaryTemplate.bundled
  static let scratchpadDebounce: Duration = .seconds(1)

  private let store: MeetingStore
  private let settings: SettingsStore
  private let pipeline: () -> ProcessingPipeline
  private let clock: any Clock<Duration>
  private let now: @Sendable () -> Date
  private var scratchpadTask: Task<Void, Never>?
  private var pendingScratchpad: String?
  private var scratchpadEdits = 0

  init(
    meetingID: UUID, store: MeetingStore, settings: SettingsStore,
    pipeline: @escaping () -> ProcessingPipeline, clock: any Clock<Duration>,
    now: @escaping @Sendable () -> Date
  ) {
    self.id = meetingID
    self.store = store
    self.settings = settings
    self.pipeline = pipeline
    self.clock = clock
    self.now = now
  }

  convenience init(meetingID: UUID, environment: AppEnvironment) {
    self.init(
      meetingID: meetingID, store: environment.store, settings: environment.settings,
      pipeline: { environment.pipeline }, clock: environment.clock, now: environment.now)
  }

  /// Follows the export until cancelled (one view `.task`).
  func observe() async {
    do {
      for try await export in store.observeMeeting(id: id) {
        self.export = export
      }
    } catch {
      self.error = "Meeting could not be loaded: \(error)"
    }
  }

  /// Follows the deliveries until cancelled (a second `.task`).
  func observeDeliveries() async {
    do {
      for try await deliveries in store.observeDeliveries(meetingID: id) {
        self.deliveries = deliveries
      }
    } catch {
      self.error = "Deliveries could not be loaded: \(error)"
    }
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

  /// Tags as typed, comma separated: trimmed, lower-cased, deduplicated and
  /// sorted. "Q4, q4 , Strategie" becomes `["q4", "strategie"]`.
  static func tags(from text: String) -> [String] {
    let tags = text.split(separator: ",")
      .map { $0.trimmingCharacters(in: .whitespaces).lowercased() }
      .filter { !$0.isEmpty }
    return Array(Set(tags)).sorted()
  }

  func setTags(text: String) async {
    await setTags(Self.tags(from: text))
  }

  func setTags(_ tags: [String]) async {
    await update("Tags") { $0.tags = tags }
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
  /// default retention from Settings with a fresh expiry from now.
  func setKeepAudio(_ keep: Bool) async {
    do {
      guard var asset = try await store.asset(meetingID: id) else { return }
      if keep {
        asset.retention = .keepForever
        asset.expiresAt = nil
      } else {
        let retention = try await settings.load().defaultRetention
        asset.retention = retention
        asset.expiresAt = retention.expiry(from: now())
      }
      try await store.save(asset)
    } catch {
      self.error = "Retention could not be changed: \(error)"
    }
  }

  var keepsAudio: Bool {
    export?.audio?.retention == .keepForever
  }

  /// Debounced on the injected clock with one sleeper: edits within the
  /// window keep it sleeping, and the last text saves once after a quiet
  /// debounce interval.
  func saveScratchpad(_ text: String) {
    pendingScratchpad = text
    scratchpadEdits += 1
    guard scratchpadTask == nil else { return }
    let clock = self.clock
    scratchpadTask = Task { [weak self] in
      var seen = -1
      while let edits = self?.scratchpadEdits, edits != seen {
        seen = edits
        do {
          try await clock.sleep(for: Self.scratchpadDebounce)
        } catch {
          return
        }
      }
      guard let self else { return }
      // Clear the handle first: `flushScratchpad` cancels a pending task,
      // and cancelling this one would abort the GRDB write inside it.
      self.scratchpadTask = nil
      await self.flushScratchpad()
    }
  }

  /// Saves the pending text now (the view going away, a selection change).
  /// A failed write keeps the text pending for the next flush.
  func flushScratchpad() async {
    scratchpadTask?.cancel()
    scratchpadTask = nil
    guard let text = pendingScratchpad else { return }
    pendingScratchpad = nil
    do {
      try await store.update(meetingID: id, now: now()) { $0.scratchpad = text }
    } catch {
      if pendingScratchpad == nil { pendingScratchpad = text }
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
