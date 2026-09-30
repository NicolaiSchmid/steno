import XCTest

/// The launch-argument parser the UI smoke tests rely on: known flags parse,
/// a misspelt `-steno-*` flag becomes the launch error the window shows.
final class UITestScenarioTests: XCTestCase {
  func testTheKnownFlagsParse() {
    let none = UITestScenario(arguments: ["/Applications/Steno.app/Contents/MacOS/Steno"])
    XCTAssertFalse(none.isUITesting)
    XCTAssertFalse(none.showPrompt)
    XCTAssertFalse(none.holdTranscribe)
    XCTAssertEqual(none.seed, .sample)
    XCTAssertFalse(none.startsRecording)
    XCTAssertFalse(none.showsOnboarding)
    XCTAssertEqual(none.unknownFlags, [])

    let testing = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-NSSomeAppKitFlag", "YES",
    ])
    XCTAssertTrue(testing.isUITesting)
    XCTAssertFalse(testing.showPrompt)
    XCTAssertEqual(testing.unknownFlags, [], "flags outside the prefix are not ours")

    let prompt = UITestScenario(arguments: ["Steno", "-steno-ui-testing", "-steno-show-prompt"])
    XCTAssertTrue(prompt.isUITesting)
    XCTAssertTrue(prompt.showPrompt)
    XCTAssertEqual(prompt.unknownFlags, [])

    let hold = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-ui-testing-hold-transcribe",
    ])
    XCTAssertTrue(hold.holdTranscribe, "the processing card's flag is known")
    XCTAssertEqual(hold.unknownFlags, [])
    XCTAssertNil(hold.launchError)
  }

  /// The onboarding flag: the window opens over unknown permissions, apart
  /// from the recording flag.
  func testTheOnboardingFlagParses() {
    let onboarding = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-show-onboarding",
    ])
    XCTAssertTrue(onboarding.showsOnboarding)
    XCTAssertFalse(onboarding.startsRecording)
    XCTAssertEqual(onboarding.seed, .sample)
    XCTAssertEqual(onboarding.unknownFlags, [])
    XCTAssertNil(onboarding.launchError)

    let recording = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-start-recording",
    ])
    XCTAssertFalse(recording.showsOnboarding)
  }

  /// The seed and recording flags of the redesign's smoke tests: rich seed,
  /// empty store (which wins over a seed) and a recording started at launch.
  func testTheSeedAndRecordingFlagsParse() {
    let rich = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-rich-seed", "-steno-start-recording",
    ])
    XCTAssertTrue(rich.isUITesting)
    XCTAssertEqual(rich.seed, .rich)
    XCTAssertTrue(rich.startsRecording)
    XCTAssertEqual(rich.unknownFlags, [])
    XCTAssertNil(rich.launchError)

    let empty = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-empty", "-steno-rich-seed",
    ])
    XCTAssertNil(empty.seed, "an empty store wins over a seed")
    XCTAssertFalse(empty.startsRecording)
    XCTAssertEqual(empty.unknownFlags, [])

    let typo = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-rich-sed", "-NSDocumentRevisionsDebugMode", "YES",
    ])
    XCTAssertEqual(typo.unknownFlags, ["-steno-rich-sed"])
    XCTAssertEqual(typo.seed, .sample, "a misspelt seed flag does not change the seed")
    XCTAssertEqual(typo.launchError, "Unknown UI-test flags: -steno-rich-sed")
  }

  /// The flag `AppEnvironment.preview()` reads is the one the parser knows.
  @MainActor func testTheHoldTranscribeFlagIsTheEnvironmentsArgument() {
    XCTAssertEqual(UITestScenario.holdTranscribeFlag, AppEnvironment.holdTranscribeArgument)
  }

  func testAnUnknownStenoFlagIsReported() {
    let scenario = UITestScenario(arguments: ["Steno", "-steno-ui-testing", "-steno-show-promt"])
    XCTAssertTrue(scenario.isUITesting)
    XCTAssertFalse(scenario.showPrompt)
    XCTAssertEqual(scenario.unknownFlags, ["-steno-show-promt"])
  }

  /// `AppBootstrap.load()` shows `launchError` instead of the app, so the
  /// smoke test fails on the reason and not on a later timeout.
  func testUnknownFlagsBecomeTheLaunchError() {
    XCTAssertNil(
      UITestScenario(arguments: ["Steno", "-steno-ui-testing", "-steno-show-prompt"]).launchError)
    XCTAssertEqual(
      UITestScenario(arguments: ["Steno", "-steno-ui-testing", "-steno-show-promt", "-steno-x"])
        .launchError,
      "Unknown UI-test flags: -steno-show-promt, -steno-x")
    XCTAssertNil(
      UITestScenario(arguments: ["Steno", "-steno-show-promt"]).launchError,
      "outside UI testing a stray flag is not ours to report")
  }

  /// `-steno-appearance=light|dark` and `-steno-window=WxH`. Without them
  /// nothing is set. The value sits in the flag's own argument: a separate
  /// token is a document AppKit opens at launch, after which SwiftUI leaves
  /// the primary window closed.
  func testTheAppearanceAndWindowFlagsParse() {
    let plain = UITestScenario(arguments: ["Steno", "-steno-ui-testing"])
    XCTAssertNil(plain.appearance)
    XCTAssertNil(plain.windowSize)
    XCTAssertEqual(plain.invalidValues, [])

    let dark = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-appearance=dark", "-steno-window=960x600",
      "-steno-empty",
    ])
    XCTAssertEqual(dark.appearance, .dark)
    XCTAssertEqual(dark.windowSize, UITestScenario.WindowSize(width: 960, height: 600))
    XCTAssertNil(dark.seed, "the flags after the valued ones still parse")
    XCTAssertEqual(dark.unknownFlags, [], "a valued flag is known by its name")
    XCTAssertEqual(dark.invalidValues, [])
    XCTAssertNil(dark.launchError)

    let light = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-appearance=light",
    ])
    XCTAssertEqual(light.appearance, .light)
    XCTAssertNil(light.windowSize)
    XCTAssertNil(light.launchError)
  }

  /// A bad or missing value is a launch error of its own line, beside the
  /// unknown-flag line, so the smoke test fails on the reason. A value in
  /// the next argument is missing: that spelling is the one AppKit opens as
  /// a document.
  func testBadAppearanceAndWindowValuesBecomeTheLaunchError() {
    let sepia = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-appearance=sepia",
    ])
    XCTAssertNil(sepia.appearance)
    XCTAssertEqual(sepia.invalidValues, ["-steno-appearance sepia"])
    XCTAssertEqual(sepia.launchError, "Invalid UI-test values: -steno-appearance sepia")

    let noHeight = UITestScenario(arguments: ["Steno", "-steno-ui-testing", "-steno-window=960"])
    XCTAssertNil(noHeight.windowSize)
    XCTAssertEqual(noHeight.launchError, "Invalid UI-test values: -steno-window 960")

    for bad in ["960x", "x600", "0x600", "960x-1", "960 600", "wide"] {
      XCTAssertNil(UITestScenario.WindowSize(bad), bad)
    }

    let spaced = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-window", "960x600", "-steno-appearance=",
    ])
    XCTAssertNil(spaced.windowSize)
    XCTAssertNil(spaced.appearance)
    XCTAssertEqual(
      spaced.invalidValues, ["-steno-appearance (no value)", "-steno-window (no value)"],
      "a value in its own argument, or nothing after the equals sign, is no value")
    XCTAssertEqual(spaced.unknownFlags, [], "both flags are known; the stray token is not ours")

    let valued = UITestScenario(arguments: ["Steno", "-steno-ui-testing", "-steno-empty=1"])
    XCTAssertEqual(valued.launchError, "Invalid UI-test values: -steno-empty takes no value")

    let both = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-apearance=dark", "-steno-window=960",
    ])
    XCTAssertEqual(
      both.launchError,
      "Unknown UI-test flags: -steno-apearance\nInvalid UI-test values: -steno-window 960")

    XCTAssertNil(
      UITestScenario(arguments: ["Steno", "-steno-appearance=sepia"]).launchError,
      "outside UI testing a bad value is not ours to report")
  }

  /// `-steno-settings-section=<rawValue>`: the section Settings opens on,
  /// any `SettingsSection` case; anything else is a launch error.
  func testTheSettingsSectionFlagParses() {
    XCTAssertNil(UITestScenario(arguments: ["Steno", "-steno-ui-testing"]).settingsSection)

    let recording = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-settings-section=recording",
    ])
    XCTAssertEqual(recording.settingsSection, .recording)
    XCTAssertEqual(recording.unknownFlags, [])
    XCTAssertNil(recording.launchError)

    for section in SettingsSection.allCases {
      let scenario = UITestScenario(arguments: [
        "Steno", "-steno-ui-testing", "-steno-settings-section=\(section.rawValue)",
      ])
      XCTAssertEqual(scenario.settingsSection, section)
    }

    let audio = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-settings-section=audio",
    ])
    XCTAssertNil(audio.settingsSection)
    XCTAssertEqual(audio.launchError, "Invalid UI-test values: -steno-settings-section audio")
  }
}
