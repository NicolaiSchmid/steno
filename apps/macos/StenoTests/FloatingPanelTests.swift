import AppKit
import StenoAudio
import StenoCore
import SwiftUI
import XCTest

/// A `PanelHost` that records what the model asked of it and answers with
/// the screens a test sets.
@MainActor
final class FakePanelHost: PanelHost {
  var currentScreens: [CGRect] = [CGRect(x: 0, y: 0, width: 1512, height: 944)]
  private(set) var shownFrames: [CGRect] = []
  private(set) var hides = 0

  func setContent(_ view: AnyView) {}
  func show(frame: CGRect) { shownFrames.append(frame) }
  func hide() { hides += 1 }
}

/// The floating panel's pure pieces: what it shows, how the bubble and the
/// menu bar label render each recorder state, where the panel sits and how
/// the anchor survives moves and screen changes, the level history, and the
/// presenter's observation of a real controller over the fake host.
@MainActor
final class FloatingPanelTests: XCTestCase {
  private let defaultsSuite = "uno.schmid.steno.mac.tests.panel.\(UUID().uuidString)"
  private let bubbleSize = CGSize(width: 168, height: PanelMetrics.bubbleHeight)
  private let promptSize = CGSize(width: 420, height: PanelMetrics.promptHeight)

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
    let since = TestSupport.now
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

  /// The full cross product of the four recorder states and an armed or
  /// absent auto-stop: bars, the clock and the auto-stop row only while
  /// recording; the stop square hidden while starting and disabled while
  /// stopping; the transient text in the two busy states.
  func testBubbleShowsBarsOnlyWhileRecordingAndDisablesStopWhileStopping() {
    let since = TestSupport.now
    let armed = AutoStopPresentation(appName: "Zoom", remainingText: "1:29", fractionRemaining: 0.5)
    let states: [RecordingState] = [.idle, .starting, .recording(since: since), .stopping]
    for autoStop in [nil, armed] {
      for state in states {
        let bubble = BubblePresentation.make(state: state, autoStop: autoStop)
        let isRecording = state == .recording(since: since)
        let label = "\(state), autoStop \(autoStop == nil ? "nil" : "armed")"
        XCTAssertEqual(bubble.showsBars, isRecording, label)
        XCTAssertEqual(bubble.stopEnabled, isRecording, label)
        XCTAssertEqual(bubble.since, isRecording ? since : nil, label)
        XCTAssertEqual(bubble.autoStop, isRecording ? autoStop : nil, label)
        switch state {
        case .idle:
          XCTAssertNil(bubble.text, label)
          XCTAssertFalse(bubble.showsStop, label)
        case .starting:
          XCTAssertEqual(bubble.text, "Starting…", label)
          XCTAssertFalse(bubble.showsStop, label)
        case .recording:
          XCTAssertNil(bubble.text, label)
          XCTAssertTrue(bubble.showsStop, label)
        case .stopping:
          XCTAssertEqual(bubble.text, "Finishing…", label)
          XCTAssertTrue(bubble.showsStop, label)
        }
      }
    }
  }

  // MARK: - MenuBarLabelPresentation

  func testMenuBarLabelShowsTheElapsedTimeOnlyWhileRecording() {
    let since = TestSupport.now
    let now = since.addingTimeInterval(754)
    let idle = MenuBarLabelPresentation.make(state: .idle, now: now)
    XCTAssertEqual(idle.symbolName, "waveform")
    XCTAssertNil(idle.elapsedText)
    XCTAssertEqual(idle.accessibilityLabel, "Steno")
    for state in [RecordingState.starting, .stopping] {
      let busy = MenuBarLabelPresentation.make(state: state, now: now)
      XCTAssertEqual(busy.symbolName, "record.circle.fill", "\(state)")
      XCTAssertNil(busy.elapsedText, "\(state)")
      XCTAssertEqual(busy.accessibilityLabel, "Steno, recording", "\(state)")
    }
    let recording = MenuBarLabelPresentation.make(state: .recording(since: since), now: now)
    XCTAssertEqual(recording.symbolName, "record.circle.fill")
    XCTAssertEqual(recording.elapsedText, "12:34")
    XCTAssertEqual(recording.accessibilityLabel, "Steno, recording, 12:34")
  }

  // MARK: - PanelAnchor

  func testDefaultAnchorSitsAtTheTopCentreOfTheVisibleFrame() {
    let visible = CGRect(x: 0, y: 0, width: 1512, height: 944)
    let anchor = PanelAnchor.default(in: visible)
    XCTAssertEqual(anchor.topCenter.x, 756)
    XCTAssertEqual(anchor.topCenter.y, 944 - Theme.Space.sm)
    XCTAssertEqual(anchor.screenFrame, visible)
  }

  func testFrameForSizeKeepsTheTopCentrePoint() {
    let anchor = PanelAnchor(
      topCenter: CGPoint(x: 756, y: 936), screenFrame: CGRect(x: 0, y: 0, width: 1512, height: 944))
    for size in [promptSize, bubbleSize] {
      let frame = anchor.frame(for: size)
      XCTAssertEqual(frame.size, size)
      XCTAssertEqual(frame.midX, 756, "\(size)")
      XCTAssertEqual(frame.maxY, 936, "\(size)")
    }
  }

  /// Validation checks the panel's frame, not its top-centre point: an
  /// anchor whose panel would hang below the screen's bottom edge falls
  /// back although the point itself is on screen.
  func testValidatedKeepsAnAnchorOnASecondScreenAndDropsOneOffEveryScreen() {
    let first = CGRect(x: 0, y: 0, width: 1512, height: 944)
    let second = CGRect(x: 1512, y: 0, width: 2560, height: 1415)
    let onSecond = PanelAnchor(topCenter: CGPoint(x: 2800, y: 1400), screenFrame: second)
    XCTAssertEqual(
      PanelAnchor.validated(onSecond, size: bubbleSize, screens: [first, second], fallback: first),
      onSecond)
    XCTAssertEqual(
      PanelAnchor.validated(onSecond, size: bubbleSize, screens: [first], fallback: first),
      .default(in: first), "a saved anchor off every screen falls back")
    XCTAssertEqual(
      PanelAnchor.validated(nil, size: bubbleSize, screens: [first], fallback: first),
      .default(in: first))

    let nearTheBottom = PanelAnchor(topCenter: CGPoint(x: 756, y: 20), screenFrame: first)
    XCTAssertEqual(
      PanelAnchor.validated(nearTheBottom, size: bubbleSize, screens: [first], fallback: first),
      .default(in: first), "the point is on screen but the 40 pt panel is not")
    XCTAssertEqual(
      PanelAnchor.validated(
        nearTheBottom, size: CGSize(width: 100, height: 20), screens: [first], fallback: first),
      nearTheBottom, "a shorter panel fits")

    XCTAssertEqual(
      PanelAnchor.from(
        frame: CGRect(x: 2716, y: 1360, width: 168, height: 40), screens: [first, second],
        fallback: first
      ).screenFrame, second, "a frame on the second screen is described with that screen")
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
    XCTAssertEqual(model.content, .prompt(prompt))
    XCTAssertTrue(host.shownFrames.isEmpty, "nothing to show before the root has measured")
    model.contentSizeDidChange(promptSize)
    let screen = host.currentScreens[0]
    XCTAssertEqual(host.shownFrames, [PanelAnchor.default(in: screen).frame(for: promptSize)])
    model.contentSizeDidChange(CGSize(width: promptSize.width + 0.4, height: promptSize.height))
    XCTAssertEqual(host.shownFrames.count, 1, "sub-point rounding is not a new size")

    model.apply(nil)
    XCTAssertEqual(host.hides, 2)
    XCTAssertEqual(model.content, .prompt(prompt), "the content stays while fading out")
    model.contentSizeDidChange(CGSize(width: 300, height: 56))
    XCTAssertEqual(host.shownFrames.count, 1, "a hidden panel is not shown by a size change")

    // The same content shown again is placed at once at its known size.
    model.apply(.prompt(prompt))
    XCTAssertEqual(host.shownFrames.count, 2)
    XCTAssertEqual(host.shownFrames.last?.size, CGSize(width: 300, height: 56))
  }

  /// The prompt morphs into the bubble at the same top-centre point, and
  /// the window waits for the bubble's size rather than showing the bubble
  /// in the prompt's frame.
  func testPromptThenBubbleKeepsTheAnchorAndResizes() throws {
    let host = FakePanelHost()
    let model = FloatingPanelModel(host: host, defaults: try makeDefaults())
    model.apply(.prompt(makePrompt()))
    model.contentSizeDidChange(promptSize)
    let shownAsPrompt = host.shownFrames.count
    model.apply(.bubble)
    XCTAssertEqual(model.content, .bubble)
    XCTAssertEqual(host.shownFrames.count, shownAsPrompt, "not re-placed at the prompt's size")
    model.contentSizeDidChange(bubbleSize)
    XCTAssertGreaterThanOrEqual(host.shownFrames.count, 2)
    let prompt = host.shownFrames[0]
    let bubble = try XCTUnwrap(host.shownFrames.last)
    XCTAssertEqual(prompt.size, promptSize)
    XCTAssertEqual(bubble.size, bubbleSize)
    XCTAssertEqual(prompt.midX, bubble.midX)
    XCTAssertEqual(prompt.maxY, bubble.maxY)

    // The armed auto-stop row widens the bubble: the same rule re-anchors it.
    let armedSize = CGSize(width: 420, height: PanelMetrics.bubbleArmedHeight)
    model.contentSizeDidChange(armedSize)
    let armed = try XCTUnwrap(host.shownFrames.last)
    XCTAssertEqual(armed.size, armedSize)
    XCTAssertEqual(armed.midX, bubble.midX)
    XCTAssertEqual(armed.maxY, bubble.maxY)
  }

  func testAMoveSavesTheAnchorToTheInjectedDefaults() throws {
    let defaults = try makeDefaults()
    let host = FakePanelHost()
    let model = FloatingPanelModel(host: host, defaults: defaults)
    model.apply(.bubble)
    model.contentSizeDidChange(bubbleSize)

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
    again.contentSizeDidChange(bubbleSize)
    XCTAssertEqual(host.shownFrames.last, saved.frame(for: bubbleSize))
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
    model.apply(.bubble)
    model.contentSizeDidChange(bubbleSize)
    XCTAssertEqual(host.shownFrames.last, onSecond.frame(for: bubbleSize))

    host.currentScreens = [first]
    model.screensDidChange()
    let fallback = PanelAnchor.default(in: first)
    XCTAssertEqual(model.anchor, fallback)
    XCTAssertEqual(host.shownFrames.last, fallback.frame(for: bubbleSize))
    let saved = try JSONDecoder().decode(
      PanelAnchor.self, from: try XCTUnwrap(defaults.data(forKey: FloatingPanelModel.anchorKey)))
    XCTAssertEqual(saved, fallback, "the stale anchor is replaced")

    // Hidden panels re-validate without being shown.
    model.apply(nil)
    let shown = host.shownFrames.count
    model.screensDidChange()
    XCTAssertEqual(host.shownFrames.count, shown)
  }

  // MARK: - FloatingPanelPresenter

  /// The presenter over a real controller and the fake host: the prompt
  /// shows the panel, Record turns it into the bubble and starts the clock,
  /// stop hides it and holds the clock, and Quit while recording hides it
  /// again. This is the one test of the observation loop and of the product
  /// wiring of `RecordingClock`.
  func testPresenterFollowsTheControllerAndDrivesTheClock() async throws {
    let manual = ManualClock()
    let environment = try await TestSupport.environment(clock: manual, seed: false)
    let controller = AppController(environment: environment, defaults: try makeDefaults())
    controller.detection.appName = { $0 ?? "?" }
    await controller.launch()
    let host = FakePanelHost()
    let clock = RecordingClock(clock: manual)
    let presenter = FloatingPanelPresenter(defaults: try makeDefaults(), makeHost: { host })
    presenter.follow(controller, clock: clock, openMain: {})
    let model = try XCTUnwrap(presenter.model)
    XCTAssertNil(model.content, "idle without a prompt shows nothing")
    let hidesAtLaunch = host.hides

    await controller.detection.handle(.microphoneOpened(bundleID: "com.apple.FaceTime", pid: 7))
    let prompt = try XCTUnwrap(controller.detection.prompt)
    await TestSupport.waitUntil("the presenter to show the prompt") {
      model.content == .prompt(prompt)
    }
    XCTAssertTrue(model.wantsShown)
    model.contentSizeDidChange(promptSize)
    XCTAssertEqual(host.shownFrames.count, 1)
    XCTAssertFalse(clock.isTicking)

    await prompt.start()
    await TestSupport.waitUntil("the prompt to become the bubble") { model.content == .bubble }
    await TestSupport.waitUntil("the clock to tick") { clock.isTicking }
    model.contentSizeDidChange(bubbleSize)
    XCTAssertEqual(host.shownFrames.count, 2)
    XCTAssertEqual(host.shownFrames.last?.size, bubbleSize)
    XCTAssertEqual(host.hides, hidesAtLaunch, "the same panel morphs; nothing hides")

    await controller.recorder.stop()
    await TestSupport.waitUntil("the panel to hide after stop") { host.hides == hidesAtLaunch + 1 }
    XCTAssertFalse(clock.isTicking)
    XCTAssertEqual(model.content, .bubble, "the bubble stays for the fade-out")

    // Quit while recording: the bubble shows during the stop and the panel
    // hides when the recorder is idle again.
    await controller.recorder.start(mode: .inPerson)
    await TestSupport.waitUntil("the bubble to return") { host.shownFrames.count >= 3 }
    await TestSupport.waitUntil("the clock to tick again") { clock.isTicking }
    await controller.shutdown()
    await TestSupport.waitUntil("the panel to hide on quit") { host.hides == hidesAtLaunch + 2 }
    XCTAssertFalse(clock.isTicking)
    await environment.pipeline.waitUntilIdle()
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
