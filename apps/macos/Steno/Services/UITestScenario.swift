import Foundation

/// The launch arguments the UI smoke tests pass, parsed once. Every flag
/// starts with `-steno-`; an unknown one is reported, so a typo in the smoke
/// suite is not a silent no-op.
struct UITestScenario: Equatable, Sendable {
  static let prefix = "-steno-"
  static let uiTestingFlag = "-steno-ui-testing"
  static let showPromptFlag = "-steno-show-prompt"
  static let knownFlags: Set<String> = [uiTestingFlag, showPromptFlag]

  /// The preview environment: in-memory database, fakes, synthetic audio.
  var isUITesting: Bool
  /// After launch, a detection prompt for "Zoom" is shown.
  var showPrompt: Bool
  /// `-steno-*` arguments that name no flag, in order.
  var unknownFlags: [String]

  init(arguments: [String]) {
    let flags = arguments.filter { $0.hasPrefix(Self.prefix) }
    isUITesting = flags.contains(Self.uiTestingFlag)
    showPrompt = flags.contains(Self.showPromptFlag)
    unknownFlags = flags.filter { !Self.knownFlags.contains($0) }
  }

  /// Under `-steno-ui-testing`, the message `AppBootstrap` shows instead of
  /// the app when a flag is misspelt, so the smoke test's window and its
  /// screenshot carry the reason. Nil when every flag is known, and outside
  /// UI testing, where a stray `-steno-*` argument is not ours to judge.
  var launchError: String? {
    guard isUITesting, !unknownFlags.isEmpty else { return nil }
    return "Unknown UI-test flags: \(unknownFlags.joined(separator: ", "))"
  }
}
