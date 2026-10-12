import AppKit
import XCTest

/// The launch's Pair Again question: nothing pairs again unless the
/// question and then the destructive warning both confirm it.
@MainActor
final class PairAgainAlertTests: XCTestCase {
  func ask(answers: [Bool]) -> (Bool, [PairAgainAlert.Prompt]) {
    var asked: [PairAgainAlert.Prompt] = []
    let answer = PairAgainAlert.ask { prompt in
      asked.append(prompt)
      return answers[asked.count - 1]
    }
    return (answer, asked)
  }

  func testNotNowAsksNothingMore() {
    let (answer, asked) = ask(answers: [false])
    XCTAssertFalse(answer)
    XCTAssertEqual(asked, [PairAgainAlert.question])
  }

  func testTheWarningComesSecondAndCanCancel() {
    let (answer, asked) = ask(answers: [true, false])
    XCTAssertFalse(answer)
    XCTAssertEqual(asked, [PairAgainAlert.question, PairAgainAlert.warning])
    XCTAssertFalse(PairAgainAlert.question.destructive)
    XCTAssertTrue(PairAgainAlert.warning.destructive)
  }

  func testBothConfirmedPairsAgain() {
    let (answer, asked) = ask(answers: [true, true])
    XCTAssertTrue(answer)
    XCTAssertEqual(asked.count, 2)
  }

  /// Return confirms the question, but cancels the destructive warning.
  func testTheWarningsDefaultButtonIsCancel() {
    let question = PairAgainAlert.alert(for: PairAgainAlert.question).buttons
    XCTAssertEqual(question.map(\.title), ["Pair Again", "Not Now"])
    XCTAssertEqual(question.map(\.keyEquivalent), ["\r", "\u{1b}"])
    XCTAssertFalse(question[0].hasDestructiveAction)

    let warning = PairAgainAlert.alert(for: PairAgainAlert.warning).buttons
    XCTAssertEqual(warning.map(\.title), ["Pair Again", "Cancel"])
    XCTAssertEqual(warning.map(\.keyEquivalent), ["", "\r"])
    XCTAssertTrue(warning[0].hasDestructiveAction)
  }
}
