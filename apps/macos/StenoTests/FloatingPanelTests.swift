import AppKit
import StenoAudio
import StenoCore
import SwiftUI
import XCTest

/// A `PanelHost` that records what the model asked of it and answers with
/// the screens and the content size a test sets.
@MainActor
final class FakePanelHost: PanelHost {
  var currentScreens: [CGRect] = [CGRect(x: 0, y: 0, width: 1512, height: 944)]
  private(set) var isShown = false
  private(set) var shownFrames: [CGRect] = []
  private(set) var hides = 0
  private(set) var contents = 0

  func setContent(_ view: AnyView) { contents += 1 }

  func show(frame: CGRect) {
    isShown = true
    shownFrames.append(frame)
  }

  func hide() {
    isShown = false
    hides += 1
  }
}

/// The floating panel's pure pieces: what it shows, how the bubble and the
/// menu bar label render each recorder state, where the panel sits and how
/// the anchor survives moves and screen changes, and the level history.
@MainActor
final class FloatingPanelTests: XCTestCase {
  private let defaultsSuite = "uno.schmid.steno.mac.tests.panel.\(UUID().uuidString)"

  override func tearDown() {
    UserDefaults.standard.removePersistentDomain(forName: defaultsSuite)
  }

  private func makePrompt() -> DetectionPromptViewModel {
    DetectionPromptViewModel(appName: "Zoom", clock: ManualClock(), timeout: .seconds(60))
  }

  private func makeDefaults() throws -> UserDefaults {
    try XCTUnwrap(UserDefaults(suiteName: defaultsSuite))
  }

  // MARK: - resolve

  /// The bubble wins whenever the recorder is not idle; the prompt shows
  /// only when idle; nothing otherwise.
  func testResolvePrefersTheBubbleThenThePromptThenNothing() {
    let prompt = makePrompt()
    let since = Date(timeIntervalSince1970: 1_790_250_000)
    let busy: [RecordingState] = [.starting, .recording(since: since), .stopping]
    for state in busy {
      XCTAssertEqual(FloatingContent.resolve(prompt: prompt, recording: state), .bubble, "\(state)")
      XCTAssertEqual(FloatingContent.resolve(prompt: nil, recording: state), .bubble, "\(state)")
    }
    XCTAssertEqual(FloatingContent.resolve(prompt: prompt, recording: .idle), .prompt(prompt))
    XCTAssertNil(FloatingContent.resolve(prompt: nil, recording: .idle))
    XCTAssertNotEqual(FloatingContent.prompt(prompt), .prompt(makePrompt()), "identity, not value")
  }

  // MARK: - BubblePresentation

  func testBubblePresentationPerState() {
    let since = Date(timeIntervalSince1970: 1_790_250_000)
    let idle = BubblePresentation.make(state: .idle, autoStop: nil)
    XCTAssertFalse(idle.showsBars)
    XCTAssertFalse(idle.showsStop)

    let starting = BubblePresentation.make(state: .starting, autoStop: nil)
    XCTAssertEqual(starting.text, "Starting…")
    XCTAssertFalse(starting.showsBars)
    XCTAssertFalse(starting.showsStop)
    XCTAssertFalse(starting.stopEnabled)
    XCTAssertTrue(starting.isBusy)

    let autoStop = AutoStopPresentation(
      appName: "Zoom", remainingText: "1:29", fractionRemaining: 0.5)
    let recording = BubblePresentation.make(state: .recording(since: since), autoStop: autoStop)
    XCTAssertNil(recording.text)
    XCTAssertEqual(recording.since, since)
    XCTAssertTrue(recording.showsBars)
    XCTAssertTrue(recording.showsStop)
    XCTAssertTrue(recording.stopEnabled)
    XCTAssertFalse(recording.isBusy)
    XCTAssertEqual(recording.autoStop, autoStop, "passes through unchanged")

    let stopping = BubblePresentation.make(state: .stopping, autoStop: autoStop)
    XCTAssertEqual(stopping.text, "Finishing…")
    XCTAssertFalse(stopping.showsBars)
    XCTAssertTrue(stopping.showsStop)
    XCTAssertFalse(stopping.stopEnabled)
    XCTAssertTrue(stopping.isBusy)
    XCTAssertNil(stopping.autoStop, "no countdown once the stop is under way")
  }

  func testAutoStopLineNamesTheAppOrTheCallApp() {
    let named = AutoStopPresentation(appName: "Zoom", remainingText: "1:29", fractionRemaining: 0.9)
    XCTAssertEqual(named.line, "Zoom closed the microphone. Stopping in 1:29.")
    let unknown = AutoStopPresentation(appName: nil, remainingText: "0:05", fractionRemaining: 0.1)
    XCTAssertEqual(unknown.line, "The call app closed the microphone. Stopping in 0:05.")
  }

  // MARK: - MenuBarLabelPresentation

  func testMenuBarLabelPerState() {
    let since = Date(timeIntervalSince1970: 1_790_250_000)
    let now = since.addingTimeInterval(754)
    let idle = MenuBarLabelPresentation.make(state: .idle, now: now)
    XCTAssertEqual(idle.symbolName, "waveform")
    XCTAssertNil(idle.elapsedText)
    XCTAssertEqual(idle.accessibilityLabel, "Steno")
    for state in [RecordingState.starting, .stopping] {
      let busy = MenuBarLabelPresentation.make(state: state, now: now)
      XCTAssertEqual(busy.symbolName, "record.circle.fill", "\(state)")
      XCTAssertNil(busy.elapsedText, "\(state)")
    }
    let recording = MenuBarLabelPresentation.make(state: .recording(since: since), now: now)
    XCTAssertEqual(recording.symbolName, "record.circle.fill")
    XCTAssertEqual(recording.elapsedText, "12:34")
    XCTAssertEqual(recording.accessibilityLabel, "Steno, recording, 12:34")
  }

  // MARK: - PanelAnchor

  func testDefaultAnchorSitsAtTheTopCentreOfTheVisibleFrame() {
    let visible = CGRect(x: 0, y: 0, width: 1512, height: 944)
    let anchor = PanelAnchor.defaultAnchor(in: visible)
    XCTAssertEqual(anchor.x, 756)
    XCTAssertEqual(anchor.y, 944 - Theme.Space.sm)
  }

  func testFrameForSizeKeepsTheTopCentrePoint() {
    let anchor = PanelAnchor(
      topCenter: CGPoint(x: 756, y: 936), screenFrame: CGRect(x: 0, y: 0, width: 1512, height: 944))
    for size in [CGSize(width: 400, height: 56), CGSize(width: 160, height: 40)] {
      let frame = anchor.frame(for: size)
      XCTAssertEqual(frame.size, size)
      XCTAssertEqual(frame.midX, 756, "\(size)")
      XCTAssertEqual(frame.maxY, 936, "\(size)")
    }
  }

  func testValidatedKeepsAnAnchorOnASecondScreenAndDropsOneOffEveryScreen() {
    let first = CGRect(x: 0, y: 0, width: 1512, height: 944)
    let second = CGRect(x: 1512, y: 0, width: 2560, height: 1415)
    let onSecond = PanelAnchor(topCenter: CGPoint(x: 2800, y: 1400), screenFrame: second)
    XCTAssertEqual(
      PanelAnchor.validated(onSecond, screens: [first, second], fallback: first), onSecond)
    XCTAssertEqual(
      PanelAnchor.validated(onSecond, screens: [first], fallback: first), .default(in: first),
      "a saved anchor off every screen falls back")
    XCTAssertEqual(
      PanelAnchor.validated(nil, screens: [first], fallback: first), .default(in: first))
  }

  func testAnchorRoundTripsThroughJSON() throws {
    let anchor = PanelAnchor(
      topCenter: CGPoint(x: 12.5, y: 900), screenFrame: CGRect(x: 0, y: 0, width: 1512, height: 944)
    )
    let data = try JSONEncoder().encode(anchor)
    XCTAssertEqual(try JSONDecoder().decode(PanelAnchor.self, from: data), anchor)
  }

  // MARK: - FloatingPanelModel

  func testNilContentHidesAndContentShowsAtTheAnchor() throws {
    let host = FakePanelHost()
    let model = FloatingPanelModel(host: host, defaults: try makeDefaults())
    model.apply(nil)
    XCTAssertEqual(host.hides, 1)
    XCTAssertTrue(host.shownFrames.isEmpty)

    let prompt = makePrompt()
    model.apply(.prompt(prompt))
    XCTAssertEqual(model.state.content, .prompt(prompt))
    XCTAssertTrue(host.shownFrames.isEmpty, "nothing to show before the root has measured")
    let size = CGSize(width: 400, height: 56)
    model.contentSizeDidChange(size)
    let screen = host.currentScreens[0]
    XCTAssertEqual(host.shownFrames, [PanelAnchor.default(in: screen).frame(for: size)])
    model.contentSizeDidChange(CGSize(width: 400.4, height: 56))
    XCTAssertEqual(host.shownFrames.count, 1, "sub-point rounding is not a new size")

    model.apply(nil)
    XCTAssertEqual(host.hides, 2)
    XCTAssertEqual(model.state.content, .prompt(prompt), "the content stays while fading out")
    model.contentSizeDidChange(CGSize(width: 300, height: 56))
    XCTAssertEqual(host.shownFrames.count, 1, "a hidden panel is not shown by a size change")
  }

  /// The prompt morphs into the bubble at the same top-centre point.
  func testPromptThenBubbleKeepsTheAnchorAndResizes() throws {
    let host = FakePanelHost()
    let model = FloatingPanelModel(host: host, defaults: try makeDefaults())
    model.apply(.prompt(makePrompt()))
    model.contentSizeDidChange(CGSize(width: 420, height: 56))
    model.apply(.bubble)
    XCTAssertEqual(model.state.content, .bubble)
    // Shown at once at the last known size, then re-placed when the bubble
    // has measured.
    model.contentSizeDidChange(CGSize(width: 168, height: 40))
    XCTAssertEqual(host.shownFrames.count, 3)
    let prompt = host.shownFrames[0]
    let bubble = try XCTUnwrap(host.shownFrames.last)
    XCTAssertEqual(prompt.size, CGSize(width: 420, height: 56))
    XCTAssertEqual(bubble.size, CGSize(width: 168, height: 40))
    XCTAssertEqual(prompt.midX, bubble.midX)
    XCTAssertEqual(prompt.maxY, bubble.maxY)

    // The armed auto-stop row widens the bubble: the same rule re-anchors it.
    model.contentSizeDidChange(CGSize(width: 420, height: 64))
    let armed = try XCTUnwrap(host.shownFrames.last)
    XCTAssertEqual(armed.size, CGSize(width: 420, height: 64))
    XCTAssertEqual(armed.midX, bubble.midX)
    XCTAssertEqual(armed.maxY, bubble.maxY)
  }

  func testAMoveSavesTheAnchorToTheInjectedDefaults() throws {
    let defaults = try makeDefaults()
    let host = FakePanelHost()
    let model = FloatingPanelModel(host: host, defaults: defaults)
    model.apply(.bubble)
    model.contentSizeDidChange(CGSize(width: 168, height: 40))

    // A resize in flight (size differs from the placed one) is not a move.
    model.panelDidMove(to: CGRect(x: 100, y: 100, width: 300, height: 40))
    XCTAssertNil(defaults.data(forKey: FloatingPanelModel.anchorKey))

    let dragged = CGRect(x: 1200, y: 60, width: 168, height: 40)
    model.panelDidMove(to: dragged)
    let saved = try JSONDecoder().decode(
      PanelAnchor.self, from: try XCTUnwrap(defaults.data(forKey: FloatingPanelModel.anchorKey)))
    XCTAssertEqual(saved.topCenter, CGPoint(x: dragged.midX, y: dragged.maxY))
    XCTAssertEqual(saved.screenFrame, host.currentScreens[0])
    XCTAssertEqual(model.anchor, saved)

    // A relaunch reads the saved anchor back.
    let again = FloatingPanelModel(host: host, defaults: defaults)
    again.apply(.bubble)
    again.contentSizeDidChange(CGSize(width: 168, height: 40))
    XCTAssertEqual(host.shownFrames.last, saved.frame(for: CGSize(width: 168, height: 40)))
  }

  /// The saved screen is unplugged mid-call: the bubble comes to the
  /// remaining screen at its default anchor.
  func testScreenChangeWithTheSavedScreenGoneReanchorsToTheFallback() throws {
    let defaults = try makeDefaults()
    let host = FakePanelHost()
    let first = CGRect(x: 0, y: 0, width: 1512, height: 944)
    let second = CGRect(x: 1512, y: 0, width: 2560, height: 1415)
    host.currentScreens = [first, second]
    let onSecond = PanelAnchor(topCenter: CGPoint(x: 2800, y: 1400), screenFrame: second)
    defaults.set(try JSONEncoder().encode(onSecond), forKey: FloatingPanelModel.anchorKey)
    let model = FloatingPanelModel(host: host, defaults: defaults)
    let size = CGSize(width: 168, height: 40)
    model.apply(.bubble)
    model.contentSizeDidChange(size)
    XCTAssertEqual(host.shownFrames.last, onSecond.frame(for: size))

    host.currentScreens = [first]
    model.screensDidChange()
    let fallback = PanelAnchor.default(in: first)
    XCTAssertEqual(model.anchor, fallback)
    XCTAssertEqual(host.shownFrames.last, fallback.frame(for: size))
    let saved = try JSONDecoder().decode(
      PanelAnchor.self, from: try XCTUnwrap(defaults.data(forKey: FloatingPanelModel.anchorKey)))
    XCTAssertEqual(saved, fallback, "the stale anchor is replaced")

    // Hidden panels re-validate without being shown.
    model.apply(nil)
    let shown = host.shownFrames.count
    model.screensDidChange()
    XCTAssertEqual(host.shownFrames.count, shown)
  }

  // MARK: - LiveBarsHistory

  func testLiveBarsHistoryKeepsFiveSamplesNewestLast() {
    var history = LiveBarsHistory()
    XCTAssertEqual(history.fractions, [0, 0, 0, 0, 0])
    history.push(levels: nil)
    history.push(levels: LaneLevels(mic: LaneLevel(rms: -60, peak: -60), system: nil))
    history.push(levels: LaneLevels(mic: LaneLevel(rms: 0, peak: 0), system: nil))
    history.push(levels: LaneLevels(mic: LaneLevel(rms: -30, peak: -30), system: nil))
    XCTAssertEqual(history.fractions, [0, 0, 0, 1, 0.5])

    // The louder lane wins; `system` nil uses `mic` alone.
    history.push(
      levels: LaneLevels(
        mic: LaneLevel(rms: -60, peak: -60), system: LaneLevel(rms: -15, peak: -15)))
    XCTAssertEqual(history.fractions, [0, 0, 1, 0.5, 0.75])
    history.push(levels: LaneLevels(mic: LaneLevel(rms: -90, peak: -90), system: nil))
    XCTAssertEqual(history.fractions.last, 0, "below the floor clamps to silence")
    XCTAssertEqual(history.fractions.count, LiveBarsHistory.count)
  }
}
