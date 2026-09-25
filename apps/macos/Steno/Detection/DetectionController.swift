import AppKit
import Foundation
import StenoAudio
import StenoCore

/// Subscribes to `MeetingDetector.events`, resolves the bundle id to an app
/// name and shows one prompt at a time. Suppressed while Steno records and
/// when `Settings.meetingDetectionEnabled` is off; the detector itself is
/// stopped while recording so it never reports Steno's own tap.
@MainActor
@Observable
final class DetectionController {
  private(set) var prompt: DetectionPromptViewModel? {
    didSet { promptDidChange?(prompt) }
  }
  /// The panel presenter follows the prompt through this.
  var promptDidChange: ((DetectionPromptViewModel?) -> Void)?
  private(set) var enabled = false
  private(set) var isRecording = false
  private let environment: AppEnvironment
  private var eventsTask: Task<Void, Never>?
  private var settingsTask: Task<Void, Never>?
  /// Set by `AppController`: starts a `.call` recording.
  var startRecording: (() async -> Void)?
  /// Resolves a bundle id to a display name; the live one asks NSWorkspace.
  var appName: @MainActor (String?) -> String = DetectionController.liveAppName

  init(environment: AppEnvironment) {
    self.environment = environment
  }

  /// Reads the setting and follows it from then on.
  func applySettings() async {
    if settingsTask == nil {
      settingsTask = Task { [weak self, environment] in
        do {
          for try await settings in environment.settings.observe() {
            guard let self else { return }
            await self.setEnabled(settings.meetingDetectionEnabled)
          }
        } catch {
          // Settings unreadable: leave detection as it is.
        }
      }
    }
    if let settings = try? await environment.settings.load() {
      await setEnabled(settings.meetingDetectionEnabled)
    }
  }

  func setEnabled(_ enabled: Bool) async {
    guard enabled != self.enabled else { return }
    self.enabled = enabled
    if enabled {
      if !isRecording { await startDetector() }
    } else {
      await stopDetector()
      await prompt?.dismiss()
    }
  }

  func recordingDidChange(_ recording: Bool) async {
    isRecording = recording
    if recording {
      await prompt?.dismiss()
      await stopDetector()
    } else if enabled {
      await startDetector()
    }
  }

  /// The decision the tests pin: an opened microphone shows a prompt only
  /// when detection is on, nothing is recording and no prompt is up; a
  /// released microphone dismisses the prompt.
  func handle(_ event: MeetingDetector.Event) async {
    switch event {
    case .microphoneOpened(let bundleID, _):
      guard enabled, !isRecording, prompt == nil else { return }
      let prompt = DetectionPromptViewModel(
        trigger: .microphoneOpened(bundleID: bundleID, appName: appName(bundleID)),
        clock: environment.clock)
      prompt.onClose = { [weak self] outcome in
        guard let self else { return }
        self.prompt = nil
        if outcome == .started { await self.startRecording?() }
      }
      self.prompt = prompt
      prompt.begin()
    case .microphoneReleased:
      await prompt?.dismiss()
    }
  }

  private func startDetector() async {
    guard eventsTask == nil else { return }
    do {
      try await environment.detector.start()
    } catch {
      return
    }
    eventsTask = Task { [weak self, environment] in
      let stream = await environment.detector.events
      for await event in stream {
        guard let self else { return }
        await self.handle(event)
      }
    }
  }

  private func stopDetector() async {
    eventsTask?.cancel()
    eventsTask = nil
    await environment.detector.stop()
  }

  func stop() async {
    await stopDetector()
    settingsTask?.cancel()
    settingsTask = nil
  }

  static func liveAppName(_ bundleID: String?) -> String {
    guard let bundleID else { return "Another app" }
    if let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID),
      let name = Bundle(url: url)?.object(forInfoDictionaryKey: "CFBundleName") as? String
    {
      return name
    }
    return bundleID
  }
}
