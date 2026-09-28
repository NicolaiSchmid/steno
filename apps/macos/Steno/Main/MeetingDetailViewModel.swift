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

  /// What the header's "Recording" line says. nil for the one case Settings
  /// already covers: kept forever with the files present.
  enum RecordingStatus: Equatable, Sendable {
    /// The master file is gone (swept, or removed by hand).
    case deleted
    /// `expiresAt` is set; the sweep removes the files from that day on.
    case deletes(on: Date)
    /// A finite rule, no stamp, meeting ready, a delivery not delivered.
    case keptUntilExportSucceeds
    /// A finite rule, no stamp, meeting failed: re-processing needs the audio.
    case keptProcessingFailed
    /// A finite rule, no stamp, meeting still on its way to the retention stage.
    case keptWhileProcessing
  }

  let id: UUID
  private(set) var export: MeetingExport?
  private(set) var deliveries: [Delivery] = []
  /// `FileManager.fileExists` on the master, rechecked whenever the export
  /// changes.
  private(set) var recordingFilesExist = false
  /// `Settings.defaultRetention`, followed by `observeSettings()`.
  private(set) var defaultRetention: AudioRetention = .keepForever
  private(set) var error: String?
  private(set) var isBusy = false
  var tab: Tab = .summary
  var showsSpeakerReview = false
  /// Set by `toggleKeepAudio(false)` when turning the keep off would delete
  /// the recording at the next sweep; the view asks first.
  var confirmsDeleteNow = false
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
        recordingFilesExist =
          export?.audio.map { FileManager.default.fileExists(atPath: $0.url.path) } ?? false
      }
    } catch {
      self.error = "Meeting could not be loaded: \(error)"
    }
  }

  /// Follows Settings so the keep toggle appears and disappears with the
  /// default rule (a third view `.task`).
  func observeSettings() async {
    do {
      for try await settings in settings.observe() {
        defaultRetention = settings.defaultRetention
      }
    } catch {
      self.error = "Settings could not be loaded: \(error)"
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

  // MARK: - Recording line

  var recordingStatus: RecordingStatus? {
    guard let export, let asset = export.audio else { return nil }
    guard recordingFilesExist else { return .deleted }
    if asset.retention == .keepForever { return nil }
    if let expiresAt = asset.expiresAt { return .deletes(on: expiresAt) }
    switch export.meeting.state {
    case .ready:
      // All delivered and still unstamped: the retention stage is about to
      // run.
      return deliveries.allDelivered ? .keptWhileProcessing : .keptUntilExportSucceeds
    case .failed:
      return .keptProcessingFailed
    case .recording, .queued, .processing:
      return .keptWhileProcessing
    }
  }

  /// The line's text; the date in the user's locale, "today" once the
  /// expiry falls on or before the current day. Never past tense for a
  /// date to come.
  var recordingStatusText: String? {
    guard let status = recordingStatus else { return nil }
    switch status {
    case .deleted: return "Recording deleted"
    case .deletes(let date):
      let today = now()
      if date <= today || Calendar.current.isDate(date, inSameDayAs: today) {
        return "Deletes today"
      }
      return "Deletes on \(date.formatted(date: .abbreviated, time: .omitted))"
    case .keptUntilExportSucceeds: return "Kept until the export succeeds"
    case .keptProcessingFailed: return "Kept; processing failed"
    case .keptWhileProcessing: return "Kept while processing"
    }
  }

  /// The per-meeting keep is offered only when the default does not keep
  /// everything already and there is a file to keep.
  var showsKeepToggle: Bool {
    defaultRetention != .keepForever && recordingFilesExist && export?.audio != nil
  }

  var keepsAudio: Bool {
    export?.audio?.retention == .keepForever
  }

  /// The toggle's action. On keeps at once; off asks first when the default
  /// rule would delete the recording at the next sweep ("Until processed,
  /// then delete" with every export done), else applies at once.
  func toggleKeepAudio(_ keep: Bool) async {
    if !keep, await wouldDeleteNow() {
      confirmsDeleteNow = true
      return
    }
    await setKeepAudio(keep)
  }

  /// `keep` sets `.keepForever` and clears `expiresAt`; off restores the
  /// default retention from Settings and stamps a fresh expiry from now
  /// only when every delivery of the meeting is `.delivered` (or there is
  /// none), the pipeline's own guard. A stamp posts `retentionApplied` so
  /// the app's sweep runs.
  func setKeepAudio(_ keep: Bool) async {
    confirmsDeleteNow = false
    do {
      guard var asset = try await store.asset(meetingID: id) else { return }
      if keep {
        asset.retention = .keepForever
        asset.expiresAt = nil
      } else {
        let retention = try await settings.load().defaultRetention
        asset.retention = retention
        let deliveries = try await store.deliveries(meetingID: id)
        asset.expiresAt = deliveries.allDelivered ? retention.expiry(from: now()) : nil
      }
      try await store.save(asset)
      if asset.expiresAt != nil {
        await store.events.post(.retentionApplied(meetingID: id))
      }
    } catch {
      self.error = "Retention could not be changed: \(error)"
    }
  }

  private func wouldDeleteNow() async -> Bool {
    guard let retention = try? await settings.load().defaultRetention,
      retention == .deleteAfterProcessing
    else { return false }
    let deliveries = (try? await store.deliveries(meetingID: id)) ?? []
    return deliveries.allDelivered
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
