import Foundation
import StenoCore
import Synchronization

/// An intake that commits each admission as core's `RecordingIntake` does,
/// without the copy: the `.complete` receipt, a meeting under a fresh id and
/// its asset in one durable transaction, which writes the ledger row
/// (`MeetingStore.saveDurably(_:meeting:asset:)`), or the receipt alone with
/// the meeting the ledger holds for the same bytes; it refuses another
/// upload's receipt the same way. `holdNext(_:)` holds the next admission
/// before or after its commit until `release()`, once. Rust: `StoreIntake`
/// in `crates/steno-handover/tests/common/mod.rs`.
final class StoreIntake: HandoverIntake, Sendable {
  struct NoReceipt: Error {}

  /// Where `holdNext(_:)` holds the admission.
  enum Hold: Sendable {
    case beforeTheCommit
    case afterTheCommit
  }

  let store: MeetingStore
  private let committed = Mutex<[UUID]>([])
  private let hold = Mutex<Hold?>(nil)
  private let (heldSignal, heldContinuation) = AsyncStream<Void>.makeStream()
  private let (released, releasing) = AsyncStream<Void>.makeStream()

  init(_ store: MeetingStore) {
    self.store = store
  }

  /// The meetings committed so far, oldest first.
  var meetings: [UUID] { committed.withLock { $0 } }

  /// Holds the next admission at `point`.
  func holdNext(_ point: Hold) {
    hold.withLock { $0 = point }
  }

  /// Waits at `point` when the admission is held there.
  private func wait(_ held: Hold?, at point: Hold) async {
    guard held == point else { return }
    heldContinuation.yield()
    for await _ in released {}
  }

  /// Returns once the held admission is in flight.
  func held() async {
    for await _ in heldSignal { return }
  }

  /// Lets the held admission go on to its commit.
  func release() {
    releasing.finish()
  }

  func admit(file: URL, metadata: RecordingMetadata, device: PairedDevice) async throws -> UUID {
    let held = hold.withLock { point in
      defer { point = nil }
      return point
    }
    await wait(held, at: .beforeTheCommit)
    guard var receipt = try await store.handoverReceipt(recordingID: metadata.recordingID)
    else { throw NoReceipt() }
    guard receipt.deviceID == device.id, receipt.byteCount == metadata.byteCount,
      receipt.sha256 == metadata.sha256
    else {
      throw MeetingStoreError.receiptOfAnotherUpload(metadata.recordingID)
    }
    var meeting = SampleData.meeting()
    meeting.id = UUID()
    var asset = SampleData.audioAsset()
    asset.id = UUID()
    asset.meetingID = meeting.id
    receipt.state = .complete(meetingID: meeting.id)
    let admitted = try await store.saveDurably(receipt, meeting: meeting, asset: asset)
    if admitted == meeting.id {
      committed.withLock { $0.append(meeting.id) }
    }
    await wait(held, at: .afterTheCommit)
    return admitted
  }
}
