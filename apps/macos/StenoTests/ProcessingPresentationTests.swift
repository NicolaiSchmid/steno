import StenoCore
import XCTest

/// `ProcessingPresentation.state` as a pure function of the last event and
/// the time since it: where the bar sits, that it never moves backwards,
/// and the wording table from the plan's D3.
final class ProcessingPresentationTests: XCTestCase {
  /// Transcribe at 10 %, next event expected at 40 %, three minutes left:
  /// `expectedTimeToNextEvent` is (0.3 / 0.9) x 180 s = 60 s.
  private let transcribing = ProcessingProgress(
    stage: .transcribe, fraction: 0.1, nextFraction: 0.4, estimatedRemaining: .seconds(180),
    isEstimateSeeded: false)

  private func state(
    _ progress: ProcessingProgress, elapsed: Duration, reduceMotion: Bool = false
  ) -> ProcessingPresentation.State {
    ProcessingPresentation.state(progress: progress, elapsed: elapsed, reduceMotion: reduceMotion)
  }

  func testFractionMovesFromTheEventToJustBeforeTheNextAndRests() {
    XCTAssertEqual(transcribing.expectedTimeToNextEvent, .seconds(60))
    XCTAssertEqual(state(transcribing, elapsed: .zero).fraction, 0.1, "the event's fraction")
    XCTAssertEqual(
      state(transcribing, elapsed: .seconds(30)).fraction, 0.245, accuracy: 1e-9, "halfway")
    let atExpected = state(transcribing, elapsed: .seconds(60))
    XCTAssertEqual(atExpected.fraction, 0.39, accuracy: 1e-9, "nextFraction - 0.01")
    XCTAssertFalse(atExpected.isSlow)
    XCTAssertEqual(atExpected.remainingText, "~2 min remaining")

    let late = state(transcribing, elapsed: .seconds(120))
    XCTAssertEqual(late.fraction, 0.39, accuracy: 1e-9, "rests short of the boundary")
    XCTAssertTrue(late.isSlow)
    XCTAssertEqual(late.remainingText, "a bit longer than usual")
  }

  func testFractionIsNonDecreasingOverASweepOfElapsedTimes() {
    var previous = -Double.infinity
    for tenths in stride(from: 0, through: 2000, by: 5) {
      let sample = state(transcribing, elapsed: .milliseconds(tenths * 100))
      XCTAssertGreaterThanOrEqual(sample.fraction, previous, "elapsed \(tenths) tenths")
      XCTAssertGreaterThanOrEqual(sample.fraction, transcribing.fraction)
      XCTAssertLessThan(sample.fraction, transcribing.nextFraction)
      previous = sample.fraction
    }
  }

  /// A next event less than one percent away: the bar stays on the event's
  /// fraction rather than moving backwards to `nextFraction - 0.01`.
  func testFractionNeverDropsBelowTheEventWhenTheGapIsUnderOnePercent() {
    let narrow = ProcessingProgress(
      stage: .merge, fraction: 0.5, nextFraction: 0.505, estimatedRemaining: .seconds(20),
      isEstimateSeeded: false)
    for seconds in [0, 1, 5, 60] {
      XCTAssertEqual(state(narrow, elapsed: .seconds(seconds)).fraction, 0.5, "\(seconds) s")
    }
  }

  /// A whole run as one event so `expectedTimeToNextEvent` equals the
  /// estimate: the table maps the remaining time, estimate less elapsed, to
  /// its text, with the seeded wording when any rate is still a seed. The
  /// sample is independent of Reduce Motion by design;
  /// `testReduceMotionKeepsTheSamplesAndDropsTheTween` is that axis's proof.
  func testWordingTableWithSeededAndLearnedEstimates() {
    func text(remaining: Int, elapsed: Int = 0, seeded: Bool) -> String {
      let progress = ProcessingProgress(
        stage: .cleanup, fraction: 0, nextFraction: 1, estimatedRemaining: .seconds(remaining),
        isEstimateSeeded: seeded)
      return state(progress, elapsed: .seconds(elapsed)).remainingText
    }
    // Learned rates.
    XCTAssertEqual(text(remaining: 5, seeded: false), "a few seconds")
    XCTAssertEqual(text(remaining: 9, seeded: false), "a few seconds")
    XCTAssertEqual(
      text(remaining: 10, seeded: false), "less than a minute")
    XCTAssertEqual(
      text(remaining: 59, seeded: false), "less than a minute")
    XCTAssertEqual(
      text(remaining: 60, seeded: false), "~1 min remaining")
    XCTAssertEqual(
      text(remaining: 61, seeded: false), "~2 min remaining",
      "minutes round up")
    XCTAssertEqual(
      text(remaining: 150, seeded: false), "~3 min remaining")
    // Seeded rates: softer, never a seconds count.
    XCTAssertEqual(text(remaining: 5, seeded: true), "about a minute")
    XCTAssertEqual(
      text(remaining: 89, seeded: true), "about a minute")
    XCTAssertEqual(text(remaining: 90, seeded: true), "about 2 min")
    XCTAssertEqual(text(remaining: 150, seeded: true), "about 3 min")
    // The count-down: the estimate less the elapsed time, floored at zero.
    XCTAssertEqual(
      text(remaining: 150, elapsed: 100, seeded: false),
      "less than a minute")
    XCTAssertEqual(
      text(remaining: 150, elapsed: 145, seeded: false),
      "a few seconds")
    XCTAssertEqual(
      text(remaining: 150, elapsed: 160, seeded: false),
      "a few seconds", "past the estimate but not yet 1.5x")
    XCTAssertEqual(
      text(remaining: 150, elapsed: 145, seeded: true),
      "about a minute")
    // Slow wins over everything else, seeded or not.
    XCTAssertEqual(
      text(remaining: 150, elapsed: 226, seeded: false),
      "a bit longer than usual")
    XCTAssertEqual(
      text(remaining: 150, elapsed: 226, seeded: true),
      "a bit longer than usual")
  }

  /// An event whose next event is expected at once, `fraction ==
  /// nextFraction` or nothing left to do: the bar sits at its target from
  /// the first sample and, since nothing was expected, any positive elapsed
  /// time reads as slow. Core posts such an event only when every rate was
  /// learned at zero.
  func testAZeroExpectedTimeToNextEventPutsTheBarAtItsTargetAndReadsSlowAtOnce() {
    let boundary = ProcessingProgress(
      stage: .diarize, fraction: 0.4, nextFraction: 0.4, estimatedRemaining: .seconds(30),
      isEstimateSeeded: false)
    XCTAssertEqual(boundary.expectedTimeToNextEvent, .zero)
    let first = state(boundary, elapsed: .zero)
    XCTAssertEqual(first.fraction, 0.4, "never below the event's fraction")
    XCTAssertFalse(first.isSlow)
    XCTAssertEqual(first.remainingText, "less than a minute")
    let second = state(boundary, elapsed: .seconds(1))
    XCTAssertEqual(second.fraction, 0.4)
    XCTAssertTrue(second.isSlow)
    XCTAssertEqual(second.remainingText, "a bit longer than usual")

    let nothingLeft = ProcessingProgress(
      stage: .summarize, fraction: 0, nextFraction: 1, estimatedRemaining: .zero,
      isEstimateSeeded: false)
    XCTAssertEqual(nothingLeft.expectedTimeToNextEvent, .zero)
    XCTAssertEqual(state(nothingLeft, elapsed: .zero).fraction, 0.99, "the target at once")
    XCTAssertEqual(state(nothingLeft, elapsed: .zero).remainingText, "a few seconds")
    XCTAssertEqual(
      state(nothingLeft, elapsed: .seconds(1)).remainingText, "a bit longer than usual")
  }

  /// Reduce Motion changes the tween between samples, not the sample.
  func testReduceMotionKeepsTheSamplesAndDropsTheTween() {
    for seconds in [0, 15, 60, 120] {
      XCTAssertEqual(
        state(transcribing, elapsed: .seconds(seconds), reduceMotion: true),
        state(transcribing, elapsed: .seconds(seconds), reduceMotion: false), "\(seconds) s")
    }
    XCTAssertNil(ProcessingPresentation.tween(reduceMotion: true))
    XCTAssertNotNil(ProcessingPresentation.tween(reduceMotion: false))
  }

  /// The lines `TabText` renders for a tab without content: the title, then
  /// the remaining text once an event exists.
  func testLinesAreTheTitleRow() {
    let waiting = ProcessingProgressModel.Entry(meetingID: UUID(), since: TestSupport.now)
    XCTAssertEqual(
      ProcessingPresentation.lines(entry: waiting, elapsed: .zero), ["Waiting to process"])
    let running = ProcessingProgressModel.Entry(
      meetingID: UUID(), progress: transcribing, since: TestSupport.now)
    XCTAssertEqual(
      ProcessingPresentation.lines(entry: running, elapsed: .zero),
      ["Transcribing…", "~3 min remaining"])
    XCTAssertEqual(
      ProcessingPresentation.lines(entry: running, elapsed: .seconds(120)),
      ["Transcribing…", "a bit longer than usual"])
  }
}
