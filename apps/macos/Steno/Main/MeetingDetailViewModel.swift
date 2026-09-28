import Foundation
import StenoCore

/// One meeting's detail: the export from `observeMeeting(id:)`, deliveries
/// from `observeDeliveries`, the bundled templates, the selected tab and
/// the speakers (`speakers`, fed from every export tick). Summary,
/// Transcript and Tasks are display only; the scratchpad is the one editable
/// text and saves once after a debounce on the injected clock. Speaker
/// changes apply at once and mark the meeting dirty; the vault re-exports
/// when the picker closes or the view goes away (`pickerClosed`,
/// `viewDisappeared`). `observe()` runs from the view's `.task`, so SwiftUI
/// ends the store observations when the selection changes.
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
  let speakers: SpeakersViewModel
  /// A speaker was named or reassigned since the last re-export.
  private(set) var speakersDirty = false
  /// The last re-export was refused because the pipeline held the meeting;
  /// try again when the export shows it `.ready`.
  private var retryRedeliverWhenReady = false
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
    self.speakers = SpeakersViewModel(store: store, now: now)
    speakers.onWrite = { [weak self] in self?.speakersDirty = true }
  }

  convenience init(meetingID: UUID, environment: AppEnvironment) {
    self.init(
      meetingID: meetingID, store: environment.store, settings: environment.settings,
      pipeline: { environment.pipeline }, clock: environment.clock, now: environment.now)
  }

  /// Follows the export until cancelled (one view `.task`), feeding the
  /// speakers and retrying a refused re-export once the meeting is ready.
  func observe() async {
    do {
      for try await export in store.observeMeeting(id: id) {
        self.export = export
        guard let export else { continue }
        speakers.update(export: export)
        if retryRedeliverWhenReady, export.meeting.state == .ready {
          retryRedeliverWhenReady = false
          await redeliverSpeakerChanges()
        }
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

  /// The same summary as sections, what the tab lays out.
  var summarySections: [RenderedSection] {
    export.map(SummaryMarkdown.sections(for:)) ?? []
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

  // MARK: - Speakers

  /// The header popover or a transcript picker closed: re-export once when a
  /// speaker changed, else nothing. A refusal (the pipeline holds the
  /// meeting) keeps the flag and retries at the next `.ready` tick.
  func pickerClosed() async {
    guard speakersDirty else { return }
    await redeliverSpeakerChanges()
  }

  private func redeliverSpeakerChanges() async {
    do {
      try await pipeline().redeliver(meetingID: id)
      speakersDirty = false
    } catch {
      retryRedeliverWhenReady = true
    }
  }

  /// The view is going away (selection change, window closed): flush a
  /// pending re-export in a task that outlives this model, with one retry
  /// after a short pause for a pipeline that was still busy.
  func viewDisappeared() {
    speakers.stopPlayback()
    guard speakersDirty else { return }
    speakersDirty = false
    retryRedeliverWhenReady = false
    let pipeline = self.pipeline
    let meetingID = id
    Task { @MainActor in
      do {
        try await pipeline().redeliver(meetingID: meetingID)
      } catch {
        try? await Task.sleep(for: .seconds(3))
        try? await pipeline().redeliver(meetingID: meetingID)
      }
    }
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
