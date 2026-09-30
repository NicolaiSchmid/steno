import AppKit
import Foundation
import StenoAdapters
import StenoAudio
import StenoBridge
import StenoCore

/// The main window's host (plan Decisions 5 and 6). Owns the list model and
/// the detail model of the selected meeting (recreated when the selection
/// changes, as the SwiftUI window did), follows them and the controller's
/// recorder, progress model and setup state with observation tracking, and
/// publishes one full snapshot per topic to the attached sink, coalesced to
/// the next main-actor turn; `recording` at most 20 Hz. Commands map one to
/// one onto the view models' public methods, so no rule lives here. The
/// destructive ones (`meetings.delete`, `meeting.deleteRecordingNow`, and
/// `meeting.setKeepAudio` turning keep off when that deletes the recording
/// now) ask through `NSAlert` first, the one native surface the page cannot
/// draw, and reply whether the user confirmed; a decline changes nothing.
@MainActor
final class MainWindowBridge: BridgeHost {
  /// What the page asks the host to open: another window, or Settings on a
  /// section. `MainWindow` installs the scene's `openWindow` and
  /// `openSettings` actions here; until it does, nothing opens.
  typealias OpenWindow = @MainActor (WindowParams) -> Void
  /// The destructive confirmation: `NSAlert` in the app, a stub in tests.
  typealias Confirm = @MainActor (ConfirmDestructiveParams) async -> Bool

  let controller: AppController
  let list: MeetingListViewModel
  private(set) var detail: MeetingDetailViewModel?
  /// The window's appearance as `MainWindow` reads it from the environment;
  /// part of the `app` snapshot.
  var appearance: BridgeAppearance = .light {
    didSet { if appearance != oldValue { schedule(.app) } }
  }
  var openWindow: OpenWindow = { _ in }
  /// The page's last reported size. Nothing reads it yet; a later plan's
  /// layout-dependent replies will.
  private(set) var layout: PageLayoutParams?

  private let confirm: Confirm
  private weak var events: (any BridgeEventSink)?
  /// True from `page.ready`: before it the page has no `window.steno` and
  /// an emit would be lost, so tracking is armed but nothing is sent.
  private var pageReady = false
  /// True between `run()`'s start and its cancellation.
  private var running = false
  private var pending: Set<BridgeTopic> = []
  private var detailTasks: [Task<Void, Never>] = []
  private var lastRecordingPublish: ContinuousClock.Instant?
  private var recordingThrottle: Task<Void, Never>?
  /// Whether the list had meetings at its last flush. The `app` snapshot
  /// reads this instead of `list.all`, so the app topic is re-published
  /// when the list empties or fills and not with every list change; the
  /// first fill also selects the newest meeting, as the window always did.
  private var hasMeetings = false
  /// `meetings.select` for a meeting the list does not have yet (a deep
  /// link racing the store): kept until its row is listed, then selected.
  private var pendingSelection: UUID?

  /// 20 Hz: the level meter's rate on the page.
  static let recordingInterval: Duration = .milliseconds(50)
  /// The main window's topics, in the order `page.ready` publishes them.
  static let topics: [BridgeTopic] = [.app, .recording, .progress, .meetingsList, .meetingDetail]

  static let deleteMeetingMessage =
    "The transcript, summary, tasks and the recording on this Mac are removed. Files already exported to Obsidian stay. People stay."
  static let deleteRecordingPrompt = ConfirmDestructiveParams(
    title: "Delete this recording now?",
    message: "The recording is deleted shortly. The transcript, summary and exports stay.",
    confirmTitle: "Delete recording")

  init(controller: AppController, confirm: Confirm? = nil) {
    self.controller = controller
    self.list = MeetingListViewModel(
      store: controller.environment.store, clock: controller.environment.clock)
    let alert: Confirm = { await Self.presentAlert($0) }
    self.confirm = confirm ?? alert
  }

  /// Runs for the window's lifetime (`MainWindow`'s `.task`): arms every
  /// topic's tracking, follows the meeting list, and on cancellation tears
  /// the detail model down.
  func run() async {
    running = true
    for topic in Self.topics { flush(topic) }
    await list.observe()
    running = false
    recordingThrottle?.cancel()
    recordingThrottle = nil
    tearDownDetail()
  }

  func attach(_ events: any BridgeEventSink) {
    self.events = events
  }

  // MARK: - Publishing

  /// Asks for a publish of `topic` on a later main-actor turn; a second ask
  /// before that turn is folded into it. `recording` waits out the rest of
  /// its 50 ms interval first.
  private func schedule(_ topic: BridgeTopic) {
    guard running, pending.insert(topic).inserted else { return }
    if topic == .recording, let last = lastRecordingPublish {
      let wait = Self.recordingInterval - (ContinuousClock.now - last)
      if wait > .zero {
        recordingThrottle?.cancel()
        recordingThrottle = Task { @MainActor [weak self] in
          try? await Task.sleep(for: wait)
          guard !Task.isCancelled else { return }
          self?.flush(.recording)
        }
        return
      }
    }
    Task { @MainActor [weak self] in self?.flush(topic) }
  }

  /// Builds the topic's snapshot inside observation tracking, so the next
  /// change to anything it read schedules the next publish, and emits it
  /// once the page is ready. The list flush also owns the selection's side
  /// effects (first fill, a selection waiting for its row, detail model
  /// swap) and the `hasMeetings` flag; the app flush consumes the
  /// controller's meeting request after the snapshot has carried it once.
  private func flush(_ topic: BridgeTopic) {
    guard running else { return }
    pending.remove(topic)
    switch topic {
    case .meetingsList:
      followListFill()
      syncDetail()
    case .meetingDetail:
      syncDetail()
    default:
      break
    }
    var snapshot: (any Encodable)?
    withObservationTracking {
      snapshot = self.snapshot(for: topic)
    } onChange: { [weak self] in
      Task { @MainActor [weak self] in self?.schedule(topic) }
    }
    if topic == .recording { lastRecordingPublish = ContinuousClock.now }
    if pageReady, let events {
      if let snapshot {
        // The sink encodes once (`emit(_:snapshot:)`); the existential is
        // opened onto its generic parameter.
        events.emit(topic, snapshot: snapshot)
      } else if topic == .meetingDetail {
        // `null` until a meeting is selected and its export has loaded; the
        // page shows its empty pane.
        events.emit(BridgeEvent(topic: .meetingDetail, payload: .null))
      }
    }
    if topic == .app { consumeMeetingRequest() }
  }

  /// The topic's snapshot from the view models as they stand; nil for a
  /// detail without an export yet, and for topics this window never
  /// publishes. Each reads only its own inputs: the `app` snapshot reads
  /// the bridge's `hasMeetings`, not the list, and the list snapshot reads
  /// the list model, not the progress model.
  private func snapshot(for topic: BridgeTopic) -> (any Encodable)? {
    switch topic {
    case .app:
      return AppSnapshot(controller: controller, appearance: appearance, hasMeetings: hasMeetings)
    case .recording:
      return RecordingSnapshot(recorder: controller.recorder)
    case .progress:
      return ProgressSnapshot(model: controller.progress)
    case .meetingsList:
      return MeetingsListSnapshot(list: list)
    case .meetingDetail:
      return detail.flatMap { MeetingDetailSnapshot(detail: $0) }
    default:
      return nil
    }
  }

  /// The menu bar or the detection prompt asked for a meeting: it becomes
  /// the selection and the request is cleared, so the next `app` snapshot
  /// carries nil and the page treats it as consumed.
  private func consumeMeetingRequest() {
    guard let requested = controller.requestedMeetingID else { return }
    list.selection = requested
    controller.requestedMeetingID = nil
  }

  /// The list flush's side effects, outside observation tracking: a
  /// selection asked for ahead of its row is applied once the row is
  /// listed, the first fill after the window opened selects the newest
  /// meeting, and `hasMeetings` flips (re-publishing `app`) only when the
  /// list empties or fills.
  private func followListFill() {
    let filled = !list.all.isEmpty
    if let pending = pendingSelection, list.all.contains(where: { $0.id == pending }) {
      pendingSelection = nil
      list.selection = pending
    }
    if filled, !hasMeetings, list.selection == nil, let first = list.meetings.first {
      list.selection = first.id
    }
    if filled != hasMeetings {
      hasMeetings = filled
      schedule(.app)
    }
  }

  /// One detail model per selected meeting, its three store observations
  /// started here and ended when the selection moves on.
  private func syncDetail() {
    let selection = list.selection
    guard selection != detail?.id else { return }
    tearDownDetail()
    if let selection {
      let model = MeetingDetailViewModel(
        meetingID: selection, environment: controller.environment,
        initialSettings: controller.storedSettings)
      detail = model
      detailTasks = [
        Task { await model.observe() },
        Task { await model.observeDeliveries() },
        Task { await model.observeSettings() },
      ]
    }
    schedule(.meetingDetail)
  }

  /// The old view's `onDisappear`: playback stops, a pending re-export is
  /// flushed by the model itself, and pending notes are saved.
  private func tearDownDetail() {
    guard let leaving = detail else { return }
    for task in detailTasks { task.cancel() }
    detailTasks = []
    detail = nil
    leaving.viewDisappeared()
    Task { await leaving.flushScratchpad() }
  }

  // MARK: - Commands

  func handle(_ request: BridgeRequest) async throws -> JSONValue? {
    switch request.method {
    case .pageReady:
      pageReady = true
      for topic in Self.topics { flush(topic) }
    case .pageLayout:
      layout = try params(PageLayoutParams.self, request)

    case .meetingsSetFilter:
      list.stateFilter = try params(SetFilterParams.self, request).filter.modelFilter
    case .meetingsSetTagFilter:
      // `null` params clear the tag too.
      list.tagFilter = try optionalParams(SetTagFilterParams.self, request)?.tag
    case .meetingsSetQuery:
      list.query = try params(SetQueryParams.self, request).query
    case .meetingsSelect:
      let id = try params(MeetingIDParams.self, request).meetingID
      if list.all.contains(where: { $0.id == id }) {
        pendingSelection = nil
        list.selection = id
      } else {
        // A deep link ahead of its row (a recording that just started, a
        // meeting the phone is handing over): selected when the row lands.
        pendingSelection = id
      }
    case .meetingsDelete:
      let id = try params(MeetingIDParams.self, request).meetingID
      guard let meeting = list.all.first(where: { $0.id == id }) else { throw Self.noSuchMeeting }
      guard MeetingListViewModel.canDelete(meeting) else {
        throw BridgeError(
          code: .failed,
          message: meeting.state == .recording
            ? "This meeting is still recording." : "This meeting is still being processed.")
      }
      let confirmed = await confirm(
        ConfirmDestructiveParams(
          title: "Delete “\(meeting.displayTitle(calendar: list.calendar))”?",
          message: Self.deleteMeetingMessage, confirmTitle: "Delete"))
      if confirmed { await list.delete(id) }
      return try reply(ConfirmReply(confirmed: confirmed))

    case .meetingSetTab:
      let tab = try params(SetTabParams.self, request).tab
      let detail = try requireDetail()
      detail.tab = tab.modelTab
    case .meetingSetTags:
      let tags = try params(SetTagsParams.self, request).tags
      let detail = try requireDetail()
      await detail.setTags(MeetingDetailViewModel.tags(from: tags.joined(separator: ",")))
    case .meetingSetTemplate:
      let templateID = try params(SetTemplateParams.self, request).templateID
      let detail = try requireDetail()
      await detail.setTemplate(templateID)
    case .meetingRerunSummary:
      let detail = try requireDetail()
      await detail.rerunSummary()
    case .meetingReexport:
      let detail = try requireDetail()
      await detail.reexport()
    case .meetingSetKeepAudio:
      let keep = try params(SetBoolParams.self, request).value
      let detail = try requireDetail()
      if !keep, detail.wouldDeleteNow {
        guard await confirm(Self.deleteRecordingPrompt) else {
          return try reply(ConfirmReply(confirmed: false))
        }
      }
      await detail.setKeepAudio(keep)
      return try reply(ConfirmReply(confirmed: true))
    case .meetingDeleteRecordingNow:
      let detail = try requireDetail()
      guard await confirm(Self.deleteRecordingPrompt) else {
        return try reply(ConfirmReply(confirmed: false))
      }
      await detail.setKeepAudio(false)
      return try reply(ConfirmReply(confirmed: true))
    case .meetingSaveNotes:
      let notes = try params(SaveNotesParams.self, request)
      if let detail, detail.id == notes.meetingID {
        detail.saveScratchpad(notes.text)
      } else {
        try await saveNotes(notes)
      }
    case .meetingFlushNotes:
      let id = try params(MeetingIDParams.self, request).meetingID
      // Another meeting's notes were written when they arrived, so only the
      // selection's debounce can have anything to flush.
      if let detail, detail.id == id { await detail.flushScratchpad() }
    case .meetingRevealRecording:
      let detail = try requireDetail()
      guard let url = detail.export?.audio?.url, detail.recordingFilesExist else {
        throw BridgeError(code: .notFound, message: "The recording is no longer on this Mac.")
      }
      NSWorkspace.shared.activateFileViewerSelecting([url])
    case .meetingRevealExport:
      let deliveries = try requireDetail().deliveries
      guard let folder = deliveries.compactMap(\.receipt).first?.folderURL else {
        throw BridgeError(code: .notFound, message: "Nothing has been exported yet.")
      }
      NSWorkspace.shared.activateFileViewerSelecting([folder])

    case .speakersOptions:
      let query = try params(SpeakerOptionsParams.self, request)
      let speakers = try requireDetail().speakers
      return try reply(
        SpeakerOptionsReply(
          prefill: speakers.prefill(for: query.speakerID),
          options: speakers.options(for: query.speakerID, query: query.query)
            .map(SpeakerOption.init)))
    case .speakersSelect:
      let selection = try params(SelectSpeakerParams.self, request)
      let detail = try requireDetail()
      let option = try Self.option(from: selection.option, speakers: detail.speakers)
      await detail.speakers.select(option, for: selection.speakerID)
      // The page has no "picker closed" moment the host can see; one
      // re-export per change is idempotent and the model retries a refusal.
      await detail.pickerClosed()
    case .speakersPlay:
      let speakerID = try params(SpeakerIDParams.self, request).speakerID
      try requireDetail().speakers.play(speakerID)
    case .speakersStop:
      try requireDetail().speakers.stopPlayback()

    case .recordingStart:
      let mode = try params(StartRecordingParams.self, request).mode
      await controller.startRecordingFromWindow(mode: mode == .call ? .call : .inPerson)
    case .recordingStop:
      await controller.recorder.stop()
    case .recordingToggle:
      await controller.recorder.toggleRecording()
    case .recordingKeepGoing:
      controller.recorder.keepRecording()
    case .recordingClearMessages:
      controller.recorder.clearMessages()

    case .setupDismissBanner:
      controller.dismissSetupBanner()

    case .systemOpenURL:
      let text = try params(OpenURLParams.self, request).url
      guard let url = URL(string: text), let scheme = url.scheme?.lowercased(),
        scheme == "https" || scheme == "mailto"
      else {
        throw BridgeError(
          code: .invalidParams, message: "Only https: and mailto: links open from the page.")
      }
      NSWorkspace.shared.open(url)
    case .systemOpenSystemSettings:
      let kind = try params(PermissionKindParams.self, request).kind
      guard let permission = PermissionKind(rawValue: kind.rawValue) else {
        throw BridgeError(code: .invalidParams, message: "Unknown permission \(kind.rawValue).")
      }
      controller.environment.permissions.openSystemSettings(for: permission)
    case .updatesCheck:
      controller.menuBar.checkForUpdates()
    case .windowOpen:
      let target = try params(WindowParams.self, request)
      openWindow(target)
    case .uiConfirmDestructive:
      let prompt = try params(ConfirmDestructiveParams.self, request)
      let confirmed = await confirm(prompt)
      return try reply(ConfirmReply(confirmed: confirmed))

    default:
      // `window.close`, the Settings and the onboarding methods belong to
      // the other windows' hosts.
      throw BridgeError(
        code: .unknownMethod,
        message: "The main window does not answer \(request.method.rawValue).")
    }
    return nil
  }

  private static let noSuchMeeting = BridgeError(
    code: .notFound, message: "No meeting with that id is listed.")

  private func requireDetail() throws -> MeetingDetailViewModel {
    guard let detail else { throw BridgeError(code: .notFound, message: "No meeting is selected.") }
    return detail
  }

  /// Notes for a meeting that is not the selection: typed before the
  /// selection moved and saved after, they are written to that meeting at
  /// once, so they never land on the meeting selected now. The debounce is
  /// the detail model's; a late save is one write.
  private func saveNotes(_ notes: SaveNotesParams) async throws {
    let environment = controller.environment
    do {
      try await environment.store.update(meetingID: notes.meetingID, now: environment.now()) {
        $0.scratchpad = notes.text
      }
    } catch let failure as MeetingStoreError {
      if case .meetingNotFound = failure { throw Self.noSuchMeeting }
      throw BridgeError(code: .failed, message: "Notes could not be saved: \(failure.description)")
    } catch {
      throw BridgeError(code: .failed, message: "Notes could not be saved: \(error)")
    }
  }

  /// The method's params decoded into its contract type; a missing or
  /// unreadable value is `invalidParams`.
  private func params<T: Decodable>(_ type: T.Type, _ request: BridgeRequest) throws -> T {
    guard let decoded = try optionalParams(type, request) else {
      throw BridgeError(
        code: .invalidParams, message: "\(request.method.rawValue) needs params.")
    }
    return decoded
  }

  private func optionalParams<T: Decodable>(_ type: T.Type, _ request: BridgeRequest) throws -> T? {
    guard let value = request.params, value != .null else { return nil }
    do {
      return try BridgeJSON.decode(T.self, from: BridgeJSON.encode(value))
    } catch {
      throw BridgeError(
        code: .invalidParams, message: "\(request.method.rawValue): \(error)")
    }
  }

  /// A reply value as the dispatcher carries it.
  private func reply(_ value: some Encodable) throws -> JSONValue {
    try BridgeJSON.decode(JSONValue.self, from: BridgeDispatcher.encoder().encode(value))
  }

  /// The page's option back into the model's: a person is looked up among
  /// the people the picker was built from; a create row carries its name.
  private static func option(from option: SpeakerOption, speakers: SpeakersViewModel) throws
    -> SpeakerOptions.Option
  {
    switch option.kind {
    case .person:
      let known = speakers.persons + speakers.recent + (speakers.export?.persons ?? [])
      guard let id = option.personID, let person = known.first(where: { $0.id == id }) else {
        throw BridgeError(code: .notFound, message: "No person with that id.")
      }
      return SpeakerOptions.Option(kind: .person(person))
    case .create:
      let name = option.label.trimmingCharacters(in: .whitespaces)
      guard !name.isEmpty else {
        throw BridgeError(code: .invalidParams, message: "A new person needs a name.")
      }
      return SpeakerOptions.Option(kind: .create(name))
    case .unknown:
      throw BridgeError(
        code: .failed, message: "Marking a speaker as unknown is not available yet.")
    }
  }

  /// `NSAlert` as a sheet on the key window (else app-modal): the
  /// destructive title first, Cancel second, Escape cancels.
  static func presentAlert(_ params: ConfirmDestructiveParams) async -> Bool {
    let alert = NSAlert()
    alert.messageText = params.title
    alert.informativeText = params.message
    alert.alertStyle = .warning
    alert.addButton(withTitle: params.confirmTitle).hasDestructiveAction = true
    alert.addButton(withTitle: "Cancel").keyEquivalent = "\u{1b}"
    let response: NSApplication.ModalResponse
    if let window = NSApp.keyWindow ?? NSApp.mainWindow {
      response = await alert.beginSheetModal(for: window)
    } else {
      response = alert.runModal()
    }
    return response == .alertFirstButtonReturn
  }
}

extension SpeakerOption {
  /// A picker row in the wire vocabulary; the tag ("Sounds like",
  /// "Attendee") is the row's detail.
  init(_ option: SpeakerOptions.Option) {
    switch option.kind {
    case .person(let person):
      self.init(
        kind: .person, label: person.displayName, detail: option.tag?.rawValue,
        personID: person.id)
    case .create(let name):
      self.init(kind: .create, label: name, detail: option.tag?.rawValue)
    }
  }
}
