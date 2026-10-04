import Dispatch
import Foundation
import GRDB
import StenoCore
import Synchronization

/// An on-disk store whose next handover receipt read, once armed, stops
/// after it has fetched the row and before its transaction ends, until
/// `release()`. The store is a WAL pool, so writes (a revoke, a pairing) run
/// to the end meanwhile, and the held read still returns the row it saw:
/// the engine resumes with a receipt that is no longer in the store.
final class ReceiptReadGate: Sendable {
  let store: MeetingStore
  private let directory: URL
  private let hold = Hold()

  init() throws {
    directory = try Fixtures.temporaryDirectory("receipt-read-gate")
    let hold = self.hold
    var configuration = Configuration()
    configuration.prepareDatabase { db in
      let connection = ObjectIdentifier(db)
      db.trace { event in
        guard case .statement(let statement) = event else { return }
        hold.observe(statement.sql, on: connection)
      }
    }
    store = try MeetingStore(
      writer: DatabasePool(
        path: directory.appendingPathComponent("steno.sqlite").path,
        configuration: configuration))
  }

  /// Holds the next receipt read.
  func arm() {
    hold.phase.withLock { $0 = .armed }
  }

  /// Returns once the armed read has fetched its row and waits.
  func reading() async {
    for await _ in hold.held { return }
  }

  /// Lets the held read return.
  func release() {
    hold.released.signal()
  }

  func remove() {
    try? FileManager.default.removeItem(at: directory)
  }

  /// The state the connections' trace hooks share. The armed read is the
  /// next statement on the receipt table; it is held at the `COMMIT` that
  /// ends its read transaction on the same connection, after the row.
  private final class Hold: Sendable {
    enum Phase {
      case idle, armed
      case reading(ObjectIdentifier)
      case done
    }

    let phase = Mutex(Phase.idle)
    let released = DispatchSemaphore(value: 0)
    let held: AsyncStream<Void>
    private let heldContinuation: AsyncStream<Void>.Continuation

    init() {
      (held, heldContinuation) = AsyncStream.makeStream()
    }

    /// Runs on the connection's queue; blocks it while the read is held.
    func observe(_ sql: String, on connection: ObjectIdentifier) {
      let holds = phase.withLock { phase -> Bool in
        switch phase {
        case .armed where sql.contains("FROM \"handoverReceipt\""):
          phase = .reading(connection)
          return false
        case .reading(let reader) where reader == connection && sql.hasPrefix("COMMIT"):
          phase = .done
          return true
        default:
          return false
        }
      }
      guard holds else { return }
      heldContinuation.yield()
      released.wait()
    }
  }
}
