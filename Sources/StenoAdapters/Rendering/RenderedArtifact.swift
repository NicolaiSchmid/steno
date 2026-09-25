import Foundation

/// One rendered file. `fileName` is relative to the meeting folder, except
/// for `.personPage`, whose file lives in the destination's people folder
/// and whose `data` is the page as it would be created from scratch; a
/// destination merges only the managed block into a page that exists.
public struct RenderedArtifact: Sendable, Equatable {
  public enum Kind: String, Sendable, Equatable {
    case folderNote, transcript, tasks, vtt, json, personPage, audio
  }

  public var kind: Kind
  public var fileName: String
  public var data: Data
  /// The person a `.personPage` belongs to.
  public var personID: UUID?

  public init(kind: Kind, fileName: String, data: Data, personID: UUID? = nil) {
    self.kind = kind
    self.fileName = fileName
    self.data = data
    self.personID = personID
  }
}
