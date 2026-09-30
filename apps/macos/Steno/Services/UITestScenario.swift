import Foundation

/// The launch arguments the UI smoke tests pass, parsed once. Every flag
/// starts with `-steno-`; an unknown one is reported, so a typo in the smoke
/// suite is not a silent no-op. A flag that takes a value carries it in the
/// same argument, `-steno-window=960x600`, never in the next one: AppKit
/// pairs dash-prefixed arguments blindly at launch and opens whatever is
/// left over as a document, after which SwiftUI leaves the primary window
/// closed. A value in its own argument is therefore reported as missing.
struct UITestScenario: Equatable, Sendable {
  static let prefix = "-steno-"
  static let uiTestingFlag = "-steno-ui-testing"
  static let showPromptFlag = "-steno-show-prompt"
  /// The preview store stays empty: the empty states of the list and the
  /// detail pane.
  static let emptyFlag = "-steno-empty"
  /// The preview store gets `PreviewSeed.Fixtures.rich`: three days, every state.
  static let richSeedFlag = "-steno-rich-seed"
  /// After launch, a call recording starts from the window, as the sidebar
  /// control would start it, so the live row and the Stop controls show.
  static let startRecordingFlag = "-steno-start-recording"
  /// The preview's permissions are all `.unknown` and the onboarding window
  /// opens at launch, on page 1.
  static let showOnboardingFlag = "-steno-show-onboarding"
  /// Read by `AppEnvironment.preview()` itself (`holdTranscribeArgument`,
  /// main-actor isolated, so the literal is repeated here and a test pins
  /// the two equal); named here so it is known.
  static let holdTranscribeFlag = "-steno-ui-testing-hold-transcribe"
  /// `-steno-appearance=light|dark`: `AppDelegate` sets `NSApp.appearance`
  /// to it at launch, so the smoke test can screenshot both appearances on
  /// the hosted runner, which is light.
  static let appearanceFlag = "-steno-appearance"
  /// `-steno-window=WxH` in points (`960x600`): the main window's size at
  /// launch, applied over SwiftUI's `defaultSize` and its restored frame.
  static let windowFlag = "-steno-window"
  /// `-steno-settings-section=<rawValue>` (`recording`): the section
  /// Settings opens on, through `AppController.requestedSettingsSection`,
  /// the deep link the setup banner uses. The smoke test cannot click a
  /// sidebar row: the rows' accessibility frames sit off the rendered rows.
  static let settingsSectionFlag = "-steno-settings-section"
  static let knownFlags: Set<String> = [
    uiTestingFlag, showPromptFlag, emptyFlag, richSeedFlag, startRecordingFlag,
    showOnboardingFlag, holdTranscribeFlag, appearanceFlag, windowFlag, settingsSectionFlag,
  ]
  /// The flags that take a `=value`; every other known flag takes none.
  static let valuedFlags: Set<String> = [appearanceFlag, windowFlag, settingsSectionFlag]

  enum Appearance: String, Sendable {
    case light
    case dark
  }

  /// A window size in points.
  struct WindowSize: Equatable, Sendable {
    var width: Double
    var height: Double
  }

  /// The preview environment: in-memory database, fakes, synthetic audio.
  var isUITesting: Bool
  /// After launch, a detection prompt for "Zoom" is shown.
  var showPrompt: Bool
  /// What the preview store is seeded with; nil under `-steno-empty`.
  var seed: PreviewSeed.Fixtures?
  /// A call recording starts once the controller has launched.
  var startsRecording: Bool
  /// The onboarding window opens at launch over unknown permissions.
  var showsOnboarding: Bool
  /// The preview's speech engine holds each transcribe for a minute and the
  /// sample meeting is queued at launch (`AppEnvironment.preview()`).
  var holdTranscribe: Bool
  /// `NSApp.appearance` at launch; nil leaves the system appearance.
  var appearance: Appearance?
  /// The main window's size at launch; nil leaves SwiftUI's default.
  var windowSize: WindowSize?
  /// The section Settings opens on once its window shows; nil leaves General.
  var settingsSection: SettingsSection?
  /// `-steno-*` arguments that name no flag, in order.
  var unknownFlags: [String]
  /// Flags whose value is missing or malformed, each as
  /// `"<flag> <value>"`, in order.
  var invalidValues: [String]

  init(arguments: [String]) {
    let flags = arguments.filter { $0.hasPrefix(Self.prefix) }
    let names = flags.map(Self.name(of:))
    var invalid: [String] = []
    appearance = Self.value(of: Self.appearanceFlag, in: flags, invalid: &invalid) {
      Appearance(rawValue: $0)
    }
    windowSize = Self.value(of: Self.windowFlag, in: flags, invalid: &invalid) {
      WindowSize($0)
    }
    settingsSection = Self.value(of: Self.settingsSectionFlag, in: flags, invalid: &invalid) {
      SettingsSection(rawValue: $0)
    }
    for flag in flags where flag.contains("=") {
      let name = Self.name(of: flag)
      if Self.knownFlags.contains(name), !Self.valuedFlags.contains(name) {
        invalid.append("\(name) takes no value")
      }
    }
    invalidValues = invalid
    isUITesting = names.contains(Self.uiTestingFlag)
    showPrompt = names.contains(Self.showPromptFlag)
    if names.contains(Self.emptyFlag) {
      seed = nil
    } else if names.contains(Self.richSeedFlag) {
      seed = .rich
    } else {
      seed = .sample
    }
    startsRecording = names.contains(Self.startRecordingFlag)
    showsOnboarding = names.contains(Self.showOnboardingFlag)
    holdTranscribe = names.contains(Self.holdTranscribeFlag)
    unknownFlags = names.filter { !Self.knownFlags.contains($0) }
  }

  /// Under `-steno-ui-testing`, the message `AppBootstrap` shows instead of
  /// the app when a flag is misspelt, so the smoke test's window and its
  /// screenshot carry the reason. Nil when every flag is known, and outside
  /// UI testing, where a stray `-steno-*` argument is not ours to judge.
  var launchError: String? {
    guard isUITesting else { return nil }
    var lines: [String] = []
    if !unknownFlags.isEmpty {
      lines.append("Unknown UI-test flags: \(unknownFlags.joined(separator: ", "))")
    }
    if !invalidValues.isEmpty {
      lines.append("Invalid UI-test values: \(invalidValues.joined(separator: ", "))")
    }
    return lines.isEmpty ? nil : lines.joined(separator: "\n")
  }

  /// The part of `argument` before its first `=`: the flag itself.
  static func name(of argument: String) -> String {
    guard let equals = argument.firstIndex(of: "=") else { return argument }
    return String(argument[..<equals])
  }

  /// The text after `=` in the first of `flags` named `flag`; nil when the
  /// flag is absent. A flag with no `=`, or nothing after it, or a value
  /// `parse` rejects is recorded in `invalid` as `"<flag> <value>"`.
  private static func value<Value>(
    of flag: String, in flags: [String], invalid: inout [String],
    _ parse: (String) -> Value?
  ) -> Value? {
    guard let argument = flags.first(where: { name(of: $0) == flag }) else { return nil }
    let text = String(argument.dropFirst(flag.count + 1))
    guard !text.isEmpty else {
      invalid.append("\(flag) (no value)")
      return nil
    }
    guard let parsed = parse(text) else {
      invalid.append("\(flag) \(text)")
      return nil
    }
    return parsed
  }
}

extension UITestScenario.WindowSize {
  /// `960x600`; nil for anything else, including a missing height or a
  /// zero side.
  init?(_ text: String) {
    let parts = text.split(separator: "x", omittingEmptySubsequences: false)
    guard parts.count == 2, let width = Double(parts[0]), let height = Double(parts[1]),
      width > 0, height > 0
    else { return nil }
    self.init(width: width, height: height)
  }
}
