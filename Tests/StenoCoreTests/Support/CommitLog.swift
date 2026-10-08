import Foundation
import GRDB
import Synchronization

@testable import StenoCore

/// The statements of a store's writer folded into its commits, from the moment
/// it is installed: the `synchronous` level each commit ran under and the
/// tables it wrote. The levels come from the `PRAGMA synchronous` statements
/// the writer runs, starting from the level it had when the log was installed.
/// Rust: `Store::probe_commits`.
final class CommitLog: Sendable {
  struct Commit: Equatable, Sendable {
    /// 1 is `NORMAL`, 2 is `FULL`.
    var synchronous: Int
    var tables: Set<String>
  }

  private struct State {
    var synchronous: Int
    var tables: Set<String> = []
    var commits: [Commit] = []
  }

  private static let tables = ["handoverReceipt", "meeting", "audioAsset", "pairedDevice"]
  private let state: Mutex<State>
  private let committing: @Sendable () -> Void

  private init(synchronous: Int, committing: @escaping @Sendable () -> Void) {
    state = Mutex(State(synchronous: synchronous))
    self.committing = committing
  }

  /// A log of `store`'s writer, which must be a `DatabasePool`'s: a queue's
  /// reads would land in it too. `committing` runs as each `COMMIT`
  /// statement starts, before it commits.
  static func install(
    on store: MeetingStore, committing: @escaping @Sendable () -> Void = {}
  ) async throws -> CommitLog {
    try await store.writer.writeWithoutTransaction { db in
      let log = CommitLog(
        synchronous: try Int.fetchOne(db, sql: "PRAGMA synchronous") ?? -1,
        committing: committing)
      db.trace { event in
        guard case .statement(let statement) = event else { return }
        log.observe(statement.sql)
      }
      return log
    }
  }

  /// `PRAGMA synchronous` on `store`'s writer now.
  static func synchronous(of store: MeetingStore) async throws -> Int? {
    try await store.writer.writeWithoutTransaction { db in
      try Int.fetchOne(db, sql: "PRAGMA synchronous")
    }
  }

  var commits: [Commit] { state.withLock { $0.commits } }

  private func observe(_ sql: String) {
    if sql.hasPrefix("COMMIT") { committing() }
    state.withLock { state in
      if let level = Self.level(set: sql) {
        state.synchronous = level
      } else if sql.hasPrefix("INSERT") || sql.hasPrefix("UPDATE") || sql.hasPrefix("DELETE") {
        for table in Self.tables where sql.contains("\"\(table)\"") {
          state.tables.insert(table)
        }
      } else if sql.hasPrefix("COMMIT") {
        state.commits.append(Commit(synchronous: state.synchronous, tables: state.tables))
        state.tables = []
      } else if sql.hasPrefix("ROLLBACK") {
        state.tables = []
      }
    }
  }

  /// The level a `PRAGMA synchronous = …` statement sets, nil for any
  /// other statement.
  private static func level(set sql: String) -> Int? {
    let prefix = "PRAGMA synchronous = "
    guard sql.hasPrefix(prefix) else { return nil }
    switch sql.dropFirst(prefix.count) {
    case "OFF", "0": return 0
    case "NORMAL", "1": return 1
    case "FULL", "2": return 2
    case "EXTRA", "3": return 3
    default: return nil
    }
  }
}
