import Foundation

/// The one spelling of every file the Obsidian destination writes:
///
/// ```
/// <vault>/Meetings/<yyyy-MM-dd>-<slug>/
///   <yyyy-MM-dd>-<slug>.md                folder note
///   <yyyy-MM-dd>-<slug> - Transcript.md
///   <yyyy-MM-dd>-<slug> - Tasks.md
///   transcript.vtt
///   meeting.json
///   audio.<ext>                           only with includeAudio
/// <vault>/<peopleFolder>/<Display Name>.md  managed block only
/// ```
///
/// Renderers name their artefacts through it and the destination places
/// them; the "never delete" tests in `ObsidianDestinationIntegrationTests`
/// are written against these names.
enum ObsidianLayout {
  static func folderNote(slug: String) -> String { "\(slug).md" }
  static func transcriptNote(slug: String) -> String { "\(slug) - Transcript.md" }
  static func tasksNote(slug: String) -> String { "\(slug) - Tasks.md" }
  static let vtt = "transcript.vtt"
  static let json = "meeting.json"

  /// `"audio.m4a"` for the AAC mixdown; the extension follows the mixdown
  /// file so the bytes and the name never disagree.
  static func audio(fileExtension: String) -> String {
    fileExtension.isEmpty ? "audio" : "audio.\(fileExtension)"
  }

  /// `"Anna Müller.md"`: the display name is the file name so `[[Anna
  /// Müller]]` resolves.
  static func personPage(displayName: String) -> String {
    "\(Slug.fileName(displayName)).md"
  }
}
