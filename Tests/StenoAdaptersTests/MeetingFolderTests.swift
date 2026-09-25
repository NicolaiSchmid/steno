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
      MeetingFolder.basename(for: meeting, timeZone: .gmt)
        == "2026-09-24-produktstrategie-90-10-roadmap-fuer-q4")
  }

  @Test func theDateFollowsTheTimeZoneAcrossMidnight() {
    var meeting = FixtureMeeting.meeting()
    meeting.startedAt = Date(timeIntervalSince1970: 1_790_290_800)  // 2026-09-24T23:00:00Z
    meeting.title = "Late"
    #expect(
      MeetingFolder.path(for: meeting, timeZone: .gmt) == "Meetings/2026-09-24-late")
    #expect(
      MeetingFolder.path(for: meeting, timeZone: FixtureMeeting.berlin)
        == "Meetings/2026-09-25-late")
  }

  @Test func noteNamesAreTheWikilinkTargetsAndFilesAddTheExtension() {
    let slug = FixtureMeeting.folderSlug
    #expect(MeetingFolder.noteName(.folder, slug: slug) == slug)
    #expect(MeetingFolder.noteName(.transcript, slug: slug) == "\(slug) - Transcript")
    #expect(MeetingFolder.noteName(.tasks, slug: slug) == "\(slug) - Tasks")
    for note in MeetingFolder.Note.allCases {
      #expect(
        MeetingFolder.noteFile(note, slug: slug) == MeetingFolder.noteName(note, slug: slug) + ".md"
      )
    }
    #expect(MeetingFolder.vtt == "transcript.vtt")
    #expect(MeetingFolder.json == "meeting.json")
    #expect(MeetingFolder.audioFile(fileExtension: "m4a") == "audio.m4a")
    #expect(MeetingFolder.audioFile(fileExtension: "") == "audio")
  }

  @Test func untitledMeetingsStillGetAFolder() {
    var meeting = FixtureMeeting.meeting()
    meeting.title = "???"
    #expect(
      MeetingFolder.basename(for: meeting, timeZone: .gmt) == "2026-09-24-meeting")
  }

  @Test func theDateFollowsStandardTimeInWinter() {
    var meeting = FixtureMeeting.meeting()
    meeting.startedAt = Date(timeIntervalSince1970: 1_796_166_000)  // 2026-12-01T23:00:00Z
    meeting.title = "Winter"
    #expect(MeetingFolder.path(for: meeting, timeZone: .gmt) == "Meetings/2026-12-01-winter")
    #expect(
      MeetingFolder.path(for: meeting, timeZone: FixtureMeeting.berlin)
        == "Meetings/2026-12-02-winter", "CET is one hour ahead, not two")
  }
}
