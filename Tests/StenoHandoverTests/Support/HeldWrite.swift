import Foundation
import Synchronization

@testable import StenoHandover

/// The engine's chunk writes (`HandoverEngine.writeChunk`) into the partial.
/// Once armed, holds the next write after its bytes landed and before the
/// engine resumes, until `release()`; every other write goes straight
/// through.
final class HeldWrite: Sendable {
  private let armed = Atomic(false)
  private let (heldSignal, heldContinuation) = AsyncStream<Void>.makeStream()
  private let (released, releasing) = AsyncStream<Void>.makeStream()

  func write(_ data: Data, at offset: UInt64, to url: URL) async throws {
    try await ReceivingFile.write(data, at: offset, to: url)
    guard armed.exchange(false, ordering: .acquiringAndReleasing) else { return }
    heldContinuation.yield()
    for await _ in released {}
  }

  /// Holds the next write.
  func arm() {
    armed.store(true, ordering: .releasing)
  }

  /// Returns once the armed write landed and is held.
  func held() async {
    for await _ in heldSignal { return }
  }

  /// Lets the held write return to the engine. Single use: a write armed
  /// after this goes straight through.
  func release() {
    releasing.finish()
  }
}
