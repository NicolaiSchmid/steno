import Foundation

/// The launch arguments the UI smoke tests pass, parsed once. Every flag
/// starts with `-steno-`; an unknown one is reported, so a typo in the smoke
/// suite is not a silent no-op.
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
  /// Takes a value, `light` or `dark`: `AppDelegate` sets `NSApp.appearance`
  /// to it at launch, so the smoke test can screenshot both appearances on
  /// the hosted runner, which is light.
  static let appearanceFlag = "-steno-appearance"
  /// Takes a value, `WxH` in points (`960x600`): the main window's size at
  /// launch, applied over SwiftUI's `defaultSize` and its restored frame.
  static let windowFlag = "-steno-window"
  static let knownFlags: Set<String> = [
    uiTestingFlag, showPromptFlag, emptyFlag, richSeedFlag, startRecordingFlag,
    showOnboardingFlag, holdTranscribeFlag, appearanceFlag, windowFlag,
  ]

  enum Appearance: String, Sendable {
    case light
    case dark
  }

  /// A window size in points, parsed from `WxH`.
  struct WindowSize: Equatable, Sendable {
    var width: Double
    var height: Double

    init(width: Double, height: Double) {
      self.width = width
      self.height = height
    }

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
  /// `-steno-*` arguments that name no flag, in order.
  var unknownFlags: [String]
  /// Flags whose value is missing or malformed, each as
  /// `"<flag> <value>"`, in order.
  var invalidValues: [String]

  init(arguments: [String]) {
    let flags = arguments.filter { $0.hasPrefix(Self.prefix) }
    var invalidValues: [String] = []
    if let text = Self.value(of: Self.appearanceFlag, in: arguments, invalid: &invalidValues) {
      if let parsed = Appearance(rawValue: text) {
        appearance = parsed
      } else {
        invalidValues.append("\(Self.appearanceFlag) \(text)")
      }
    }
    if let text = Self.value(of: Self.windowFlag, in: arguments, invalid: &invalidValues) {
      if let parsed = WindowSize(text) {
        windowSize = parsed
      } else {
        invalidValues.append("\(Self.windowFlag) \(text)")
      }
    }
    self.invalidValues = invalidValues
    isUITesting = flags.contains(Self.uiTestingFlag)
    showPrompt = flags.contains(Self.showPromptFlag)
    if flags.contains(Self.emptyFlag) {
      seed = nil
    } else if flags.contains(Self.richSeedFlag) {
      seed = .rich
    } else {
      seed = .sample
    }
    startsRecording = flags.contains(Self.startRecordingFlag)
    showsOnboarding = flags.contains(Self.showOnboardingFlag)
    holdTranscribe = flags.contains(Self.holdTranscribeFlag)
    unknownFlags = flags.filter { !Self.knownFlags.contains($0) }
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

  /// The argument after the first `flag`, when there is one and it is not a
  /// flag itself; a `flag` without a value is recorded in `invalid`. Nil
  /// when `flag` is absent.
  private static func value(
    of flag: String, in arguments: [String], invalid: inout [String]
  ) -> String? {
    guard let index = arguments.firstIndex(of: flag) else { return nil }
    let next = arguments.index(after: index)
    guard next < arguments.endIndex, !arguments[next].hasPrefix("-") else {
      invalid.append("\(flag) (no value)")
      return nil
    }
    return arguments[next]
  }
}
