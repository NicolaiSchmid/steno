import Dispatch
import Foundation
import GRDB
import StenoCore
import Synchronization

/// An on-disk store that can hold a statement's transaction open: once
/// armed, a hold stops the next matching statement after it ran and before
/// its transaction ends, until `release()`. The test service's in-memory
/// store is one connection, where a held read would block the revoke; a WAL
/// pool gives each read its own connection, so writes (a revoke, a pairing)
/// commit meanwhile, and a read sees the last commit while a write is held.
final class StoreGate: Sendable {
  let store: MeetingStore
  /// The store's pool, for a test that changes the schema under it.
  let pool: DatabasePool
  /// The next handover receipt read; the engine resumes with a receipt that
  /// may no longer be in the store.
  let receiptRead = Hold(matching: "FROM \"handoverReceipt\"")
  /// The next save of an existing handover receipt, executed but not
  /// committed; every later write queues behind it.
  let receiptWrite = Hold(matching: "UPDATE \"handoverReceipt\"")
  /// The next paired device delete (a revoke), executed but not committed.
  let deviceDelete = Hold(matching: "DELETE FROM \"pairedDevice\"")
  /// The next save of an existing paired device (pairing again under its
  /// id), executed but not committed.
  let deviceSave = Hold(matching: "UPDATE \"pairedDevice\"")
  private let holds: [Hold]
  private let directory: URL

  init() throws {
    directory = try Fixtures.temporaryDirectory("store-gate")
    let holds = [receiptRead, receiptWrite, deviceDelete, deviceSave]
    self.holds = holds
    var configuration = Configuration()
    configuration.prepareDatabase { db in
      let connection = ObjectIdentifier(db)
      db.trace { event in
        guard case .statement(let statement) = event else { return }
        for hold in holds { hold.observe(statement.sql, on: connection) }
      }
    }
    pool = try DatabasePool(
      path: directory.appendingPathComponent("steno.sqlite").path,
      configuration: configuration)
    store = try MeetingStore(writer: pool)
  }

  /// Whether a hold went on by itself after `Hold.limit`: the test waited on
  /// something the held statement blocked.
  var timedOut: Bool { holds.contains { $0.timedOut } }

  /// Lets any held statement go on and deletes the database directory.
  func remove() {
    for hold in holds { hold.release() }
    try? FileManager.default.removeItem(at: directory)
  }

  /// One statement to hold. It is held at the `COMMIT` that ends its
  /// transaction on the same connection, after the statement ran.
  final class Hold: Sendable {
    /// A held statement goes on after this, so a test that waits on what it
    /// blocks fails instead of hanging the suite.
    static let limit: DispatchTimeInterval = .seconds(30)

    private enum Phase {
      case idle, armed
      case running(ObjectIdentifier)
      case done
    }

    private let sql: String
    private let phase = Mutex(Phase.idle)
    private let expired = Mutex(false)
    private let released = DispatchSemaphore(value: 0)
    private let (heldSignal, heldContinuation) = AsyncStream<Void>.makeStream()

    init(matching sql: String) {
      self.sql = sql
    }

    /// Holds the next matching statement.
    func arm() {
      phase.withLock { $0 = .armed }
    }

    /// Returns once the armed statement ran and is held.
    func held() async {
      for await _ in heldSignal { return }
    }

    /// Lets the held statement's transaction end. Holds are single use, so a
    /// second call (from `remove()`) is harmless.
    func release() {
      released.signal()
    }

    var timedOut: Bool { expired.withLock { $0 } }

    /// Runs on the connection's queue; blocks it while the statement is held.
    fileprivate func observe(_ statement: String, on connection: ObjectIdentifier) {
      let holds = phase.withLock { phase -> Bool in
        switch phase {
        case .armed where statement.contains(sql):
          phase = .running(connection)
          return false
        case .running(let owner) where owner == connection && statement.hasPrefix("COMMIT"):
          phase = .done
          return true
        default:
          return false
        }
      }
      guard holds else { return }
      heldContinuation.yield()
      if released.wait(timeout: .now() + Self.limit) == .timedOut {
        expired.withLock { $0 = true }
      }
    }
  }
}
