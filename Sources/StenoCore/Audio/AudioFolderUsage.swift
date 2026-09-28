import Foundation

/// The disk cost of "Keep forever", for Settings > Audio: the logical size
/// of every regular file under the audio folder, recursively, hidden files
/// skipped. Logical (`.fileSizeKey`) rather than allocated size, which is
/// block-rounded on APFS and not reported by every file system.
public enum AudioFolderUsage {
  /// Throws when `folder` is missing, not a folder, or cannot be read. An
  /// entry inside it that cannot be read (permissions, removed by the sweep
  /// during the walk) is skipped, so one file never hides the whole size.
  public static func measure(_ folder: URL) throws -> Int64 {
    let manager = FileManager.default
    var isDirectory: ObjCBool = false
    guard manager.fileExists(atPath: folder.path, isDirectory: &isDirectory),
      isDirectory.boolValue
    else {
      throw CocoaError(.fileReadNoSuchFile, userInfo: [NSFilePathErrorKey: folder.path])
    }
    guard manager.isReadableFile(atPath: folder.path) else {
      throw CocoaError(.fileReadNoPermission, userInfo: [NSFilePathErrorKey: folder.path])
    }
    let keys: Set<URLResourceKey> = [.isRegularFileKey, .fileSizeKey]
    guard
      let enumerator = manager.enumerator(
        at: folder, includingPropertiesForKeys: Array(keys), options: [.skipsHiddenFiles])
    else {
      throw CocoaError(.fileReadUnknown, userInfo: [NSFilePathErrorKey: folder.path])
    }
    var total: Int64 = 0
    for case let url as URL in enumerator {
      guard let values = try? url.resourceValues(forKeys: keys), values.isRegularFile == true
      else { continue }
      total += Int64(values.fileSize ?? 0)
    }
    return total
  }
}
