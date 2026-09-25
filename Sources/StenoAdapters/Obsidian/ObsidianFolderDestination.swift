import Foundation
import StenoCore

/// What can go wrong in the vault. The app shows `description` verbatim.
public enum ObsidianError: Error, Sendable, Equatable, CustomStringConvertible {
  case vaultMissing(String)
  case vaultNotWritable(String)
  case peopleFolderInvalid(String)
  case writeFailed(path: String, underlying: String)
  case audioUnavailable

  public var description: String {
    switch self {
    case .vaultMissing(let path): "The Obsidian vault at \(path) does not exist."
    case .vaultNotWritable(let path): "The Obsidian vault at \(path) is not writable."
    case .peopleFolderInvalid(let folder):
      "The people folder \"\(folder)\" must be a relative path inside the vault."
    case .writeFailed(let path, let underlying): "Could not write \(path): \(underlying)"
    case .audioUnavailable:
      "The meeting has no audio mixdown to copy; every other file was written."
    }
  }
}

/// The Obsidian vault folder destination: `Meetings/<date>-<slug>/` with the
/// folder note, transcript, tasks, `transcript.vtt`, `meeting.json`, the
/// optional audio copy, and one managed block per person page. The folder is
/// pinned by the previous receipt on re-export; only files the app wrote
/// (tracked in the receipt) or freshly rendered are ever opened for writing;
/// nothing is deleted but the writer's own temp files.
public struct ObsidianFolderDestination: Destination {
  public static let destinationID = "obsidian-folder"

  public let settings: ObsidianSettings
  /// Time zone of the folder date and every date in the notes. `.current`
  /// in the app; tests pin it.
  public let timeZone: TimeZone
  let sink: LocalFolderSink

  public init(settings: ObsidianSettings, timeZone: TimeZone = .current) {
    self.settings = settings
    self.timeZone = timeZone
    self.sink = LocalFolderSink(root: URL(fileURLWithPath: settings.vaultPath, isDirectory: true))
  }

  public var id: String { Self.destinationID }

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
    let folder = try resolveFolder(for: meeting, previous: previous)
    let slug = URL(fileURLWithPath: folder).lastPathComponent
    let options = RenderOptions(
      linkStyle: .wikilink, peopleFolder: settings.peopleFolder, taskTag: settings.taskTag,
      timeZone: timeZone)
    let renderer = ArtifactRenderer()
    let artifacts = try renderer.render(meeting, options: options, folderSlug: slug)

    try wrapping(folder) { try sink.createDirectory(folder) }
    AtomicFileWriter.removeStaleTemporaries(in: sink.url(folder))
    if let peopleFolder = settings.peopleFolder {
      try wrapping(peopleFolder) { try sink.createDirectory(peopleFolder) }
      AtomicFileWriter.removeStaleTemporaries(in: sink.url(peopleFolder))
    }

    // Files from the previous receipt stay listed unless rewritten below, so
    // an opted-out audio copy or a disabled people folder keeps its entry.
    var files: [String: DeliveredFile] = [:]
    if let previous, previous.root == settings.vaultPath {
      for file in previous.files { files[file.relativePath] = file }
    }
    let owned = Set(files.values.filter { $0.ownership == .owned }.map(\.relativePath))

    // Freshly rendered paths are written on first delivery and, on
    // re-export, when the receipt lists them or nothing is there yet. A file
    // the app never wrote is never opened for writing.
    func writeOwned(_ path: String, _ data: Data) throws {
      guard previous == nil || owned.contains(path) || !sink.exists(path) else { return }
      try wrapping(path) { try sink.write(data, to: path) }
      files[path] = DeliveredFile(
        relativePath: path, ownership: .owned, sha256: ContentHash.sha256(data))
    }

    let personLine = renderer.renderPersonLine(meeting, folderSlug: slug, options: options)
    for artifact in artifacts {
      guard artifact.kind == .personPage else {
        try writeOwned("\(folder)/\(artifact.fileName)", artifact.data)
        continue
      }
      guard let peopleFolder = settings.peopleFolder else { continue }
      let path = "\(peopleFolder)/\(artifact.fileName)"
      var data = artifact.data
      if let existing = try wrapping(path, { try sink.read(path) }) {
        let merged = ManagedBlock.merge(
          personLine, meetingID: meeting.meeting.id,
          into: String(decoding: existing, as: UTF8.self))
        data = Data(merged.utf8)
      }
      try wrapping(path) { try sink.write(data, to: path) }
      files[path] = DeliveredFile(
        relativePath: path, ownership: .managedBlock, sha256: ContentHash.sha256(data))
    }

    if settings.includeAudio {
      guard let mixdown = meeting.audio?.mixdownURL, let data = try? Data(contentsOf: mixdown)
      else { throw ObsidianError.audioUnavailable }
      try writeOwned(
        "\(folder)/\(ObsidianLayout.audio(fileExtension: mixdown.pathExtension))", data)
    }

    return DeliveryReceipt(
      root: settings.vaultPath,
      folder: folder,
      files: files.values.sorted { $0.relativePath < $1.relativePath },
      rendererVersion: ArtifactRenderer.version)
  }

  // MARK: - Rules

  /// The receipt's folder on re-export. On first delivery the scope's path,
  /// with `-2`, `-3`, … appended while the folder exists and holds another
  /// meeting's `meeting.json` (or none); a folder holding our own id is a
  /// crashed attempt and is reused.
  func resolveFolder(for meeting: MeetingExport, previous: DeliveryReceipt?) throws -> String {
    if let previous { return previous.folder }
    let base = MeetingFolder.path(for: meeting.meeting, timeZone: timeZone)
    var candidate = base
    var suffix = 2
    while sink.exists(candidate) {
      if let data = try? sink.read("\(candidate)/\(ObsidianLayout.json)"),
        let probe = try? StenoJSON.decode(MeetingIDProbe.self, from: data),
        probe.meeting.id == meeting.meeting.id
      {
        return candidate
      }
      candidate = "\(base)-\(suffix)"
      suffix += 1
    }
    return candidate
  }

  /// The vault exists and the people folder, if any, is a relative path
  /// without `..`, `\` or empty components.
  func checkVault() throws {
    guard sink.isDirectory("") else { throw ObsidianError.vaultMissing(settings.vaultPath) }
    guard let folder = settings.peopleFolder else { return }
    let trimmed = folder.trimmingCharacters(in: .whitespaces)
    let components = trimmed.split(separator: "/", omittingEmptySubsequences: false)
    guard !trimmed.isEmpty, !trimmed.hasPrefix("/"), !trimmed.contains("\\"),
      !components.contains(where: { $0 == ".." || $0.isEmpty })
    else { throw ObsidianError.peopleFolderInvalid(folder) }
  }

  private func wrapping<T>(_ path: String, _ body: () throws -> T) throws -> T {
    do {
      return try body()
    } catch let failure as AtomicFileWriter.Failure {
      throw ObsidianError.writeFailed(path: failure.path, underlying: failure.underlying)
    } catch {
      throw ObsidianError.writeFailed(
        path: sink.url(path).path, underlying: String(describing: error))
    }
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
