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
/// identity recorded that one there, which is not the identity this app's
/// phones know. Written with the Rust app's bytes (serde's pretty JSON).
/// Rust: `FingerprintFile` in `crates/steno-services/src/handover.rs`.
public struct IdentityRecordFile: Sendable {
  /// The key the fingerprint is filed under.
  public static let key = "steno.handoverIdentityFingerprint"
  public let url: URL

  public init(supportDirectory: URL) {
    url = supportDirectory.appendingPathComponent("swift-handover-identity.json")
  }

  /// A record that exists but cannot be read or parsed. Its description is
  /// the menu bar's reason; `underlying` says why.
  public struct Unreadable: Error, CustomStringConvertible {
    public let underlying: any Error
    public var description: String { "this Mac's record of its phone pairing could not be read" }
  }

  /// The recorded fingerprint, nil when the file does not exist. A file
  /// that does not parse is `Unreadable`, never "nothing recorded".
  public func recorded() throws -> String? {
    guard FileManager.default.fileExists(atPath: url.path) else { return nil }
    do {
      return try JSONDecoder().decode([String: String].self, from: Data(contentsOf: url))[Self.key]
    } catch {
      throw Unreadable(underlying: error)
    }
  }

  /// Records `fingerprint` over whatever was recorded: a temporary file
  /// beside it, synced, renamed over it, then the folder synced.
  public func record(_ fingerprint: String) throws {
    let folder = url.deletingLastPathComponent()
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    // Lowercase hex needs no escaping.
    let bytes = Data("{\n  \"\(Self.key)\": \"\(fingerprint)\"\n}".utf8)
    let temporary = folder.appendingPathComponent(".\(url.lastPathComponent).\(UUID().uuidString)")
    do {
      try bytes.write(to: temporary)
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
}
