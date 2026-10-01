import AppKit
import Foundation
import StenoAudio
import StenoCore

/// The running app's object graph over one `AppEnvironment`: the recorder,
/// the detection controller, the menu bar view model, the processing
/// progress model, pending speaker reviews, the retention sweep after
/// processed meetings, the handover listener when phones are paired, the
/// first-launch login item registration, the recorder's permission report,
/// refreshed whenever the app becomes active (the user comes back from
/// System Settings), and the environment's pipeline warm-up when a
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
  /// Meetings the pipeline flagged with unconfirmed speakers. The set
  /// clears itself from the store, see `reviewChanged(_:speakers:)`.
  private(set) var pendingReviews: Set<UUID> = []
  /// One `observeMeeting(id:)` subscription per pending review, ended when
  /// the review clears. `observeMeetings` tracks the meeting table only, so
  /// a speaker confirmed through `MeetingStore.confirm` (a speaker-table
  /// write) would not reach `meetingsChanged(_:)`; the per-meeting export
  /// observation includes the speakers and fires on every such write.
  private var reviewObservers: [UUID: Task<Void, Never>] = [:]
  /// The ids of the last `observeMeetings()` emission, empty before the
  /// first. `meetingsChanged(_:)` keeps it, so a caller can tell that the
  /// list has been seen once (the subscription is the last thing `launch()`
  /// starts and its first list lands on a later turn).
  private(set) var listedMeetingIDs: Set<UUID> = []
  /// The meeting the main window should show next (from the menu bar or the
  /// detection prompt).
  var requestedMeetingID: UUID?
  /// The Settings section to select next, from the setup banner, the detail
  /// rows and the footer (`openSettings(_:)`); the Settings page applies and
  /// clears it, as `MainWindow` does for `requestedMeetingID`.
  var requestedSettingsSection: SettingsSection?
  /// "Not now" on the setup banner hides it for the rest of this launch; it
  /// comes back on the next launch while the configuration is still missing.
  private(set) var setupBannerDismissed = false
  /// The stored settings as last emitted by `environment.settings.observe()`,
  /// nil until the first emission after `launch()`. The setup banner reads
  /// `setupBannerMessage` from it and a new detail model seeds its configured
  /// flags from it, so neither shows a wrong frame before its own
  /// observation lands.
  private(set) var storedSettings: Settings?
  private(set) var launched = false
  private var observers: [Task<Void, Never>] = []
  private var activationObserver: (any NSObjectProtocol)?

  static let loginItemRegisteredKey = "steno.loginItemRegistered"

  private let defaults: UserDefaults

  init(environment: AppEnvironment, defaults: UserDefaults = .standard) {
    self.environment = environment
    self.defaults = defaults
    self.recorder = RecordingController(environment: environment)
    self.menuBar = MenuBarViewModel(environment: environment)
    self.progress = ProcessingProgressModel(now: environment.now)
    self.detection = DetectionController(environment: environment)
    detection.startRecording = { [weak self] callApp in
      await self?.recorder.start(mode: .call, callApp: callApp)
    }
    detection.microphoneActivity = { [weak self] event in
      await self?.recorder.microphoneActivity(event)
    }
    recorder.recordingDidChange = { [weak self] recording in
      guard let self else { return }
      // In the background: the recording must not wait for a model load.
      if recording { Task { [environment] in await environment.warmUpPipelineIfModelsInstalled() } }
      await self.detection.recordingDidChange(recording)
    }
    activationObserver = NotificationCenter.default.addObserver(
      forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main
    ) { [weak self] _ in
      Task { @MainActor in await self?.recorder.refreshPermissions() }
    }
  }

  /// Everything that happens once at launch, in order: the pipeline's
  /// events are subscribed, interrupted recordings become failed, meetings
  /// left queued or processing are processed again, the retention sweep
  /// runs, the login item is registered the first time (when the setting
  /// says so), the detector starts, the handover listener starts when a
  /// phone is already paired, and the meeting list is observed. The two
  /// event subscriptions, the progress model's and this controller's, come
  /// first so the first events of resumed runs reach both.
  func launch() async {
    guard !launched else { return }
    launched = true
    let progressEvents = await environment.events.subscribe()
    observers.append(
      Task { [progress, environment] in
        await progress.observe(
          events: progressEvents, meetings: environment.store.observeMeetings())
      })
    let events = await environment.events.subscribe()
    observers.append(
      Task { [weak self, environment] in
        for await event in events {
          guard let self else { return }
          switch event {
          case .speakersNeedReview(let meetingID, _):
            self.reviewRequested(meetingID)
          case .retentionApplied:
            // The stage has written `expiresAt`; the `.ready` row change
            // came earlier, before deliver and retention ran, so it is not
            // the trigger.
            await environment.runRetentionSweep()
          case .deleted(let meetingID):
            self.clearReview(meetingID)
          case .progress:
            break
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
          for try await settings in environment.settings.observe() {
            guard let self else { return }
            self.storedSettings = settings
          }
        } catch {
          // Settings and the detail pane report store errors; the banner
          // just stays hidden.
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

  /// A pending review for a meeting the store no longer lists is dropped,
  /// so a badge never points at nothing; one whose speakers are all
  /// confirmed by now is dropped too, so a badge never asks for a review
  /// that has nothing left to review.
  private func meetingsChanged(_ meetings: [Meeting]) async {
    let listed = Set(meetings.map(\.id))
    listedMeetingIDs = listed
    for meetingID in pendingReviews where !listed.contains(meetingID) {
      clearReview(meetingID)
    }
    for meetingID in pendingReviews {
      guard let speakers = try? await environment.store.speakers(meetingID: meetingID) else {
        continue
      }
      reviewChanged(meetingID, speakers: speakers)
    }
  }

  /// Marks the meeting pending and starts following its export, so the
  /// review clears as soon as the last speaker is confirmed, from any
  /// caller of `MeetingStore.confirm`. A nil export (a meeting the store
  /// never had, or one deleted meanwhile) is left to `meetingsChanged(_:)`
  /// and the `.deleted` event, which own that rule.
  private func reviewRequested(_ meetingID: UUID) {
    pendingReviews.insert(meetingID)
    guard reviewObservers[meetingID] == nil else { return }
    reviewObservers[meetingID] = Task { [weak self, environment] in
      do {
        for try await export in environment.store.observeMeeting(id: meetingID) {
          guard let self else { return }
          guard let export else { continue }
          self.reviewChanged(meetingID, speakers: export.speakers)
        }
      } catch {
        // The detail view reports store errors; nothing to do here.
      }
    }
  }

  /// The one rule: a review is pending while any speaker of the meeting is
  /// not `.confirmed`. No speakers at all is nothing to review.
  private func reviewChanged(_ meetingID: UUID, speakers: [Speaker]) {
    guard pendingReviews.contains(meetingID) else { return }
    if speakers.allSatisfy(\.assignment.isConfirmed) {
      clearReview(meetingID)
    }
  }

  private func clearReview(_ meetingID: UUID) {
    pendingReviews.remove(meetingID)
    reviewObservers.removeValue(forKey: meetingID)?.cancel()
  }

  /// The sidebar control's start: the recorder starts as it does from the
  /// menu bar, then the live row is requested so the window selects it.
  /// Starts from the menu bar or the detection prompt call `recorder.start`
  /// and never move the selection. A start that fails re-reads the
  /// permissions: a denial born at the first TCC prompt disables the control
  /// at once, without waiting for the app to become active again.
  func startRecordingFromWindow(mode: CaptureMode) async {
    await recorder.start(mode: mode)
    switch recorder.recording {
    case .recording:
      requestedMeetingID = recorder.activeMeetingID
    case .idle:
      await recorder.refreshPermissions()
    case .starting, .stopping:
      break
    }
  }

  /// Deep link into Settings: callers set the request here, then call the
  /// scene's `openWindow(id: "settings")` and activate the app; the page
  /// selects the section from the `app` snapshot (the setup banner, the
  /// detail rows and the footer take this path).
  @discardableResult
  func openSettings(_ section: SettingsSection) -> SettingsSection {
    requestedSettingsSection = section
    return section
  }

  /// What the setup banner says; nil before the first settings emission and
  /// once both the endpoint and the vault are configured. Follows the store,
  /// so the banner disappears as soon as Settings saves the missing piece.
  var setupBannerMessage: SetupBannerMessage? {
    storedSettings.flatMap { SetupBannerMessage(settings: $0) }
  }

  /// "Not now" on the setup banner.
  func dismissSetupBanner() {
    setupBannerDismissed = true
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
  /// `.recording` (or fail) first, then stopped with `.quit` as its reason
  /// and enqueued like any other; then the detector, the handover listener
  /// and every observation end.
  func shutdown() async {
    await recorder.awaitSettled()
    if case .recording = recorder.recording { await recorder.stop(reason: .quit) }
    await detection.stop()
    if let handover = environment.handover { await handover.stop() }
    for observer in observers { observer.cancel() }
    observers = []
    for observer in reviewObservers.values { observer.cancel() }
    reviewObservers = [:]
    if let activationObserver { NotificationCenter.default.removeObserver(activationObserver) }
    activationObserver = nil
  }
}
