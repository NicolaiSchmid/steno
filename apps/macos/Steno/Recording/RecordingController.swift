import Foundation
import StenoAudio
import StenoCore

/// The recorder's state as the menu bar item, the Record menu and the
/// detection prompt see it.
enum RecordingState: Equatable, Sendable {
  case idle
  case starting
  case recording(since: Date)
  case stopping

  var label: String {
    switch self {
    case .idle: "Not recording"
    case .starting: "Starting…"
    case .recording: "Recording"
    case .stopping: "Finishing…"
    }
  }
}

/// The one recorder in the app, owned by `AppController` like
/// `DetectionController`: a `CaptureSession` per recording over
/// `AppEnvironment.makeCaptureSession`, the state machine, levels, the
/// calendar lookup and the messages. The menu bar, the Record menu, the
/// detection prompt and `shutdown()` all drive this, none of them each
/// other. The meeting rows are core's business: `LocalRecordingIntake`
/// writes the `.recording` row with the calendar title and attendees at
/// `begin`, sets retention from `Settings` as they are at `complete`,
/// writes the duration and enqueues; the app carries no copy of that
/// transaction.
@MainActor
@Observable
final class RecordingController {
  private struct Active {
    var session: CaptureSession
    var meetingID: UUID
    var mode: CaptureMode
    var observers: [Task<Void, Never>]
  }

  private(set) var recording: RecordingState = .idle {
    didSet {
      guard recording != .starting, recording != .stopping else { return }
      let waiters = settledWaiters
      settledWaiters = []
      for waiter in waiters { waiter.resume() }
    }
  }
  private(set) var levels: LaneLevels?
  private(set) var lastError: String?
  private(set) var lastWarning: String?

  private let environment: AppEnvironment
  private var active: Active?
  private var settledWaiters: [CheckedContinuation<Void, Never>] = []
  /// Called around recordings; `AppController` points it at the detection
  /// controller so an open prompt closes when a recording starts.
  var recordingDidChange: ((Bool) async -> Void)?

  init(environment: AppEnvironment) {
    self.environment = environment
  }

  var isRecording: Bool {
    switch recording {
    case .idle: false
    case .starting, .recording, .stopping: true
    }
  }

  /// The status line for the state.
  var statusText: String { recording.label }

  var elapsed: TimeInterval? {
    if case .recording(let since) = recording { return environment.now().timeIntervalSince(since) }
    return nil
  }

  // MARK: - Start and stop

  /// Starts a recording; `.call` records mic and system lanes, `.inPerson`
  /// one room lane. The meeting row is written first (`.recording`) with
  /// the calendar title and attendees, so the list shows it immediately.
  func start(mode: CaptureMode) async {
    guard recording == .idle else { return }
    recording = .starting
    lastError = nil
    lastWarning = nil
    let startedAt = environment.now()
    let intake = environment.makeLocalIntake()
    var meetingID: UUID?
    do {
      let settings = try await environment.settings.load()
      let configuration = CaptureConfiguration(
        mode: mode, inputDeviceUID: settings.inputDeviceUID,
        outputDirectory: settings.audioFolder)
      let session = try environment.makeCaptureSession(configuration)
      let event = await resolveCalendarEvent(at: startedAt)
      let meeting = try await intake.begin(
        source: mode == .call ? .macCall : .macInPerson,
        title: event?.title,
        calendarEventID: event?.id,
        attendees: (event?.attendees ?? [])
          .filter { !$0.isCurrentUser }
          .map { LocalRecordingIntake.Attendee(displayName: $0.name, email: $0.email) },
        startedAt: startedAt)
      meetingID = meeting.id
      await recordingDidChange?(true)
      try await session.start(meetingID: meeting.id)
      var active = Active(session: session, meetingID: meeting.id, mode: mode, observers: [])
      active.observers = observe(session)
      self.active = active
      recording = .recording(since: startedAt)
    } catch {
      lastError = "Recording could not start: \(error)"
      if let meetingID {
        try? await intake.fail(meetingID: meetingID, reason: "Recording could not start: \(error)")
      }
      recording = .idle
      await recordingDidChange?(false)
    }
  }

  /// Stops the recording and hands it to the intake, which sets retention
  /// from the settings as they are now (a change made during the recording
  /// applies to it), writes the final duration and enqueues the meeting.
  /// After a device loss the session hands back the partial recording,
  /// which is enqueued like any other.
  func stop() async {
    guard let active, case .recording = recording else { return }
    recording = .stopping
    do {
      let result = try await active.session.stop()
      try await environment.makeLocalIntake().complete(
        meetingID: active.meetingID,
        result: RecordingResult(asset: result.asset, duration: result.statistics.duration))
      if active.mode == .call, result.statistics.systemLaneSilent {
        lastWarning = "The system audio lane stayed silent. Check the system audio permission."
      }
      if result.statistics.endedOnDeviceLoss {
        lastWarning = "An audio device disappeared; the partial recording was kept."
      }
    } catch {
      // `complete` already marked the row failed when the save or the
      // enqueue threw; a session that could not stop is marked here.
      lastError = "Recording could not be saved: \(error)"
      try? await environment.makeLocalIntake().fail(
        meetingID: active.meetingID, reason: "Recording could not be saved: \(error)")
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

  /// Returns once no `start()` or `stop()` is in flight, so a quit during
  /// the start window can finish the capture instead of leaving its threads
  /// running and the master unfinalised.
  func awaitSettled() async {
    guard recording == .starting || recording == .stopping else { return }
    await withCheckedContinuation { settledWaiters.append($0) }
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

  // MARK: - Messages

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
