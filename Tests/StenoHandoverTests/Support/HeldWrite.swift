import Foundation
import Synchronization

@testable import StenoHandover

/// The engine's chunk writes (`HandoverEngine.writeChunk`) into the partial.
/// Once armed, holds the next write after its bytes landed and before the
/// engine resumes, until `release()`; every other write goes straight
/// through.
final class HeldWrite: Sendable {
  private enum Phase {
    case idle, armed, holding, done
  }

  private let phase = Mutex(Phase.idle)
  private let (heldSignal, heldContinuation) = AsyncStream<Void>.makeStream()
  private let (released, releasing) = AsyncStream<Void>.makeStream()

  func write(_ data: Data, at offset: UInt64, to url: URL) async throws {
    try await ReceivingFile.write(data, at: offset, to: url)
    let hold = phase.withLock { phase -> Bool in
      guard case .armed = phase else { return false }
      phase = .holding
      return true
    }
    guard hold else { return }
    heldContinuation.yield()
    for await _ in released {}
    phase.withLock { $0 = .done }
  }

  /// Holds the next write.
  func arm() {
    phase.withLock { $0 = .armed }
  }

  /// Returns once the armed write landed and is held.
  func held() async {
    for await _ in heldSignal { return }
  }

  /// Lets the held write return to the engine.
  func release() {
    releasing.finish()
  }
}
