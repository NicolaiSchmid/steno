import Foundation
import Testing

@testable import StenoAdapters

@Suite struct TimecodeTests {
  @Test func clockFloorsToWholeSeconds() {
    #expect(Timecode.clock(0) == "00:00:00")
    #expect(Timecode.clock(754.9) == "00:12:34")
    #expect(Timecode.clock(3661) == "01:01:01")
    #expect(Timecode.clock(36_000 * 3) == "30:00:00", "hours are not capped at two digits' worth")
    #expect(Timecode.clock(-5) == "00:00:00")
    #expect(Timecode.clock(.nan) == "00:00:00")
  }

  @Test func vttRoundsToMilliseconds() {
    #expect(Timecode.vtt(0) == "00:00:00.000")
    #expect(Timecode.vtt(754.567) == "00:12:34.567")
    #expect(Timecode.vtt(0.0005) == "00:00:00.001")
    #expect(Timecode.vtt(59.9996) == "00:01:00.000")
    #expect(Timecode.vtt(4.2) == "00:00:04.200")
  }

  @Test func padsToTheRequestedWidth() {
    #expect(Timecode.pad(7) == "07")
    #expect(Timecode.pad(123) == "123")
    #expect(Timecode.pad(5, width: 3) == "005")
  }
}
