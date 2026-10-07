import Foundation
import GRDB

/// Another connection's write lock on a database file, held from `init`
/// until `release()`: what a second process in the middle of a write
/// transaction looks like to the store. A checkpoint needs that lock, so
/// `MeetingStore.checkpointDurably()` fails while it is held.
public final class HeldWriteLock: Sendable {
  private let queue: DatabaseQueue

  public init(on url: URL) throws {
    var configuration = Configuration()
    configuration.allowsUnsafeTransactions = true
    queue = try DatabaseQueue(path: url.path, configuration: configuration)
    try queue.inDatabase { db in try db.execute(sql: "BEGIN IMMEDIATE") }
  }

  public func release() throws {
    try queue.inDatabase { db in try db.execute(sql: "ROLLBACK") }
  }
}

extension MeetingStore {
  /// An on-disk store at `url` whose `checkpointDurably()` throws
  /// `SQLITE_BUSY` until the returned lock is released: another connection
  /// holds the write lock, and the store does not wait on a busy lock
  /// (GRDB's default, where `onDisk` waits five seconds), so the test does
  /// not wait either.
  public static func withCheckpointBlocked(at url: URL) throws -> (MeetingStore, HeldWriteLock) {
    let store = try MeetingStore(writer: DatabasePool(path: url.path))
    return (store, try HeldWriteLock(on: url))
  }
}
