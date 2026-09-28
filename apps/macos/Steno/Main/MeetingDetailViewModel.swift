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
  /// `Settings.llmConfigured` and `vaultConfigured`, seeded from the
  /// settings passed at construction (the controller's last observed value)
  /// and followed by `observeSettings()`: they select the skipped-summary
  /// rows, the footer and the two Actions menu items.
  private(set) var llmConfigured: Bool
  private(set) var vaultConfigured: Bool
  private(set) var error: String?
  private(set) var isBusy = false
  var tab: Tab = .summary
  /// Set by `toggleKeepAudio(false)` when turning the keep off would delete
  /// the recording now; the view asks first.
  var confirmsDeleteNow = false
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
    now: @escaping @Sendable () -> Date, initialSettings: Settings? = nil
  ) {
    self.id = meetingID
    self.store = store
    self.settings = settings
    self.llmConfigured = initialSettings?.llmConfigured ?? false
    self.vaultConfigured = initialSettings?.vaultConfigured ?? false
    self.pipeline = pipeline
    self.clock = clock
    self.now = now
    self.speakers = SpeakersViewModel(store: store, now: now)
    speakers.onWrite = { [weak self] in self?.speakersDirty = true }
  }

  /// `initialSettings` is the controller's `storedSettings`, so the rows
  /// and the footer render the configured state on their first frame.
  convenience init(meetingID: UUID, environment: AppEnvironment, initialSettings: Settings? = nil) {
    self.init(
      meetingID: meetingID, store: environment.store, settings: environment.settings,
      pipeline: { environment.pipeline }, clock: environment.clock, now: environment.now,
      initialSettings: initialSettings)
  }

  /// Follows the export until cancelled (one view `.task`), feeding the
  /// speakers and retrying a refused re-export once the meeting is ready.
  func observe() async {
    do {
      for try await export in store.observeMeeting(id: id) {
        self.export = export
        recordingFilesExist =
          export?.audio.map { FileManager.default.fileExists(atPath: $0.url.path) } ?? false
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

  /// Follows Settings so the keep toggle appears and disappears with the
  /// default rule (a third view `.task`).
  func observeSettings() async {
    do {
      for try await settings in settings.observe() {
        defaultRetention = settings.defaultRetention
        llmConfigured = settings.llmConfigured
        vaultConfigured = settings.vaultConfigured
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

  /// The pipeline accepts a re-run or a re-export: ready or failed.
  var canRerun: Bool {
    guard let meeting else { return false }
    return meeting.state == .ready || meeting.state.isFailed
  }

  /// "Re-run summary" and "Run summary": the pipeline would throw without an
  /// endpoint (`rerunSummary` with a nil summarizer), so the app disables
  /// the action first.
  var canRerunSummary: Bool { canRerun && llmConfigured }

  /// "Re-export" and "Export now": without a vault there is nowhere to
  /// export to.
  var canReexport: Bool { canRerun && vaultConfigured }

  /// Which empty-tab row the Summary and Tasks tabs show; see `SummaryStatus`.
  var summaryStatus: SummaryStatus {
    SummaryStatus(meeting: meeting, llmConfigured: llmConfigured)
  }

  /// What the footer shows; see `ExportStatus`.
  var exportStatus: ExportStatus {
    ExportStatus(deliveries: deliveries, vaultConfigured: vaultConfigured)
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
  /// rule would delete the recording now ("Until processed, then delete"
  /// on a ready meeting with every export done), else applies at once.
  func toggleKeepAudio(_ keep: Bool) async {
    if !keep, wouldDeleteNow {
      confirmsDeleteNow = true
      return
    }
    await setKeepAudio(keep)
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

  /// `keep` sets `.keepForever`; off restores the default retention from
  /// Settings. Both go through `ProcessingPipeline.applyRetention`, so the
  /// stamp (only on a ready meeting whose every delivery is `.delivered`, or
  /// has none) and the `retentionApplied` post are the pipeline's, not a
  /// second copy here.
  func setKeepAudio(_ keep: Bool) async {
    confirmsDeleteNow = false
    do {
      let rule: AudioRetention =
        if keep { .keepForever } else { try await settings.load().defaultRetention }
      try await pipeline().applyRetention(meetingID: id, rule: rule)
    } catch {
      self.error = "Retention could not be changed: \(error)"
    }
  }

  /// The pipeline's stamp guard over the observed state: the default rule
  /// stamps today, the meeting is ready and nothing is left to export.
  var wouldDeleteNow: Bool {
    defaultRetention == .deleteAfterProcessing && meeting?.state == .ready
      && deliveries.allDelivered
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

  private func update(_ what: String, _ mutate: @escaping @Sendable (inout Meeting) -> Void) async {
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
