import Foundation
import StenoCore
import Testing

@testable import StenoAdapters

@Suite struct MeetingFolderTests {
  @Test func usesTheStartDateInTheGivenTimeZone() {
    let meeting = FixtureMeeting.meeting()
    #expect(
      MeetingFolder.path(for: meeting, timeZone: FixtureMeeting.berlin)
        == "Meetings/2026-09-24-produktstrategie-90-10-roadmap-fuer-q4")
    #expect(
      MeetingFolder.basename(for: meeting, timeZone: RenderOptions.utc)
        == "2026-09-24-produktstrategie-90-10-roadmap-fuer-q4")
  }

  @Test func theDateFollowsTheTimeZoneAcrossMidnight() {
    var meeting = FixtureMeeting.meeting()
    meeting.startedAt = Date(timeIntervalSince1970: 1_790_290_800)  // 2026-09-24T23:00:00Z
    meeting.title = "Late"
    #expect(
      MeetingFolder.path(for: meeting, timeZone: RenderOptions.utc) == "Meetings/2026-09-24-late")
    #expect(
      MeetingFolder.path(for: meeting, timeZone: FixtureMeeting.berlin)
        == "Meetings/2026-09-25-late")
  }

  @Test func untitledMeetingsStillGetAFolder() {
    var meeting = FixtureMeeting.meeting()
    meeting.title = "???"
    #expect(
      MeetingFolder.basename(for: meeting, timeZone: RenderOptions.utc) == "2026-09-24-meeting")
  }
}
