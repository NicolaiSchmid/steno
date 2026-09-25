import Foundation
import StenoCore
import Testing

@testable import StenoAdapters

/// The delivery policy without a filesystem: which receipt applies, what may
/// be written, what the receipt says afterwards, and the collision rule.
@Suite struct DeliveryLedgerTests {
  static let root = "/vault"
  static let folder = "Meetings/2026-09-24-sync"
  static let note = "\(folder)/2026-09-24-sync.md"
  static let audio = "\(folder)/audio.m4a"
  static let anna = "People/Anna.md"

  static func receipt(root: String = root, files: [String: FileOwnership]) -> DeliveryReceipt {
    DeliveryReceipt(
      root: root, folder: folder,
      files: files.keys.sorted().map {
        DeliveredFile(
          relativePath: $0, ownership: files[$0]!, sha256: Data(repeating: 1, count: 32))
      },
      rendererVersion: 1)
  }

  @Test func aFirstDeliveryWritesEverything() {
    let ledger = DeliveryLedger(previous: nil, root: Self.root)
    #expect(ledger.isFirstDelivery)
    #expect(ledger.pinnedFolder == nil)
    #expect(ledger.mayWrite(Self.note, exists: true), "a first delivery owns its folder")
    #expect(ledger.mayWrite(Self.note, exists: false))
    #expect(ledger.files.isEmpty)
  }

  @Test func filesTheAppNeverWroteAreNotOpenedOnReexport() {
    let previous = Self.receipt(files: [Self.note: .owned, Self.anna: .managedBlock])
    let ledger = DeliveryLedger(previous: previous, root: Self.root)
    #expect(!ledger.isFirstDelivery)
    #expect(ledger.pinnedFolder == Self.folder)
    #expect(ledger.mayWrite(Self.note, exists: true), "listed as owned: rewritten")
    #expect(ledger.mayWrite(Self.audio, exists: false), "absent: written")
    #expect(!ledger.mayWrite(Self.audio, exists: true), "the user's file: never opened")
    #expect(!ledger.mayWrite(Self.anna, exists: true), "a managed block is not owned outright")
  }

  @Test func aReceiptFromAnotherRootIsAFirstDelivery() {
    let previous = Self.receipt(root: "/elsewhere", files: [Self.note: .owned])
    let ledger = DeliveryLedger(previous: previous, root: Self.root)
    #expect(ledger.isFirstDelivery)
    #expect(ledger.pinnedFolder == nil, "the other root pins nothing")
    #expect(ledger.files.isEmpty, "and carries nothing over")
    #expect(ledger.mayWrite(Self.note, exists: true))
  }

  @Test func spellingsOfOneRootAreTheSameRoot() {
    #expect(DeliveryLedger.sameRoot("/vault", "/vault/"))
    #expect(DeliveryLedger.sameRoot("/vault", "/vault/./Meetings/.."))
    #expect(DeliveryLedger.sameRoot("/a/b", "/a//b"))
    #expect(!DeliveryLedger.sameRoot("/vault", "/vault2"))
    #expect(!DeliveryLedger.sameRoot("/vault", "/other/vault"))
    let previous = Self.receipt(root: "/vault/", files: [Self.note: .owned])
    #expect(DeliveryLedger(previous: previous, root: "/vault").pinnedFolder == Self.folder)
  }

  @Test func theReceiptCarriesUnwrittenFilesAndRecordsNewOnes() {
    let previous = Self.receipt(files: [Self.note: .owned, Self.audio: .owned])
    var ledger = DeliveryLedger(previous: previous, root: Self.root)
    let bytes = Data("note\n".utf8)
    ledger.record(Self.note, .owned, bytes)
    ledger.record(Self.anna, .managedBlock, Data("page\n".utf8))

    let receipt = ledger.receipt(folder: Self.folder)
    #expect(receipt.root == Self.root)
    #expect(receipt.folder == Self.folder)
    #expect(receipt.rendererVersion == ArtifactRenderer.version)
    #expect(
      receipt.files.map(\.relativePath) == [Self.note, Self.audio, Self.anna].sorted(),
      "sorted by path; the opted-out audio stays listed")
    #expect(
      receipt.files.first { $0.relativePath == Self.note }?.sha256 == ContentHash.sha256(bytes))
    #expect(
      receipt.files.first { $0.relativePath == Self.audio }
        == previous.files.first { $0.relativePath == Self.audio }, "carried over unchanged")
    #expect(receipt.files.first { $0.relativePath == Self.anna }?.ownership == .managedBlock)
    #expect(ledger.lists(in: Self.folder) { $0.hasPrefix("audio.") })
    #expect(!ledger.lists(in: "People") { $0.hasPrefix("audio.") })
    #expect(!ledger.lists(in: "Meetings") { $0.hasPrefix("2026") }, "only direct children count")
  }

  @Test func theCollisionRuleSuffixesTakenFoldersAndReusesACrashedAttempt() {
    let ours = SampleData.uuid(1)
    let theirs = SampleData.uuid(2)
    let base = Self.folder
    func resolve(_ existing: [String: UUID?]) -> String {
      DeliveryLedger.resolveFolder(
        base: base, meetingID: ours, exists: { existing.keys.contains($0) },
        meetingIn: { existing[$0] ?? nil })
    }
    #expect(resolve([:]) == base)
    #expect(resolve([base: theirs]) == "\(base)-2", "another meeting's folder")
    #expect(resolve([base: nil]) == "\(base)-2", "a folder without meeting.json is taken too")
    #expect(resolve([base: theirs, "\(base)-2": nil]) == "\(base)-3")
    #expect(resolve([base: ours]) == base, "our own meeting.json: a crashed attempt, reused")
    #expect(resolve([base: theirs, "\(base)-2": ours]) == "\(base)-2")
  }

  @Test func folderURLJoinsRootAndFolder() {
    let receipt = Self.receipt(root: "/vault/", files: [:])
    #expect(receipt.folderURL.path == "/vault/\(Self.folder)")
    #expect(Self.receipt(files: [:]).folderURL.path == "/vault/\(Self.folder)")
  }
}
