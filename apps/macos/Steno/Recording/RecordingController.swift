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
/// calendar lookup, the messages and the auto-stop after a call ends. The
/// menu bar, the Record menu, the detection prompt and `shutdown()` all
/// drive this, none of them each other. The meeting rows are core's
/// business: `LocalRecordingIntake` writes the `.recording` row with the
/// calendar title and attendees at `begin`, sets retention from `Settings`
/// as they are at `complete`, writes the duration and the end reason and
/// enqueues; the app carries no copy of that transaction.
@MainActor
@Observable
final class RecordingController {
  private struct Active {
    var session: CaptureSession
    var meetingID: UUID
    var mode: CaptureMode
    var observers: [Task<Void, Never>]
  }

  /// Foreign microphone activity while Steno records, forwarded by
  /// `DetectionController` from the detector's events with the bundle id
  /// already resolved to a name (nil when it could not be).
  enum MicrophoneActivity: Equatable, Sendable {
    case opened(appName: String?)
    case released
  }

  /// How long a `.call` recording runs on after the call app closed the
  /// microphone before it stops on its own. Long enough for a Meet or Zoom
  /// reconnect; "Keep recording" cancels it.
  static let autoStopGrace: Duration = .seconds(90)

  private(set) var recording: RecordingState = .idle {
    didSet {
      guard recording != .starting, recording != .stopping else { return }
      let waiters = settledWaiters
      settledWaiters = []
      for waiter in waiters { waiter.resume() }
    }
  }
  private(set) var levels: LaneLevels?
  /// The capture statistics of the last recording that stopped: duration,
  /// dropped frames per lane, whether the system lane stayed silent.
  private(set) var lastStatistics: CaptureStatistics?
  private(set) var lastError: String?
  private(set) var lastWarning: String?
  /// The required permissions the last `refreshPermissions()` found
  /// `.denied`, in `PermissionKind.allCases` order. A report, not a guard:
  /// `start(mode:)` does not read it.
  private(set) var deniedPermissions: [PermissionKind] = []
  /// The armed auto-stop while a `.call` recording waits out the grace
  /// after the call app released the microphone; nil otherwise.
  private(set) var autoStop: AutoStop?

  private let environment: AppEnvironment
  private var active: Active?
  private var settledWaiters: [CheckedContinuation<Void, Never>] = []
  /// A foreign process held the microphone during this recording (the
  /// prompt's app at start, or the first `.opened` seen while recording);
  /// only then does a release mean a call ended.
  private var sawForeignMicrophone = false
  private var callAppName: String?
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

  /// The meeting row of the live recording: set from `.recording` until the
  /// stop has handed the row over, nil when idle or while starting.
  var activeMeetingID: UUID? { active?.meetingID }

  /// The live recording's capture mode, nil when idle or while starting;
  /// the main window's `recording` snapshot reads it.
  var activeMode: CaptureMode? { active?.mode }

  /// The call app the recording is attributed to: the one the detection
  /// prompt named at start, or the last foreign microphone opener seen
  /// while recording. Nil for in-person recordings and unnamed apps.
  var activeCallApp: String? { callAppName }

  var elapsed: TimeInterval? {
    if case .recording(let since) = recording { return environment.now().timeIntervalSince(since) }
    return nil
  }

  // MARK: - Start and stop

  /// Starts a recording; `.call` records mic and system lanes, `.inPerson`
  /// one room lane. The meeting row is written first (`.recording`) with
  /// the calendar title and attendees, so the list shows it immediately.
  /// `callApp` is the app the detection prompt named; a recording started
  /// from it arms the auto-stop on the first release without waiting for
  /// another `.opened`.
  func start(mode: CaptureMode, callApp: String? = nil) async {
    guard recording == .idle else { return }
    recording = .starting
    lastError = nil
    lastWarning = nil
    sawForeignMicrophone = callApp != nil
    callAppName = callApp
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
      // Subscribed before `start`: `notices` carries changes from the moment
      // of subscription only, and a device that changes in the first
      // milliseconds of a recording would otherwise go unreported. The
      // observers start after it, so their `.recording` guards never see
      // `.starting`.
      let states = await session.states
      let levels = await session.levels
      let notices = await session.notices
      try await session.start(meetingID: meeting.id)
      var active = Active(session: session, meetingID: meeting.id, mode: mode, observers: [])
      active.observers = observe(states: states, levels: levels, notices: notices)
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
  /// applies to it), writes the final duration and `reason` and enqueues the
  /// meeting. After a failure the session hands back the partial recording,
  /// which is enqueued like any other with the reason the states observer
  /// passes. Any stop disarms the auto-stop and forgets the call app.
  func stop(reason: RecordingEndReason = .manual) async {
    guard let active, case .recording = recording else { return }
    recording = .stopping
    disarmAutoStop()
    sawForeignMicrophone = false
    callAppName = nil
    do {
      let result = try await active.session.stop()
      lastStatistics = result.statistics
      try await environment.makeLocalIntake().complete(
        meetingID: active.meetingID,
        result: RecordingResult(
          asset: result.asset, duration: result.statistics.duration, endReason: reason))
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

  /// The session's `states` (a failure stops and stores `.deviceLost` or
  /// `.failed`), its `levels`, and its `notices`: a device change is a
  /// warning line while the recording continues, replaced by the resumed
  /// line or, after the last failed restart, by the failure the states
  /// observer reports.
  private func observe(
    states: AsyncStream<CaptureState>, levels: AsyncStream<LaneLevels>,
    notices: AsyncStream<CaptureNotice>
  ) -> [Task<Void, Never>] {
    let statesTask = Task { [weak self] in
      for await state in states {
        guard let self else { return }
        if case .failed(let error, _) = state, case .recording = self.recording {
          self.lastError = "Recording failed: \(error.description)"
          await self.stop(reason: error == .deviceLost ? .deviceLost : .failed)
          return
        }
      }
    }
    let levelsTask = Task { [weak self] in
      for await levels in levels {
        guard let self else { return }
        self.levels = levels
      }
    }
    let noticesTask = Task { [weak self] in
      for await notice in notices {
        guard let self, case .recording = self.recording else { return }
        switch notice {
        case .deviceChanged:
          self.lastWarning = "Audio devices changed. Reconnecting…"
        case .deviceResumed:
          self.lastWarning = "Audio devices changed. Recording continues."
        }
      }
    }
    return [statesTask, levelsTask, noticesTask]
  }

  // MARK: - Auto-stop after the call ends

  /// The policy: a `.call` recording during which a foreign process held the
  /// microphone arms the grace countdown when that microphone is released;
  /// a microphone opened again cancels it. In-person recordings and calls
  /// nobody else ever joined never arm. An `.opened` is remembered from
  /// `.starting` on: the detector forwards from the moment the start is
  /// announced, and a call joined while Steno is still starting must arm on
  /// its release like any other. The mode is checked at `.released`, the
  /// only place the memory has an effect.
  func microphoneActivity(_ event: MicrophoneActivity) async {
    switch event {
    case .opened(let appName):
      switch recording {
      case .idle, .stopping: return
      case .starting, .recording: break
      }
      sawForeignMicrophone = true
      callAppName = appName
      disarmAutoStop()
    case .released:
      guard let active, case .recording = recording, active.mode == .call, sawForeignMicrophone,
        autoStop == nil
      else { return }
      armAutoStop()
    }
  }

  /// "Keep recording": the countdown goes and does not come back until the
  /// next call is observed (`.opened` then `.released`). A click that lands
  /// after the row is gone (the call app reopened the microphone, a double
  /// click) changes nothing.
  func keepRecording() {
    guard autoStop != nil else { return }
    disarmAutoStop()
    sawForeignMicrophone = false
  }

  private func armAutoStop() {
    let appName = callAppName
    let countdown = Countdown(duration: Self.autoStopGrace, clock: environment.clock) {
      [weak self] in
      guard let self, self.autoStop != nil else { return }
      await self.stop(reason: .callEnded(appName: appName))
    }
    autoStop = AutoStop(appName: appName, countdown: countdown)
    countdown.begin()
  }

  private func disarmAutoStop() {
    autoStop?.countdown.cancel()
    autoStop = nil
  }

  private func resolveCalendarEvent(at now: Date) async -> CalendarEvent? {
    guard let events = try? await environment.calendar.events(on: now) else { return nil }
    return CalendarEvent.match(in: events, now: now)
  }

  // MARK: - Permissions

  /// Re-reads the required permissions and records the denied ones. Only
  /// `.denied` counts: `.unknown` means macOS has not asked yet and the
  /// first recording will.
  func refreshPermissions() async {
    var denied: [PermissionKind] = []
    for kind in PermissionKind.allCases where kind.isRequired {
      if await environment.permissions.state(of: kind) == .denied {
        denied.append(kind)
      }
    }
    deniedPermissions = denied
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
