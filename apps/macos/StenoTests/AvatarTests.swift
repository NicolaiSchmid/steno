import XCTest

final class AvatarTests: XCTestCase {
  func testInitialsAreTheFirstLettersOfTheFirstTwoWords() {
    XCTAssertEqual(Avatar.initials(of: "Jan Herold"), "JH")
    XCTAssertEqual(Avatar.initials(of: "Nicolai"), "N")
    XCTAssertEqual(Avatar.initials(of: "  isabel  vennemann  "), "IV")
    XCTAssertEqual(Avatar.initials(of: "Philipp Schröder-Meyer Ost"), "PS")
    XCTAssertEqual(Avatar.initials(of: "élodie"), "É")
    XCTAssertNil(Avatar.initials(of: "   "))
    XCTAssertNil(Avatar.initials(of: ""))
  }
}
