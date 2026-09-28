import Foundation
import StenoAudio
import StenoCore
import StenoSpeech
import os

/// The running app's object graph over one `AppEnvironment`: the recorder,
/// the detection controller, the menu bar view model, the processing
/// progress model, pending speaker reviews, the retention sweep after
/// processed meetings, the handover listener when phones are paired, the
/// first-launch login item registration, and the pipeline warm-up when a
/// recording starts.
@MainActor
@Observable
final class AppController {
  let environment: AppEnvironment
  let recorder: RecordingController
  let menuBar: MenuBarViewModel
  /// Where the pipeline is with every queued or processing meeting; the
  /// menu bar row, the list entry and the detail view read it.
  let progress: ProcessingProgressModel
  let detection: DetectionController
  /// Meetings the pipeline flagged with unconfirmed speakers.
  private(set) var pendingReviews: Set<UUID> = []
  /// The meeting the main window should show next (from the menu bar or the
  /// detection prompt).
  var requestedMeetingID: UUID?
  private(set) var launched = false
  private var observers: [Task<Void, Never>] = []

  static let loginItemRegisteredKey = "steno.loginItemRegistered"

  private static let logger = Logger(subsystem: "uno.schmid.steno.mac", category: "pipeline")

  private let defaults: UserDefaults

  init(environment: AppEnvironment, defaults: UserDefaults = .standard) {
    self.environment = environment
    self.defaults = defaults
    self.recorder = RecordingController(environment: environment)
    self.menuBar = MenuBarViewModel(environment: environment)
    self.progress = ProcessingProgressModel(now: environment.now)
    self.detection = DetectionController(environment: environment)
    detection.startRecording = { [weak self] in await self?.recorder.start(mode: .call) }
    recorder.recordingDidChange = { [weak self] recording in
      guard let self else { return }
      if recording { self.warmUpPipeline() }
      await self.detection.recordingDidChange(recording)
    }
  }

  /// Loads the speech engine and the diarizer while the recording runs, so
  /// the cold model load is over before the meeting ends and never sits in
  /// the wait the owner watches. In the background: the recording must not
  /// wait for a CoreML compile. Only when both models are on disk, because a
  /// `prepare()` may download and nothing downloads during a call; the guard
  /// reads the engine from the setting, not from the pipeline's engine, so
  /// the preview's `FakeSpeechEngine` is guarded by the `parakeet-v3` marker
  /// files like the real one. A failure is logged and swallowed; the run's
  /// own `prepare()` reports it. A `reloadPipeline()` during the recording
  /// yields a cold replacement, which is accepted.
  private func warmUpPipeline() {
    Task { [environment] in
      guard let settings = try? await environment.settings.load(),
        let engine = try? SpeechEngineID(settingsValue: settings.speechEngineID),
        environment.models.isInstalled(engine.asset),
        environment.models.isInstalled(.offlineDiarizer)
      else { return }
      do {
        try await environment.pipeline.warmUp()
      } catch {
        Self.logger.error(
          "Pipeline warm-up failed: \(String(describing: error), privacy: .public)")
      }
    }
  }

  /// Everything that happens once at launch, in order: the pipeline's
  /// events are subscribed, interrupted recordings become failed, meetings
  /// left queued or processing are processed again, the retention sweep
  /// runs, the login item is registered the first time (when the setting
  /// says so), the detector starts, the handover listener starts when a
  /// phone is already paired, and the meeting list is observed. The event
  /// subscription comes first so the first events of resumed runs reach
  /// the progress model.
  func launch() async {
    guard !launched else { return }
    launched = true
    let events = await environment.events.subscribe()
    observers.append(
      Task { [weak self, environment] in
        for await event in events {
          guard let self else { return }
          switch event {
          case .speakersNeedReview(let meetingID, _):
            self.pendingReviews.insert(meetingID)
          case .retentionApplied:
            // The stage has written `expiresAt`; the `.ready` row change
            // came earlier, before deliver and retention ran, so it is not
            // the trigger.
            await environment.runRetentionSweep()
          case .deleted(let meetingID):
            self.pendingReviews.remove(meetingID)
            self.progress.apply(event)
          case .progress:
            self.progress.apply(event)
          }
        }
      })
    await environment.reconcileInterruptedRecordings()
    await environment.resumeUnfinishedProcessing()
    await environment.runRetentionSweep()
    await registerLoginItemOnFirstLaunch()
    await detection.applySettings()
    await startHandoverIfPaired()
    observers.append(Task { [menuBar] in await menuBar.observe() })
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

  /// A pending review for a meeting the store no longer lists is dropped,
  /// so a badge never points at nothing; the progress model gains an entry
  /// for every queued or processing meeting and loses the others.
  private func meetingsChanged(_ meetings: [Meeting]) async {
    pendingReviews.formIntersection(meetings.map(\.id))
    progress.meetingsChanged(meetings)
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
