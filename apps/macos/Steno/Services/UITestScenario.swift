import Foundation

/// The launch arguments the UI smoke tests pass, parsed once. Every flag
/// starts with `-steno-`; an unknown one is reported, so a typo in the smoke
/// suite is not a silent no-op.
struct UITestScenario: Equatable, Sendable {
  static let prefix = "-steno-"
  static let uiTestingFlag = "-steno-ui-testing"
  /// The preview store stays empty: the empty states of the list and the
  /// detail pane.
  static let emptyFlag = "-steno-empty"
  /// The preview store gets `PreviewSeed.Set.rich`: three days, every state.
  static let richSeedFlag = "-steno-rich-seed"
  /// After launch, a call recording starts from the window, as the sidebar
  /// control would start it, so the live row and the Stop controls show.
  static let startRecordingFlag = "-steno-start-recording"
  /// `AppEnvironment.holdTranscribeArgument`, listed so it is not reported.
  static let holdTranscribeFlag = "-steno-ui-testing-hold-transcribe"
  static let knownFlags: Set<String> = [
    uiTestingFlag, emptyFlag, richSeedFlag, startRecordingFlag, holdTranscribeFlag,
  ]

  /// The preview environment: in-memory database, fakes, synthetic audio.
  var isUITesting: Bool
  /// What the preview store is seeded with; nil under `-steno-empty`.
  var seed: PreviewSeed.Set?
  /// A call recording starts once the controller has launched.
  var startsRecording: Bool
  /// `-steno-*` arguments that name no flag, in order.
  var unknownFlags: [String]

  init(arguments: [String]) {
    let flags = arguments.filter { $0.hasPrefix(Self.prefix) }
    isUITesting = flags.contains(Self.uiTestingFlag)
    if flags.contains(Self.emptyFlag) {
      seed = nil
    } else if flags.contains(Self.richSeedFlag) {
      seed = .rich
    } else {
      seed = .sample
    }
    startsRecording = flags.contains(Self.startRecordingFlag)
    unknownFlags = flags.filter { !Self.knownFlags.contains($0) }
  }
}
