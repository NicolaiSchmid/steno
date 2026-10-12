import Foundation
import StenoCore

#if canImport(Darwin)
  import Darwin
#else
  import Glibc
#endif

/// The handover identity's fingerprint (SHA-256 of the leaf DER, lowercase
/// hex) in `swift-handover-identity.json` under the support directory, as
/// `{"steno.handoverIdentityFingerprint": "<hex>"}`, replaced atomically and
/// durably. Outside the keychain, which may be the thing that lost the
/// identity, and outside the settings. Its own file, never the Rust app's
/// `handover-identity.json`: a desktop build that once minted its own
/// identity records that one there, and this app's phones know this app's.
/// Rust: `FingerprintFile` in `crates/steno-services/src/handover.rs`.
public struct IdentityRecordFile: Sendable {
  /// The key the fingerprint is filed under.
  public static let key = "steno.handoverIdentityFingerprint"
  public let url: URL

  public init(supportDirectory: URL) {
    url = supportDirectory.appendingPathComponent("swift-handover-identity.json")
  }

  /// The recorded fingerprint, nil when the file does not exist. A file
  /// that does not parse is an error, never "nothing recorded".
  public func recorded() throws -> String? {
    guard FileManager.default.fileExists(atPath: url.path) else { return nil }
    return try JSONDecoder().decode([String: String].self, from: Data(contentsOf: url))[Self.key]
  }

  /// Records `fingerprint` over whatever was recorded: a temporary file
  /// beside it, synced, renamed over it, then the folder synced.
  public func record(_ fingerprint: String) throws {
    let folder = url.deletingLastPathComponent()
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
    let temporary = folder.appendingPathComponent(".\(url.lastPathComponent).\(UUID().uuidString)")
    do {
      try encoder.encode([Self.key: fingerprint]).write(to: temporary)
      try RecordingIntake.Syncs.fullSync(temporary)
      guard rename(temporary.path, url.path) == 0 else {
        throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
      }
    } catch {
      try? FileManager.default.removeItem(at: temporary)
      throw error
    }
    try? RecordingIntake.Syncs.fullSync(folder)
  }

  /// Forgets the record; a missing file is not an error.
  public func forget() throws {
    guard FileManager.default.fileExists(atPath: url.path) else { return }
    try FileManager.default.removeItem(at: url)
    try? RecordingIntake.Syncs.fullSync(url.deletingLastPathComponent())
  }
}
