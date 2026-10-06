import Foundation
import StenoCore
import Synchronization

/// Core's `FakeHandoverIntake` with its first admission held after the fake
/// recorded it and before it answers the engine, until `release()`; every
/// later admission goes straight through. `fake` reads the admissions.
final class HeldIntake: HandoverIntake, Sendable {
  let fake: FakeHandoverIntake
  private let first = Atomic(true)
  private let (heldSignal, heldContinuation) = AsyncStream<Void>.makeStream()
  private let (released, releasing) = AsyncStream<Void>.makeStream()

  init(_ fake: FakeHandoverIntake) {
    self.fake = fake
  }

  func admit(file: URL, metadata: RecordingMetadata, device: PairedDevice) async throws -> UUID {
    let meetingID = try await fake.admit(file: file, metadata: metadata, device: device)
    guard first.exchange(false, ordering: .acquiringAndReleasing) else { return meetingID }
    heldContinuation.yield()
    for await _ in released {}
    return meetingID
  }

  /// Returns once the first admission is held.
  func held() async {
    for await _ in heldSignal { return }
  }

  /// Lets the held admission answer the engine.
  func release() {
    releasing.finish()
  }
}
