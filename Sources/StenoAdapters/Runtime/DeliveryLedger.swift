import Foundation
import StenoCore

/// The delivery policy with no I/O in it: which receipt applies to this
/// root, whether a path may be opened for writing, what has been written,
/// and the receipt that results. A destination renders, asks the ledger and
/// calls its sink; the rules live here once, so a second destination reuses
/// them untouched and the "files the app never wrote are never opened for
/// writing" rule cannot drift between transports.
struct DeliveryLedger {
  /// The destination root this delivery writes to (the vault path).
  let root: String
  /// The receipt that applies to `root`: nil on a first delivery, which is
  /// also what a receipt from another root becomes. A moved vault or a
  /// scratch run does not pin a folder or protect a file here.
  let previous: DeliveryReceipt?
  private(set) var files: [String: DeliveredFile] = [:]
  private let owned: Set<String>

  init(previous: DeliveryReceipt?, root: String) {
    self.root = root
    self.previous = previous.flatMap { Self.sameRoot($0.root, root) ? $0 : nil }
    // Files from the previous receipt stay listed unless rewritten, so an
    // opted-out audio copy or a disabled people folder keeps its entry.
    for file in self.previous?.files ?? [] { files[file.relativePath] = file }
    owned = Set(files.values.filter { $0.ownership == .owned }.map(\.relativePath))
  }

  var isFirstDelivery: Bool { previous == nil }

  /// The folder the previous receipt pinned, when one applies.
  var pinnedFolder: String? { previous?.folder }

  /// Whether an owned path may be opened for writing: on first delivery
  /// always; on re-export when the receipt lists it as owned or nothing is
  /// there yet. A file the app never wrote is never opened for writing.
  func mayWrite(_ path: String, exists: Bool) -> Bool {
    isFirstDelivery || owned.contains(path) || !exists
  }

  mutating func record(_ path: String, _ ownership: FileOwnership, _ data: Data) {
    files[path] = DeliveredFile(
      relativePath: path, ownership: ownership, sha256: ContentHash.sha256(data))
  }

  /// Whether a listed file sits directly in `folder` with a name that
  /// satisfies `name`.
  func lists(in folder: String, where name: (String) -> Bool) -> Bool {
    files.keys.contains { path in
      guard path.hasPrefix("\(folder)/") else { return false }
      let child = String(path.dropFirst(folder.count + 1))
      return !child.contains("/") && name(child)
    }
  }

  func receipt(folder: String) -> DeliveryReceipt {
    DeliveryReceipt(
      root: root,
      folder: folder,
      files: files.values.sorted { $0.relativePath < $1.relativePath },
      rendererVersion: ArtifactRenderer.version)
  }

  // MARK: - Rules

  /// Two spellings of one root: trailing slashes, `.` and `..` components
  /// and, on macOS, a `/private` prefix are not a different root.
  static func sameRoot(_ lhs: String, _ rhs: String) -> Bool {
    func standardized(_ path: String) -> String {
      URL(fileURLWithPath: path, isDirectory: true).standardizedFileURL.path
    }
    return standardized(lhs) == standardized(rhs)
  }

  /// The folder of a first delivery: `base`, with `-2`, `-3`, … appended
  /// while the candidate exists and holds another meeting's `meeting.json`
  /// (or none); a candidate holding `meetingID` is a crashed attempt and is
  /// reused. `exists` and `meetingIn` are the transport's two lookups.
  static func resolveFolder(
    base: String, meetingID: UUID, exists: (String) -> Bool, meetingIn: (String) -> UUID?
  ) -> String {
    var candidate = base
    var suffix = 2
    while exists(candidate) {
      if meetingIn(candidate) == meetingID { return candidate }
      candidate = "\(base)-\(suffix)"
      suffix += 1
    }
    return candidate
  }
}

extension DeliveryReceipt {
  /// `root` joined with `folder`: the meeting folder to reveal in Finder.
  public var folderURL: URL {
    URL(fileURLWithPath: root, isDirectory: true).appendingPathComponent(folder, isDirectory: true)
  }
}
