import Foundation

/// One rendered file of the meeting folder; `fileName` is relative to it.
public struct RenderedArtifact: Sendable, Equatable {
  public enum Kind: String, Sendable, Equatable {
    case json, folderNote, transcript, tasks, vtt
  }

  public var kind: Kind
  public var fileName: String
  public var data: Data

  public init(kind: Kind, fileName: String, data: Data) {
    self.kind = kind
    self.fileName = fileName
    self.data = data
  }
}

/// One person's page for this meeting; `fileName` is relative to the
/// destination's people folder. `page` is the page as created from scratch,
/// `line` the one line the destination merges into the managed block of a
/// page that already exists.
public struct PersonPage: Sendable, Equatable {
  public var fileName: String
  public var page: String
  public var line: String

  public init(fileName: String, page: String, line: String) {
    self.fileName = fileName
    self.page = page
    self.line = line
  }
}
