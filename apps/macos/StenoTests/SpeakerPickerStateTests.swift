import StenoCore
import XCTest

final class SpeakerPickerStateTests: XCTestCase {
  /// Speaker 2's options over the sample data: Jérôme ("Sounds like"), then
  /// Nicolai ("In this meeting").
  private var options: [SpeakerOptions.Option] {
    SpeakerOptions.build(
      speaker: SampleData.speakers()[1], speakers: SampleData.speakers(),
      persons: SampleData.persons(), participants: SampleData.participants(), suggestion: nil,
      recent: [], query: "")
  }

  private func personID(_ option: SpeakerOptions.Option?) -> UUID? {
    if case .person(let person) = option?.kind { return person.id }
    return nil
  }

  func testPrefilledReturnTakesTheSuggestedPerson() {
    var state = SpeakerPickerState()
    state.reset(prefill: "Jérôme")
    XCTAssertEqual(state.query, "Jérôme")
    XCTAssertNil(state.commit(), "nothing to take before the options arrive")
    state.setOptions(options)
    XCTAssertEqual(personID(state.commit()), SampleData.personJeromeID)
  }

  func testEmptyReturnIsANoOpUntilDownMoves() {
    var state = SpeakerPickerState()
    state.reset(prefill: nil)
    state.setOptions(options)
    XCTAssertNil(state.commit())
    state.move(.up)
    XCTAssertNil(state.commit(), "Up with nothing highlighted stays nowhere")
    state.move(.down)
    XCTAssertEqual(personID(state.commit()), SampleData.personJeromeID)
    state.move(.down)
    XCTAssertEqual(personID(state.commit()), SampleData.personNicolaiID)
    state.move(.down)
    XCTAssertEqual(personID(state.commit()), SampleData.personNicolaiID, "Down past the end stays")
    state.move(.up)
    state.move(.up)
    XCTAssertEqual(personID(state.commit()), SampleData.personJeromeID, "Up stops at the first")
  }

  func testTypingHighlightsTheFirstResultAndClearingResets() {
    var state = SpeakerPickerState()
    state.reset(prefill: "Jérôme")
    state.queryChanged("ni")
    XCTAssertFalse(state.isPrefilled)
    state.setOptions(options.filter { $0.displayName.localizedCaseInsensitiveContains("ni") })
    XCTAssertEqual(personID(state.commit()), SampleData.personNicolaiID)
    state.queryChanged("")
    state.setOptions(options)
    XCTAssertNil(state.commit(), "an emptied field commits nothing")
    state.setOptions([])
    state.move(.down)
    XCTAssertNil(state.commit())
  }

  func testHighlightClampsWhenTheOptionsShrink() {
    var state = SpeakerPickerState()
    state.reset(prefill: nil)
    state.setOptions(options)
    state.move(.down)
    state.move(.down)
    XCTAssertEqual(state.highlighted, 1)
    state.setOptions(Array(options.prefix(1)))
    XCTAssertEqual(state.highlighted, 0)
  }
}
