import Foundation
import StenoCore

/// Takes the verified file out of the inbox before the intake behind it
/// sees it, as core's `RecordingIntake` has done by the time it returns: the
/// intake behind gets the file under `directory`, where a test still reads
/// it after `complete`, and the engine finds the inbox as the real intake
/// leaves it. On a failure the file goes back, where the real intake leaves
/// it for the phone's retry. Every test service's intake sits behind one
/// (`TestService.moving`).
///
/// The real intake copies the file and removes it only when it returns, so
/// in the app a re-announce during the intake still finds the verified file
/// and keeps its chunk set, and a revoke during the intake can remove it
/// before the intake copied it (the intake then fails, and the phone keeps
/// its recording). Here the file is gone for the whole intake: a
/// re-announce or a revoke during a held intake meets the inbox as the real
/// intake leaves it once it returned, which is the state the tests of a
/// revoke during the intake check.
struct MovingIntake: HandoverIntake {
  let intake: any HandoverIntake
  let directory: URL

  func admit(file: URL, metadata: RecordingMetadata, device: PairedDevice) async throws -> UUID {
    let taken = directory.appendingPathComponent(file.lastPathComponent)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    // A later admission of the same recording id (another phone's, after a
    // revoke) replaces the earlier one's file.
    try? FileManager.default.removeItem(at: taken)
    try FileManager.default.moveItem(at: file, to: taken)
    do {
      return try await intake.admit(file: taken, metadata: metadata, device: device)
    } catch {
      try? FileManager.default.moveItem(at: taken, to: file)
      throw error
    }
  }
}
