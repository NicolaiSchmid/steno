import Foundation
import StenoAudio
import StenoCore

/// The running app's object graph over one `AppEnvironment`: the recorder,
/// the detection controller, the menu bar view model, pending speaker
/// reviews, the retention sweep after processed meetings, the handover
/// listener when phones are paired, and the first-launch login item
/// registration.
@MainActor
@Observable
final class AppController {
  let environment: AppEnvironment
  let recorder: RecordingController
  let menuBar: MenuBarViewModel
  let detection: DetectionController
  /// Meetings the pipeline flagged with unconfirmed speakers.
  private(set) var pendingReviews: Set<UUID> = []
  /// The meeting the main window should show next (from the menu bar or the
  /// detection prompt).
  var requestedMeetingID: UUID?
  private(set) var launched = false
  private var observers: [Task<Void, Never>] = []
  private var readyMeetings: Set<UUID> = []

  static let loginItemRegisteredKey = "steno.loginItemRegistered"

  private let defaults: UserDefaults

  init(environment: AppEnvironment, defaults: UserDefaults = .standard) {
    self.environment = environment
    self.defaults = defaults
    self.recorder = RecordingController(environment: environment)
    self.menuBar = MenuBarViewModel(environment: environment)
    self.detection = DetectionController(environment: environment)
    detection.startRecording = { [weak self] in await self?.recorder.start(mode: .call) }
    recorder.recordingDidChange = { [weak self] recording in
      await self?.detection.recordingDidChange(recording)
    }
  }

  /// Everything that happens once at launch, in order: interrupted
  /// recordings become failed, the retention sweep runs, the login item is
  /// registered the first time (when the setting says so), the detector
  /// starts, the handover listener starts when a phone is already paired,
  /// and the pipeline's events are observed.
  func launch() async {
    guard !launched else { return }
    launched = true
    await environment.reconcileInterruptedRecordings()
    await environment.runRetentionSweep()
    await registerLoginItemOnFirstLaunch()
    await detection.applySettings()
    await startHandoverIfPaired()
    observers.append(Task { [menuBar] in await menuBar.observe() })
    observers.append(Task { [menuBar] in await menuBar.observeProgress() })
    observers.append(
      Task { [weak self, environment] in
        let stream = await environment.events.subscribe()
        for await event in stream {
          guard let self else { return }
          if case .speakersNeedReview(let meetingID, _) = event {
            self.pendingReviews.insert(meetingID)
          }
        }
      })
    observers.append(
      Task { [weak self, environment] in
        do {
          for try await meetings in environment.store.observeMeetings() {
            guard let self else { return }
            await self.meetingsChanged(meetings)
          }
        } catch {
          // The list view reports store errors; nothing to do here.
        }
      })
  }

  /// A meeting that just reached `.ready` or `.failed` triggers the sweep
  /// (its retention stage set `expiresAt`).
  private func meetingsChanged(_ meetings: [Meeting]) async {
    let finished = Set(meetings.filter { $0.state == .ready || $0.state.isFailed }.map(\.id))
    let newlyFinished = finished.subtracting(readyMeetings)
    readyMeetings = finished
    if !newlyFinished.isEmpty {
      await environment.runRetentionSweep()
    }
    pendingReviews.formIntersection(meetings.map(\.id))
  }

  func reviewCompleted(meetingID: UUID) {
    pendingReviews.remove(meetingID)
  }

  private func registerLoginItemOnFirstLaunch() async {
    guard !environment.isPreview, !defaults.bool(forKey: Self.loginItemRegisteredKey) else {
      return
    }
    guard let settings = try? await environment.settings.load(), settings.launchAtLogin else {
      return
    }
    defaults.set(true, forKey: Self.loginItemRegisteredKey)
    if environment.loginItem.status == .notRegistered {
      try? environment.loginItem.setEnabled(true)
    }
    menuBar.refreshLoginItem()
  }

  /// Advertising triggers the local network prompt, so the listener only
  /// starts on its own when a phone is already paired; the Phones settings
  /// start it for the first pairing.
  private func startHandoverIfPaired() async {
    guard let handover = environment.handover else { return }
    guard let devices = try? await handover.pairedDevices(), !devices.isEmpty else { return }
    try? await handover.start()
  }

  /// Quit: a recording that is still starting is allowed to reach
  /// `.recording` (or fail) first, then stopped and enqueued like any other;
  /// then the detector, the handover listener and every observation end.
  func shutdown() async {
    await recorder.awaitSettled()
    if case .recording = recorder.recording { await recorder.stop() }
    await detection.stop()
    if let handover = environment.handover { await handover.stop() }
    for observer in observers { observer.cancel() }
    observers = []
  }
}
