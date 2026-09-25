import Foundation
import StenoCore

/// Speaker and person names as the renderers write them: the current display
/// name from the export, wikilinked only when the options link people and
/// the name belongs to a `Person` (who has a page), never for a cluster
/// label or a participant the app knows by name alone.
struct Names {
  let export: MeetingExport
  let options: RenderOptions

  /// The display name of a segment's speaker; `"Unknown"` for none.
  func speaker(_ speakerID: UUID?) -> String {
    guard let speakerID else { return "Unknown" }
    return export.displayName(forSpeaker: speakerID)
  }

  /// The speaker's name, linked when it resolves to a person.
  func linkedSpeaker(_ speakerID: UUID?) -> String {
    let name = speaker(speakerID)
    let isPerson = speakerID.flatMap { export.speaker(id: $0)?.personID } != nil
    return isPerson ? person(name) : name
  }

  /// A person's name, linked when the options link people.
  func person(_ displayName: String) -> String {
    options.linksPeople ? MarkdownText.wikilink(displayName) : displayName
  }

  /// A task's assignee, linked when it resolves to a person: through
  /// `assigneePersonID`, or through a speaker whose cluster label the model
  /// used as the name (it sees labels, as the summary does), else the name
  /// the model wrote, plain.
  func assignee(_ task: MeetingTask) -> String? {
    if let personID = task.assigneePersonID, let person = export.person(id: personID) {
      return self.person(person.displayName)
    }
    guard let name = task.assigneeName, !name.isEmpty else { return nil }
    if let speaker = export.speakers.first(where: { $0.clusterLabel == name }),
      speaker.personID != nil
    {
      return person(export.displayName(forSpeaker: speaker.id))
    }
    return name
  }

  /// Everybody in the meeting, once each: participants in export order
  /// (linked when they are a person), then speakers that resolved to nobody
  /// under their cluster label.
  func participants() -> [String] {
    var seen = Set<String>()
    var names: [String] = []
    for participant in export.participants where seen.insert(participant.displayName).inserted {
      names.append(
        participant.personID == nil ? participant.displayName : person(participant.displayName))
    }
    for speaker in export.speakers {
      let name = export.displayName(forSpeaker: speaker.id)
      guard seen.insert(name).inserted else { continue }
      names.append(speaker.personID == nil ? name : person(name))
    }
    return names
  }
}
