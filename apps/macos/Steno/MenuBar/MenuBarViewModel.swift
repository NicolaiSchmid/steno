import Foundation
import StenoAudio
import StenoCore

/// The menu bar item's state: recording over a `CaptureSession` built per
/// recording, the processing queue (pipeline `progress` events joined with
/// `observeMeetings()`), and the login item. Stopping a recording sets the
/// asset's retention from `Settings`, writes the meeting's final duration
/// and hands it to `ProcessingPipeline.enqueue`; the title and calendar
/// participants were written when the recording started.
@MainActor
@Observable
final class MenuBarViewModel {
  enum RecordingState: Equatable, Sendable {
    case idle
    case starting
    case recording(since: Date)
    case stopping
  }

  struct QueueItem: Identifiable, Equatable, Sendable {
    var meeting: Meeting
    var stage: PipelineStage?
    var id: UUID { meeting.id }
    var fraction: Double { stage?.fraction ?? 0 }
  }

  private struct Active {
    var session: CaptureSession
    var meetingID: UUID
    var mode: CaptureMode
    var settings: Settings
    var observers: [Task<Void, Never>]
  }

  private(set) var recording: RecordingState = .idle
  private(set) var levels: LaneLevels?
  private(set) var queue: [QueueItem] = []
  private(set) var recent: [Meeting] = []
  private(set) var launchAtLogin: LoginItemStatus
  private(set) var lastError: String?
  private(set) var lastWarning: String?
  /// The meeting the last `stop()` enqueued.
  private(set) var lastStoppedMeetingID: UUID?

  private let environment: AppEnvironment
  private var active: Active?
  private var stages: [UUID: PipelineStage] = [:]
  private var meetings: [Meeting] = []
  private var observers: [Task<Void, Never>] = []
  /// Called around recordings so the detector never sees Steno's own tap;
  /// set by `AppController`.
  var recordingDidChange: ((Bool) async -> Void)?

  init(environment: AppEnvironment) {
    self.environment = environment
    self.launchAtLogin = environment.loginItem.status
    observers.append(
      Task { [weak self] in
        do {
          for try await meetings in environment.store.observeMeetings() {
            guard let self else { return }
            self.meetings = meetings
            self.rebuildQueue()
          }
        } catch {
          self?.lastError = "Meeting list unavailable: \(error)"
        }
      })
    observers.append(
      Task { [weak self] in
        let stream = await environment.events.subscribe()
        for await event in stream {
          guard let self else { return }
          if case .progress(let meetingID, let stage) = event {
            self.stages[meetingID] = stage
            self.rebuildQueue()
          }
        }
      })
  }

  var isRecording: Bool {
    switch recording {
    case .idle: false
    case .starting, .recording, .stopping: true
    }
  }

  var elapsed: TimeInterval? {
    if case .recording(let since) = recording { return environment.now().timeIntervalSince(since) }
    return nil
  }

  // MARK: - Recording

  /// Starts a recording; `.call` records mic and system lanes, `.inPerson`
  /// one room lane. The meeting row is written first (`.recording`) with
  /// the calendar title and attendees, so the list shows it immediately.
  func start(mode: CaptureMode) async {
    guard recording == .idle else { return }
    recording = .starting
    lastError = nil
    lastWarning = nil
    let startedAt = environment.now()
    let meetingID = UUID()
    do {
      let settings = try await environment.settings.load()
      let configuration = CaptureConfiguration(
        mode: mode, inputDeviceUID: settings.inputDeviceUID,
        outputDirectory: settings.audioFolder)
      let session = try environment.makeCaptureSession(configuration)
      let event = await resolveCalendarEvent(at: startedAt)
      let meeting = Meeting(
        id: meetingID,
        title: event.map(\.title).flatMap { $0.isEmpty ? nil : $0 }
          ?? Self.defaultTitle(mode: mode, startedAt: startedAt),
        startedAt: startedAt,
        duration: 0,
        source: mode == .call ? .macCall : .macInPerson,
        calendarEventID: event?.id,
        state: .recording,
        templateID: settings.defaultTemplateID,
        createdAt: startedAt,
        updatedAt: startedAt)
      try await environment.store.save(meeting)
      for attendee in event?.attendees ?? [] where !attendee.isCurrentUser {
        try await environment.store.save(
          Participant(
            id: UUID(), meetingID: meetingID, personID: nil, displayName: attendee.name,
            role: .them, email: attendee.email))
      }
      await recordingDidChange?(true)
      try await session.start(meetingID: meetingID)
      var active = Active(
        session: session, meetingID: meetingID, mode: mode, settings: settings, observers: [])
      active.observers = observe(session)
      self.active = active
      recording = .recording(since: startedAt)
    } catch {
      lastError = "Recording could not start: \(error)"
      try? await environment.store.setState(
        .failed(reason: "Recording could not start: \(error)"), meetingID: meetingID,
        now: environment.now())
      recording = .idle
      await recordingDidChange?(false)
    }
  }

  /// Stops the recording, sets retention from the settings, writes the final
  /// duration and enqueues the meeting. After a device loss the session hands
  /// back the partial recording, which is enqueued like any other.
  func stop() async {
    guard let active, case .recording = recording else { return }
    recording = .stopping
    do {
      let result = try await active.session.stop()
      var asset = result.asset
      asset.retention = active.settings.defaultRetention
      asset.expiresAt = nil
      let meeting = try await environment.store.update(
        meetingID: active.meetingID, now: environment.now()
      ) { meeting in
        meeting.duration = result.statistics.duration
      }
      try await environment.pipeline.enqueue(meeting, asset: asset)
      lastStoppedMeetingID = active.meetingID
      if active.mode == .call, result.statistics.systemLaneSilent {
        lastWarning = "The system audio lane stayed silent. Check the system audio permission."
      }
      if result.statistics.endedOnDeviceLoss {
        lastWarning = "An audio device disappeared; the partial recording was kept."
      }
    } catch {
      lastError = "Recording could not be saved: \(error)"
      try? await environment.store.setState(
        .failed(reason: "Recording could not be saved: \(error)"), meetingID: active.meetingID,
        now: environment.now())
    }
    self.active = nil
    levels = nil
    recording = .idle
    await recordingDidChange?(false)
    // Last: the states observer may be the caller (device loss), and a
    // cancelled task aborts the GRDB writes above.
    for observer in active.observers { observer.cancel() }
  }

  func toggleRecording() async {
    switch recording {
    case .idle: await start(mode: .call)
    case .recording: await stop()
    case .starting, .stopping: break
    }
  }

  private func observe(_ session: CaptureSession) -> [Task<Void, Never>] {
    let states = Task { [weak self] in
      let stream = await session.states
      for await state in stream {
        guard let self else { return }
        if case .failed(let error, _) = state, case .recording = self.recording {
          self.lastError = "Recording failed: \(error.description)"
          await self.stop()
          return
        }
      }
    }
    let levels = Task { [weak self] in
      let stream = await session.levels
      for await levels in stream {
        guard let self else { return }
        self.levels = levels
      }
    }
    return [states, levels]
  }

  private func resolveCalendarEvent(at now: Date) async -> CalendarEvent? {
    guard let events = try? await environment.calendar.events(on: now) else { return nil }
    return CalendarEvent.match(in: events, now: now)
  }

  static func defaultTitle(mode: CaptureMode, startedAt: Date) -> String {
    let formatter = DateFormatter()
    formatter.dateStyle = .medium
    formatter.timeStyle = .short
    let kind = mode == .call ? "Call" : "Meeting"
    return "\(kind) \(formatter.string(from: startedAt))"
  }

  // MARK: - Queue

  private func rebuildQueue() {
    let live = meetings.filter { $0.state == .queued || $0.state == .processing }
    queue =
      live
      .map { QueueItem(meeting: $0, stage: stages[$0.id]) }
      .sorted { $0.meeting.startedAt < $1.meeting.startedAt }
    for id in Array(stages.keys) where !live.contains(where: { $0.id == id }) {
      stages[id] = nil
    }
    recent = Array(
      meetings
        .filter { $0.state == .ready || $0.state.isFailed }
        .sorted { $0.startedAt > $1.startedAt }
        .prefix(5))
  }

  // MARK: - Login item

  func setLaunchAtLogin(_ enabled: Bool) async {
    do {
      try environment.loginItem.setEnabled(enabled)
      try await environment.updateSettings { $0.launchAtLogin = enabled }
    } catch {
      lastError = "Login item could not be changed: \(error)"
    }
    launchAtLogin = environment.loginItem.status
  }

  func refreshLoginItem() {
    launchAtLogin = environment.loginItem.status
  }

  func openLoginItemSettings() {
    environment.loginItem.openSystemSettings()
  }

  func checkForUpdates() {
    environment.updater.checkForUpdates()
  }

  func clearMessages() {
    lastError = nil
    lastWarning = nil
  }

  /// The Debug menu's system audio probe result.
  func noteProbe(granted: Bool) {
    lastWarning =
      granted
      ? "System audio probe: the tap carried signal (permission granted)."
      : "System audio probe: the tap stayed silent (permission missing or denied)."
  }
}
