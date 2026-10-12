import Foundation
import StenoCore
import Testing

@testable import StenoHandover

/// The identity guard over a fake keychain, the Mac's Create New Keychain
/// case among them: nothing is minted while a fingerprint is recorded or a
/// phone is paired, another identity is not adopted, a read that fails is
/// no reason to ask, and Pair Again removes the phones before it mints.
/// The record is the Swift app's own file; the Rust app's is never touched.
/// Rust: `crates/steno-handover/tests/identity.rs` and
/// `crates/steno-services/src/keychain_reset_tests.rs`.
@Suite struct IdentityGuardTests {
  /// The keychain: holds one identity or none; a store puts `next` in, as
  /// the login keychain would hold the minted one.
  final class FakeKeychain: HandoverIdentityStore, @unchecked Sendable {
    var identity: HandoverIdentity?
    var readError: (any Error)?
    let next: HandoverIdentity
    var stores = 0
    var deletes = 0

    init(_ identity: HandoverIdentity?, next: HandoverIdentity) {
      self.identity = identity
      self.next = next
    }

    func load() throws -> HandoverIdentity? {
      if let readError { throw readError }
      return identity
    }
    func store(_ identity: MintedIdentity) throws {
      stores += 1
      self.identity = next
    }
    func delete() throws {
      deletes += 1
      identity = nil
    }
  }

  let known = try! TestIdentity.load()
  let other = try! TestIdentity.load("other-identity")
  let directory = FileManager.default.temporaryDirectory
    .appendingPathComponent("steno-identity-guard-\(UUID().uuidString)", isDirectory: true)
  let store = try! MeetingStore.inMemory()

  func identityGuard(_ keychain: FakeKeychain, recorded: HandoverIdentity? = nil, phones: Int = 0)
    async throws -> HandoverIdentityGuard
  {
    let record = IdentityRecordFile(supportDirectory: directory)
    if let recorded { try record.record(ContentHash.hex(recorded.fingerprint)) }
    for index in 0..<phones {
      try await store.save(
        PairedDevice(id: UUID(), name: "Phone \(index)", pairedAt: Date()),
        tokenHash: Data(repeating: UInt8(index), count: 32))
    }
    return HandoverIdentityGuard(keychain: keychain, record: record, store: store)
  }

  func refusal(_ body: () async throws -> HandoverIdentity) async -> IdentityUnavailability? {
    do {
      _ = try await body()
      return nil
    } catch {
      return error as? IdentityUnavailability
    }
  }

  func hex(_ identity: HandoverIdentity) -> String { ContentHash.hex(identity.fingerprint) }

  @Test func anEmptyKeychainWithPairedPhonesMintsNothing() async throws {
    defer { try? FileManager.default.removeItem(at: directory) }
    let keychain = FakeKeychain(nil, next: other)
    let identityGuard = try await identityGuard(keychain, phones: 1)
    #expect(await refusal { try await identityGuard.loadOrCreate(commonName: "A") } == .missing)
    #expect(keychain.stores == 0)
    #expect(try identityGuard.record.recorded() == nil)
    #expect(try await store.pairedDevices().count == 1)
  }

  /// A recorded fingerprint over an empty keychain is missing at every
  /// launch until the identity is back; then it loads and the phones stay.
  @Test func aRecordOverAnEmptyKeychainWaitsUntilTheIdentityIsRestored() async throws {
    defer { try? FileManager.default.removeItem(at: directory) }
    let keychain = FakeKeychain(nil, next: other)
    let identityGuard = try await identityGuard(keychain, recorded: known, phones: 1)
    let bytes = try Data(contentsOf: identityGuard.record.url)
    for _ in 0..<2 {
      #expect(await refusal { try await identityGuard.loadOrCreate(commonName: "A") } == .missing)
    }
    #expect(keychain.stores == 0)
    #expect(try Data(contentsOf: identityGuard.record.url) == bytes)

    keychain.identity = known
    #expect(hex(try await identityGuard.loadOrCreate(commonName: "A")) == hex(known))
    #expect(try Data(contentsOf: identityGuard.record.url) == bytes)
    #expect(try await store.pairedDevices().count == 1)
  }

  @Test func anotherIdentityIsNotAdoptedAndNeitherIsTouched() async throws {
    defer { try? FileManager.default.removeItem(at: directory) }
    let keychain = FakeKeychain(other, next: other)
    let identityGuard = try await identityGuard(keychain, recorded: known, phones: 1)
    #expect(await refusal { try await identityGuard.loadOrCreate(commonName: "A") } == .replaced)
    #expect(
      await refusal { try await identityGuard.loadOrPairAgain(commonName: "A") { false } }
        == .replaced)
    #expect(hex(try #require(keychain.identity)) == hex(other))
    #expect(keychain.stores == 0 && keychain.deletes == 0)
    #expect(try identityGuard.record.recorded() == hex(known))
    #expect(try await store.pairedDevices().count == 1)
  }

  /// A locked or denied keychain is unavailable for the run, without the
  /// question.
  @Test func aReadThatFailsAsksNothingAndMintsNothing() async throws {
    defer { try? FileManager.default.removeItem(at: directory) }
    let keychain = FakeKeychain(nil, next: other)
    keychain.readError = IdentityError.security("SecItemCopyMatching(certificate)", -25308)
    let identityGuard = try await identityGuard(keychain)
    var asked = false
    let refused = await refusal {
      try await identityGuard.loadOrPairAgain(commonName: "A") {
        asked = true
        return true
      }
    }
    guard case .unreadable = refused else {
      Issue.record("unreadable, got \(String(describing: refused))")
      return
    }
    #expect(!asked)
    #expect(keychain.stores == 0 && keychain.deletes == 0)
  }

  @Test func theFirstRunMintsStoresAndRecords() async throws {
    defer { try? FileManager.default.removeItem(at: directory) }
    let keychain = FakeKeychain(nil, next: other)
    let identityGuard = try await identityGuard(keychain)
    #expect(hex(try await identityGuard.loadOrCreate(commonName: "A")) == hex(other))
    #expect(try identityGuard.record.recorded() == hex(other))
    #expect(hex(try await identityGuard.loadOrCreate(commonName: "A")) == hex(other))
    #expect(keychain.stores == 1)
  }

  /// The identity of a build from before the guard has its fingerprint
  /// recorded, not replaced.
  @Test func anIdentityWithoutARecordIsRecordedNotMinted() async throws {
    defer { try? FileManager.default.removeItem(at: directory) }
    let keychain = FakeKeychain(known, next: other)
    let identityGuard = try await identityGuard(keychain, phones: 1)
    #expect(hex(try await identityGuard.loadOrCreate(commonName: "A")) == hex(known))
    #expect(try identityGuard.record.recorded() == hex(known))
    #expect(keychain.stores == 0)
  }

  @Test func pairAgainRemovesThePhonesAndMintsANewIdentity() async throws {
    defer { try? FileManager.default.removeItem(at: directory) }
    let keychain = FakeKeychain(nil, next: other)
    let identityGuard = try await identityGuard(keychain, recorded: known, phones: 2)
    var questions = 0
    let identity = try await identityGuard.loadOrPairAgain(commonName: "A") {
      questions += 1
      return true
    }
    #expect(questions == 1)
    #expect(hex(identity) == hex(other))
    #expect(try await store.pairedDevices().isEmpty)
    #expect(try identityGuard.record.recorded() == hex(other))
    #expect(keychain.deletes == 1 && keychain.stores == 1)
    #expect(hex(try await identityGuard.loadOrCreate(commonName: "A")) == hex(other))
  }

  @Test func aDeclinedPairAgainChangesNothing() async throws {
    defer { try? FileManager.default.removeItem(at: directory) }
    let keychain = FakeKeychain(nil, next: other)
    let identityGuard = try await identityGuard(keychain, recorded: known, phones: 1)
    let bytes = try Data(contentsOf: identityGuard.record.url)
    #expect(
      await refusal { try await identityGuard.loadOrPairAgain(commonName: "A") { false } }
        == .missing)
    #expect(keychain.identity == nil && keychain.stores == 0 && keychain.deletes == 0)
    #expect(try Data(contentsOf: identityGuard.record.url) == bytes)
    #expect(try await store.pairedDevices().count == 1)
  }

  @Test func aSettingsSaveKeepsTheRecord() async throws {
    defer { try? FileManager.default.removeItem(at: directory) }
    let keychain = FakeKeychain(known, next: other)
    let identityGuard = try await identityGuard(keychain, recorded: known, phones: 1)
    let bytes = try Data(contentsOf: identityGuard.record.url)
    var settings = Settings()
    settings.launchAtLogin = false
    try await SettingsStore(writer: store.writer).save(settings)
    #expect(try Data(contentsOf: identityGuard.record.url) == bytes)
    #expect(hex(try await identityGuard.loadOrCreate(commonName: "A")) == hex(known))
  }

  /// The Swift app records into `swift-handover-identity.json` and never
  /// reads or writes the Rust app's `handover-identity.json`, even when that
  /// names another identity.
  @Test func theRustAppsRecordIsNeitherReadNorWritten() async throws {
    defer { try? FileManager.default.removeItem(at: directory) }
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    let rust = directory.appendingPathComponent("handover-identity.json")
    let rustBytes = Data("{\"\(IdentityRecordFile.key)\": \"\(hex(other))\"}".utf8)
    try rustBytes.write(to: rust)
    let keychain = FakeKeychain(known, next: other)
    let identityGuard = try await identityGuard(keychain, phones: 1)
    #expect(identityGuard.record.url.lastPathComponent == "swift-handover-identity.json")
    #expect(hex(try await identityGuard.loadOrCreate(commonName: "A")) == hex(known))
    #expect(try identityGuard.record.recorded() == hex(known))
    #expect(try Data(contentsOf: rust) == rustBytes)
  }

  @Test func theRecordFileReplacesForgetsAndRefusesDamage() throws {
    defer { try? FileManager.default.removeItem(at: directory) }
    let record = IdentityRecordFile(supportDirectory: directory)
    try record.forget()
    try record.record("ab12")
    try record.record("cd34")
    #expect(
      try JSONDecoder().decode([String: String].self, from: Data(contentsOf: record.url))
        == [IdentityRecordFile.key: "cd34"])
    #expect(try FileManager.default.contentsOfDirectory(atPath: directory.path).count == 1)
    try Data("{".utf8).write(to: record.url)
    #expect(throws: (any Error).self) { try record.recorded() }
    try record.forget()
    #expect(try record.recorded() == nil)
  }
}
