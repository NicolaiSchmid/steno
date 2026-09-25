import Foundation
import StenoCore

/// The scope's fixed meeting folder and the one spelling of every file in it:
///
/// ```
/// <root>/Meetings/<yyyy-MM-dd>-<slug>/
///   <yyyy-MM-dd>-<slug>.md                folder note
///   <yyyy-MM-dd>-<slug> - Transcript.md
///   <yyyy-MM-dd>-<slug> - Tasks.md
///   transcript.vtt
///   meeting.json
///   audio.<ext>                           only with includeAudio
/// ```
///
/// The date is `startedAt` in the given time zone; the folder's basename is
/// the slug every note inside it is named after. Renderers name notes and
/// links through it, destinations place files by it; the "never delete"
/// tests in `ObsidianDestinationIntegrationTests` are written against these
/// names.
public enum MeetingFolder {
  public static let root = "Meetings"

  /// `"2026-09-24-produktstrategie-90-10-roadmap-fuer-q4"`.
  public static func basename(for meeting: Meeting, timeZone: TimeZone) -> String {
    "\(DateText.day(meeting.startedAt, in: timeZone))-\(Slug.title(meeting.title))"
  }

  /// `"Meetings/2026-09-24-produktstrategie-90-10-roadmap-fuer-q4"`.
  public static func path(for meeting: Meeting, timeZone: TimeZone) -> String {
    "\(root)/\(basename(for: meeting, timeZone: timeZone))"
  }

  /// The three notes in a meeting folder.
  public enum Note: Sendable, CaseIterable {
    case folder, transcript, tasks
  }

  /// The note's name without extension, which is what a wikilink targets:
  /// `"<slug>"`, `"<slug> - Transcript"`, `"<slug> - Tasks"`.
  public static func noteName(_ note: Note, slug: String) -> String {
    switch note {
    case .folder: slug
    case .transcript: "\(slug) - Transcript"
    case .tasks: "\(slug) - Tasks"
    }
  }

  /// `noteName` with `.md`.
  public static func noteFile(_ note: Note, slug: String) -> String {
    "\(noteName(note, slug: slug)).md"
  }

  public static let vtt = "transcript.vtt"
  public static let json = "meeting.json"

  /// `"audio.m4a"` for the AAC mixdown; the extension follows the mixdown
  /// file so the bytes and the name never disagree.
  public static func audioFile(fileExtension: String) -> String {
    fileExtension.isEmpty ? "audio" : "audio.\(fileExtension)"
  }
}
