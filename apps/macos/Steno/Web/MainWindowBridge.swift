import AppKit
import Foundation
import StenoAdapters
import StenoAudio
import StenoBridge
import StenoCore

/// The main window's host (plan Decisions 5 and 6). Owns the list model and
/// the detail model of the selected meeting (recreated when the selection
/// changes, as the SwiftUI window did) and maps them, the controller's
/// recorder, progress model and setup state onto the window's five topics;
/// `TopicPublisher` follows them and publishes, `recording` at most 20 Hz.
/// Commands map one to one onto the view models' public methods, so no rule
/// lives here. The
/// destructive ones (`meetings.delete`, `meeting.deleteRecordingNow`, and
/// `meeting.setKeepAudio` turning keep off when that deletes the recording
/// now) ask through `NSAlert` first, the one native surface the page cannot
/// draw, and reply whether the user confirmed; a decline changes nothing.
@MainActor
final class MainWindowBridge: BridgeHost, TopicSource {
  /// What the page asks the host to open: another window, or Settings on a
  /// section. `MainWindow` installs the scene's `openWindow` action here;
  /// until it does, nothing opens.
  typealias OpenWindow = @MainActor (WindowParams) -> Void
  /// The destructive confirmation: `NSAlert` in the app, a stub in tests.
  typealias Confirm = @MainActor (ConfirmDestructiveParams) async -> Bool

  let controller: AppController
  let list: MeetingListViewModel
  private(set) var detail: MeetingDetailViewModel?
  var openWindow: OpenWindow = { _ in }

  private let confirm: Confirm
  private lazy var publisher = TopicPublisher(
    topics: Self.topics, source: self, minimumInterval: [.recording: Self.recordingInterval])
  private var detailTasks: [Task<Void, Never>] = []
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

  /// Publishes every topic now and lets later changes publish; `run()`
  /// calls it before following the list, and a test that dispatches at once
  /// calls it directly.
  func start() {
    publisher.start()
  }

  /// Runs for the window's lifetime (`MainWindow`'s `.task`): arms every
  /// topic's tracking, follows the meeting list, and on cancellation tears
  /// the detail model down.
  func run() async {
    start()
    await list.observe()
    publisher.stop()
    tearDownDetail()
  }

  func attach(_ events: any BridgeEventSink) {
    publisher.attach(events)
  }

  // MARK: - Topics

  /// The topic's snapshot from the view models as they stand; `null` for a
  /// detail without a selection or an export yet (the page shows its empty
  /// pane), nil for topics this window never publishes. Each reads only its
  /// own inputs: the `app` snapshot reads the bridge's `hasMeetings`, not
  /// the list, and the list snapshot reads the list model, not the progress
  /// model.
  func snapshot(for topic: BridgeTopic) -> (any Encodable)? {
    switch topic {
    case .app:
      return AppSnapshot(controller: controller, hasMeetings: hasMeetings)
    case .recording:
      return RecordingSnapshot(recorder: controller.recorder)
    case .progress:
      return ProgressSnapshot(model: controller.progress)
    case .meetingsList:
      return MeetingsListSnapshot(list: list)
    case .meetingDetail:
      if let detail, let snapshot = MeetingDetailSnapshot(detail: detail) { return snapshot }
      return JSONValue.null
    default:
      return nil
    }
  }

  /// The list publish owns the selection's side effects (first fill, a
  /// selection waiting for its row, detail model swap) and the
  /// `hasMeetings` flag, outside observation tracking.
  func willPublish(_ topic: BridgeTopic) {
    switch topic {
    case .meetingsList:
      followListFill()
      syncDetail()
    case .meetingDetail:
      syncDetail()
    default:
      break
    }
  }

  /// The app publish consumes the controller's meeting request after the
  /// snapshot has carried it once.
  func didPublish(_ topic: BridgeTopic, emitted: Bool) {
    if topic == .app { consumeMeetingRequest() }
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
      publisher.schedule(.app)
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
    publisher.schedule(.meetingDetail)
  }

  /// The old view's `onDisappear`: playback stops, a pending re-export is
  /// flushed by the model itself.
  private func tearDownDetail() {
    guard let leaving = detail else { return }
    for task in detailTasks { task.cancel() }
    detailTasks = []
    detail = nil
    leaving.viewDisappeared()
  }

  // MARK: - Commands

  func handle(_ request: BridgeRequest) async throws -> JSONValue? {
    switch request.method {
    case .pageReady:
      UITestDiagnostics.note("page ready")
      publisher.pageDidBecomeReady()
    case .pageLayout:
      // Validated and dropped: nothing reads the page's size yet.
      _ = try request.params(PageLayoutParams.self)

    case .meetingsSetFilter:
      list.stateFilter = try request.params(SetFilterParams.self).filter.modelFilter
    case .meetingsSetTagFilter:
      // `null` params clear the tag too.
      list.tagFilter = try request.optionalParams(SetTagFilterParams.self)?.tag
    case .meetingsSetQuery:
      list.query = try request.params(SetQueryParams.self).query
    case .meetingsSelect:
      let id = try request.params(MeetingIDParams.self).meetingID
      if list.all.contains(where: { $0.id == id }) {
        pendingSelection = nil
        list.selection = id
      } else {
        // A deep link ahead of its row (a recording that just started, a
        // meeting the phone is handing over): selected when the row lands.
        pendingSelection = id
      }
    case .meetingsDelete:
      let id = try request.params(MeetingIDParams.self).meetingID
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
      return try BridgeReplies.value(ConfirmReply(confirmed: confirmed))

    case .meetingSetTab:
      let tab = try request.params(SetTabParams.self).tab
      let detail = try requireDetail()
      detail.tab = tab.modelTab
    case .meetingSetTags:
      let tags = try request.params(SetTagsParams.self).tags
      let detail = try requireDetail()
      await detail.setTags(MeetingDetailViewModel.tags(from: tags))
    case .meetingSetTemplate:
      let templateID = try request.params(SetTemplateParams.self).templateID
      let detail = try requireDetail()
      await detail.setTemplate(templateID)
    case .meetingRerunSummary:
      let detail = try requireDetail()
      await detail.rerunSummary()
    case .meetingReexport:
      let detail = try requireDetail()
      await detail.reexport()
    case .meetingProcessAgain:
      try requireDetail().processAgain()
    case .meetingSetKeepAudio:
      let keep = try request.params(SetBoolParams.self).value
      let detail = try requireDetail()
      return try await setKeepAudio(keep, on: detail, confirming: !keep && detail.wouldDeleteNow)
    case .meetingDeleteRecordingNow:
      return try await setKeepAudio(false, on: try requireDetail(), confirming: true)
    case .meetingSaveNotes:
      // The page debounces typing and names the meeting, so the text is
      // written where it says, selected or not, the moment it arrives.
      try await saveNotes(try request.params(SaveNotesParams.self))
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
      let query = try request.params(SpeakerOptionsParams.self)
      let speakers = try requireDetail().speakers
      return try BridgeReplies.value(
        SpeakerOptionsReply(
          prefill: speakers.prefill(for: query.speakerID),
          options: speakers.options(for: query.speakerID, query: query.query)
            .map(SpeakerOption.init)))
    case .speakersSelect:
      let selection = try request.params(SelectSpeakerParams.self)
      let detail = try requireDetail()
      let option = try Self.option(from: selection.option, speakers: detail.speakers)
      await detail.speakers.select(option, for: selection.speakerID)
      // The page has no "picker closed" moment the host can see; one
      // re-export per change is idempotent and the model retries a refusal.
      await detail.pickerClosed()
    case .speakersPlay:
      let speakerID = try request.params(SpeakerIDParams.self).speakerID
      try requireDetail().speakers.play(speakerID)
    case .speakersStop:
      try requireDetail().speakers.stopPlayback()

    case .recordingStart:
      let mode = try request.params(StartRecordingParams.self).mode
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
      try BridgeSystemCommands.openURL(try request.params(OpenURLParams.self).url)
    case .systemOpenSystemSettings:
      let kind = try request.params(PermissionKindParams.self).kind
      controller.environment.permissions.openSystemSettings(
        for: try BridgeSystemCommands.permissionKind(kind))
    case .updatesCheck:
      controller.menuBar.checkForUpdates()
    case .windowOpen:
      let target = try request.params(WindowParams.self)
      openWindow(target)
    case .uiConfirmDestructive:
      let prompt = try request.params(ConfirmDestructiveParams.self)
      let confirmed = await confirm(prompt)
      return try BridgeReplies.value(ConfirmReply(confirmed: confirmed))

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
  /// Applies the keep flag, after the delete-recording prompt when
  /// `confirming`; the reply says whether the user went ahead.
  private func setKeepAudio(
    _ keep: Bool, on detail: MeetingDetailViewModel, confirming: Bool
  ) async throws -> JSONValue? {
    if confirming, !(await confirm(Self.deleteRecordingPrompt)) {
      return try BridgeReplies.value(ConfirmReply(confirmed: false))
    }
    await detail.setKeepAudio(keep)
    return try BridgeReplies.value(ConfirmReply(confirmed: true))
  }

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
