import Foundation
import Synchronization

@testable import StenoHandover

/// The engine's whole-file hash of the partial (`HandoverEngine.hashMatches`).
/// Once armed, holds the next hash after it ran and before the engine
/// resumes, until `release()`; every other hash goes straight through.
final class HeldHash: Sendable {
  private let armed = Atomic(false)
  private let (heldSignal, heldContinuation) = AsyncStream<Void>.makeStream()
  private let (released, releasing) = AsyncStream<Void>.makeStream()

  func hashMatches(_ url: URL, expected: Data) async throws -> Bool {
    let matches = try await ReceivingFile.hashMatches(url, expected: expected)
    guard armed.exchange(false, ordering: .acquiringAndReleasing) else { return matches }
    heldContinuation.yield()
    for await _ in released {}
    return matches
  }

  /// Holds the next hash.
  func arm() {
    armed.store(true, ordering: .releasing)
  }

  /// Returns once the armed hash ran and is held.
  func held() async {
    for await _ in heldSignal { return }
  }

  /// Lets the held hash answer the engine. Single use: a hash armed after
  /// this goes straight through.
  func release() {
    releasing.finish()
  }
}
