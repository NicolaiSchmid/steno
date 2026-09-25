import Foundation
import StenoCore

/// What can go wrong in the vault. The app shows `description` verbatim.
public enum ObsidianError: Error, Sendable, Equatable, CustomStringConvertible {
  case vaultMissing(String)
  case vaultNotWritable(String)
  case peopleFolderInvalid(String)
  case readFailed(path: String, underlying: String)
  case writeFailed(path: String, underlying: String)
  case audioUnavailable

  public var description: String {
    switch self {
    case .vaultMissing(let path): "The Obsidian vault at \(path) does not exist."
    case .vaultNotWritable(let path): "The Obsidian vault at \(path) is not writable."
    case .peopleFolderInvalid(let folder):
      "The people folder \"\(folder)\" must be a relative path inside the vault."
    case .readFailed(let path, let underlying): "Could not read \(path): \(underlying)"
    case .writeFailed(let path, let underlying): "Could not write \(path): \(underlying)"
    case .audioUnavailable:
      "The meeting has no audio mixdown to copy; every other file was written."
    }
  }
}

/// The Obsidian vault folder destination: `Meetings/<date>-<slug>/` with the
/// folder note, transcript, tasks, `transcript.vtt`, `meeting.json`, the
/// optional audio copy, and one managed block per person page. The policy
/// (which receipt applies, what may be written, what the receipt says) is
/// `DeliveryLedger`'s; this type renders, asks the ledger and writes through
/// `LocalFolderSink`. Nothing is deleted but the writer's own temp files.
public struct ObsidianFolderDestination: Destination {
  public static let destinationID = "obsidian-folder"

  /// `destinationID` for the app's stored settings. A one-off run into
  /// another vault (`steno deliver --vault`) passes its own id so its
  /// `Delivery` row and receipt never replace the stored destination's.
  public let id: String
  public let settings: ObsidianSettings
  /// Time zone of the folder date and every date in the notes. `.current`
  /// in the app; tests pin it.
  public let timeZone: TimeZone
  let sink: LocalFolderSink

  public init(
    settings: ObsidianSettings, timeZone: TimeZone = .current, id: String = Self.destinationID
  ) {
    self.id = id
    self.settings = settings
    self.timeZone = timeZone
    self.sink = LocalFolderSink(root: URL(fileURLWithPath: settings.vaultPath, isDirectory: true))
  }

  /// The vault is a writable directory (probed with a file that is created
  /// and removed) and the people folder is a relative path without `..`. A
  /// missing `.obsidian/` is not an error: the folder may be a vault Obsidian
  /// has not opened yet.
  public func validate() async throws {
    try checkVault()
    let probe = ".steno-probe-\(AtomicFileWriter.randomHex())"
    do {
      try Data().write(to: sink.url(probe))
      try FileManager.default.removeItem(at: sink.url(probe))
    } catch {
      throw ObsidianError.vaultNotWritable(settings.vaultPath)
    }
  }

  public func deliver(_ meeting: MeetingExport, previous: DeliveryReceipt?) async throws
    -> DeliveryReceipt
  {
    try checkVault()
    var ledger = DeliveryLedger(previous: previous, root: settings.vaultPath)
    let folder = ledger.pinnedFolder ?? resolveFolder(for: meeting)
    let slug = URL(fileURLWithPath: folder).lastPathComponent
    let options = RenderOptions(
      linkStyle: .wikilink, personPages: settings.peopleFolder != nil, taskTag: settings.taskTag,
      timeZone: timeZone)
    let renderer = ArtifactRenderer()

    try writing(folder) { try sink.createDirectory(folder) }
    AtomicFileWriter.removeStaleTemporaries(in: sink.url(folder))

    func writeOwned(_ path: String, _ data: Data) throws {
      guard ledger.mayWrite(path, exists: sink.exists(path)) else { return }
      try writing(path) { try sink.write(data, to: path) }
      ledger.record(path, .owned, data)
    }

    for artifact in try renderer.renderMeetingFiles(meeting, options: options, folderSlug: slug) {
      try writeOwned("\(folder)/\(artifact.fileName)", artifact.data)
    }

    if let peopleFolder = settings.peopleFolder {
      try writing(peopleFolder) { try sink.createDirectory(peopleFolder) }
      AtomicFileWriter.removeStaleTemporaries(in: sink.url(peopleFolder))
      for page in renderer.renderPersonPages(meeting, options: options, folderSlug: slug) {
        let path = "\(peopleFolder)/\(page.fileName)"
        var data = Data(page.page.utf8)
        if let existing = try reading(path, { try sink.read(path) }) {
          // A page that is not UTF-8 text cannot be merged without changing
          // bytes outside the block, so it is left as it is and reported.
          guard let text = String(data: existing, encoding: .utf8) else {
            throw ObsidianError.readFailed(
              path: sink.url(path).path,
              underlying: "not UTF-8 text; the page was left unchanged")
          }
          data = Data(ManagedBlock.merge(page.line, meetingID: meeting.meeting.id, into: text).utf8)
        }
        try writing(path) { try sink.write(data, to: path) }
        ledger.record(path, .managedBlock, data)
      }
    }

    if settings.includeAudio {
      // The mixdown is copied while it exists; after the retention sweep the
      // copy already in the vault (in the receipt or on disk) is the audio.
      // Only when neither is there has the meeting no audio to deliver.
      if let mixdown = meeting.audio?.mixdownURL,
        FileManager.default.fileExists(atPath: mixdown.path)
      {
        let data = try reading(mixdown.path) { try Data(contentsOf: mixdown) }
        try writeOwned(
          "\(folder)/\(MeetingFolder.audioFile(fileExtension: mixdown.pathExtension))", data)
      } else if !audioIsInTheVault(folder: folder, ledger: ledger) {
        throw ObsidianError.audioUnavailable
      }
    }

    return ledger.receipt(folder: folder)
  }

  // MARK: - Vault lookups

  /// The folder of a first delivery: the scope's path with the ledger's
  /// collision rule, fed by the two things only this transport knows.
  func resolveFolder(for meeting: MeetingExport) -> String {
    DeliveryLedger.resolveFolder(
      base: MeetingFolder.path(for: meeting.meeting, timeZone: timeZone),
      meetingID: meeting.meeting.id,
      exists: sink.exists,
      meetingIn: { folder in
        guard let data = try? sink.read("\(folder)/\(MeetingFolder.json)") else { return nil }
        return try? StenoJSON.decode(MeetingIDProbe.self, from: data).meeting.id
      })
  }

  /// An `audio` or `audio.<ext>` file in the meeting folder, listed in the
  /// receipt or present on disk.
  func audioIsInTheVault(folder: String, ledger: DeliveryLedger) -> Bool {
    func isAudio(_ name: String) -> Bool { name == "audio" || name.hasPrefix("audio.") }
    return ledger.lists(in: folder, where: isAudio)
      || sink.fileNames(in: folder).contains(where: isAudio)
  }

  /// The vault exists and the people folder, if any, is a relative path
  /// without `..`, `\`, empty components or surrounding whitespace.
  func checkVault() throws {
    guard sink.isDirectory("") else { throw ObsidianError.vaultMissing(settings.vaultPath) }
    guard let folder = settings.peopleFolder else { return }
    let components = folder.split(separator: "/", omittingEmptySubsequences: false)
    guard !folder.isEmpty, !folder.hasPrefix("/"), !folder.contains("\\"),
      folder == folder.trimmingCharacters(in: .whitespacesAndNewlines),
      !components.contains(where: { $0 == ".." || $0.isEmpty })
    else { throw ObsidianError.peopleFolderInvalid(folder) }
  }

  private func reading<T>(_ path: String, _ body: () throws -> T) throws -> T {
    do {
      return try body()
    } catch {
      throw ObsidianError.readFailed(path: absolute(path), underlying: String(describing: error))
    }
  }

  private func writing<T>(_ path: String, _ body: () throws -> T) throws -> T {
    do {
      return try body()
    } catch let failure as AtomicFileWriter.Failure {
      throw ObsidianError.writeFailed(path: failure.path, underlying: failure.underlying)
    } catch {
      throw ObsidianError.writeFailed(path: absolute(path), underlying: String(describing: error))
    }
  }

  /// `path` as the message names it: vault-relative paths become absolute,
  /// absolute ones (the mixdown) stay.
  private func absolute(_ path: String) -> String {
    path.hasPrefix("/") ? path : sink.url(path).path
  }

  /// Enough of `meeting.json` to read the meeting id, whatever else the
  /// schema holds.
  private struct MeetingIDProbe: Decodable {
    struct Meeting: Decodable {
      var id: UUID
    }
    var meeting: Meeting
  }
}
