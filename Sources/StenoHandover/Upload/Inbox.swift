import Foundation
import StenoCore

/// The directory where uploads live until `HandoverIntake` takes them:
///
///     <inbox>/<recordingID>.partial          chunks being received
///     <inbox>/<recordingID>.metadata.json    the announced `RecordingMetadata`
///     <inbox>/<recordingID>.<ext>            verified, waiting for the intake
///
/// The intake deletes the verified file once the meeting is enqueued; the
/// inbox removes the metadata. `sweep` removes what no receipt accounts for.
struct Inbox: Sendable {
  let directory: URL

  func partial(_ recordingID: UUID) -> URL {
    directory.appendingPathComponent("\(recordingID.uuidString.lowercased()).partial")
  }

  func metadata(_ recordingID: UUID) -> URL {
    directory.appendingPathComponent("\(recordingID.uuidString.lowercased()).metadata.json")
  }

  func verified(_ recordingID: UUID, format: AudioFormat) -> URL {
    directory.appendingPathComponent(
      "\(recordingID.uuidString.lowercased()).\(format.fileExtension)")
  }

  func prepare() throws {
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
  }

  /// Starts a recording: empty partial file and the metadata beside it.
  func begin(_ metadata: RecordingMetadata) throws {
    try prepare()
    try ReceivingFile.create(at: partial(metadata.recordingID))
    try StenoJSON.encode(metadata).write(to: self.metadata(metadata.recordingID), options: .atomic)
  }

  func loadMetadata(_ recordingID: UUID) -> RecordingMetadata? {
    guard let data = try? Data(contentsOf: metadata(recordingID)) else { return nil }
    return try? StenoJSON.decode(RecordingMetadata.self, from: data)
  }

  func hasPartial(_ recordingID: UUID) -> Bool {
    FileManager.default.fileExists(atPath: partial(recordingID).path)
  }

  func hasVerified(_ recordingID: UUID, format: AudioFormat) -> Bool {
    FileManager.default.fileExists(atPath: verified(recordingID, format: format).path)
  }

  /// Renames the complete partial to its final name.
  func promote(_ recordingID: UUID, format: AudioFormat) throws -> URL {
    let destination = verified(recordingID, format: format)
    if FileManager.default.fileExists(atPath: destination.path) {
      try FileManager.default.removeItem(at: destination)
    }
    try FileManager.default.moveItem(at: partial(recordingID), to: destination)
    return destination
  }

  /// Removes every file of the recording.
  func discard(_ recordingID: UUID, format: AudioFormat?) {
    var urls = [partial(recordingID), metadata(recordingID)]
    if let format {
      urls.append(verified(recordingID, format: format))
    } else {
      for candidate in AudioFormat.allCases {
        urls.append(verified(recordingID, format: candidate))
      }
    }
    for url in urls {
      try? FileManager.default.removeItem(at: url)
    }
  }

  /// Recording ids that have any file in the inbox.
  func recordingIDs() -> Set<UUID> {
    let names =
      (try? FileManager.default.contentsOfDirectory(atPath: directory.path)) ?? []
    var ids: Set<UUID> = []
    for name in names {
      let stem = name.split(separator: ".", maxSplits: 1).first.map(String.init) ?? name
      if let id = UUID(uuidString: stem) { ids.insert(id) }
    }
    return ids
  }
}
