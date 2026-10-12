import Foundation
import StenoCore
import Testing

@testable import StenoHandover

/// Pair Again's keychain writes, cut short before each one in turn: the
/// stage's delete and store, then the promote's delete (certificates before
/// their keys) and its two relabels. Every next launch either asks again,
/// until the record names the new identity, or loads the new one; none is
/// stuck unable to read the keychain, and none mints. Of two identities
/// under the current label, the recorded one loads. The fake keeps
/// certificates and keys as separate items, as the file keychain does, and
/// runs the product's order and choice (`IdentityItems`' extension).
@Suite final class IdentityItemsTests {
  /// Certificates and keys, each labelled, a key found by its certificate's
  /// fingerprint as the keychain finds it by hash. A write cut short throws
  /// `Cut` before it changes anything.
  final class FakeItems: IdentityItems, @unchecked Sendable {
    struct Item {
      var label: String
      let identity: HandoverIdentity
    }
    struct Cut: Error { let write: String }

    var certificateItems: [Item] = []
    var keyItems: [Item] = []
    /// What a store puts under its label, as the login keychain would hold
    /// the minted identity.
    let next: HandoverIdentity
    var writes: [String] = []
    /// The index in `writes` of the write that is cut short.
    var cutBefore: Int?

    init(next: HandoverIdentity) { self.next = next }

    func add(_ identity: HandoverIdentity, label: String) {
      certificateItems.append(Item(label: label, identity: identity))
      keyItems.append(Item(label: label, identity: identity))
    }

    func write(_ name: String) throws {
      if writes.count == cutBefore { throw Cut(write: name) }
      writes.append(name)
    }

    func certificates(labelled label: String) -> [String] {
      certificateItems.filter { $0.label == label }.map { hex($0.identity) }
    }

    func fingerprint(of certificate: String) -> String { certificate }

    func identity(of certificate: String, labelled label: String) throws -> HandoverIdentity {
      guard privateKey(of: certificate) != nil,
        let item = certificateItems.first(where: { hex($0.identity) == certificate })
      else {
        throw IdentityError.malformed(
          "the certificate labelled \"\(label)\" has no private key in the keychain")
      }
      return item.identity
    }

    func privateKey(of certificate: String) -> String? {
      keyItems.contains { hex($0.identity) == certificate } ? certificate : nil
    }

    func store(_: MintedIdentity, label: String) throws {
      try write("store the key under \(label)")
      keyItems.append(Item(label: label, identity: next))
      try write("store the certificate under \(label)")
      certificateItems.append(Item(label: label, identity: next))
    }

    func setLabel(_ label: String, onCertificate certificate: String) throws {
      guard let index = certificateItems.firstIndex(where: { hex($0.identity) == certificate })
      else { throw IdentityError.notFound(label: label) }
      try write("label the certificate \(label)")
      certificateItems[index].label = label
    }

    func setLabel(_ label: String, onKey key: String) throws {
      guard let index = keyItems.firstIndex(where: { hex($0.identity) == key }) else {
        throw IdentityError.notFound(label: label)
      }
      try write("label the key \(label)")
      keyItems[index].label = label
    }

    func deleteCertificates(labelled label: String) throws {
      try write("delete the certificates under \(label)")
      certificateItems.removeAll { $0.label == label }
    }

    func deleteKey(_ key: String) throws {
      try write("delete a key found through its certificate")
      keyItems.removeAll { hex($0.identity) == key }
    }

    func deleteKeys(labelled label: String) throws {
      try write("delete the keys under \(label)")
      keyItems.removeAll { $0.label == label }
    }
  }

  static let label = "current"
  static let staging = "staged"
  /// The record of an identity neither test identity is.
  static let previous = String(repeating: "ab", count: 32)

  let known: HandoverIdentity
  let other: HandoverIdentity
  let directory: URL

  init() throws {
    known = try TestIdentity.load()
    other = try TestIdentity.load("other-identity")
    directory = try Fixtures.temporaryDirectory("steno-identity-items")
  }

  deinit { try? FileManager.default.removeItem(at: directory) }

  /// One Mac: a keychain, a record and a store with one paired phone.
  struct Mac {
    let items: FakeItems
    let record: IdentityRecordFile
    let store: MeetingStore

    /// A launch: a fresh guard over the same keychain, record and store.
    var launch: HandoverIdentityGuard {
      HandoverIdentityGuard(
        keychain: LabelledIdentityKeychain(
          items: items, label: IdentityItemsTests.label, stagingLabel: IdentityItemsTests.staging),
        record: record, store: store)
    }
  }

  /// The Mac before Pair Again: the record names `previous`, or nothing
  /// (an upgrade from a build without the guard, over a missing identity),
  /// and the keychain holds `known` as the current identity (`replaced`)
  /// or only as a leftover staged by an earlier Pair Again cut short
  /// (`missing`).
  func mac(replaced: Bool, recorded: Bool) async throws -> Mac {
    try await mac(
      holding: [(known, replaced ? Self.label : Self.staging)],
      recorded: recorded ? Self.previous : nil)
  }

  /// A Mac whose keychain holds `held`, in that order, under the labels
  /// given, with `recorded` in its record.
  func mac(holding held: [(HandoverIdentity, String)], recorded: String?) async throws -> Mac {
    let items = FakeItems(next: other)
    for (identity, label) in held { items.add(identity, label: label) }
    let record = IdentityRecordFile(
      supportDirectory: directory.appendingPathComponent(UUID().uuidString))
    if let recorded { try record.record(recorded) }
    let store = try MeetingStore.inMemory()
    try await store.save(
      PairedDevice(id: UUID(), name: "Phone", pairedAt: Date()),
      tokenHash: Data(repeating: 1, count: 32))
    return Mac(items: items, record: record, store: store)
  }

  /// `other` alone, under the current label; nothing of `known` is left.
  /// A promote cut short between its relabels leaves the key under the
  /// staging label (`keyLabel`), which the identity loads through.
  func expectOnlyTheNewIdentity(_ items: FakeItems, keyLabel: String = label) {
    #expect(
      items.certificateItems.map { "\($0.label) \(hex($0.identity))" } == ["current \(hex(other))"])
    #expect(
      items.keyItems.map { "\($0.label) \(hex($0.identity))" } == ["\(keyLabel) \(hex(other))"])
  }

  /// The starting points of Pair Again: missing and replaced under a
  /// record, and missing with no record yet.
  static let starts = [(false, true), (true, true), (false, false)]

  /// The writes in order: the promote deletes the certificates before the
  /// keys it found through them, and relabels the certificate before the
  /// key; a stage deletes a leftover first.
  @Test(arguments: starts)
  func pairAgainWritesInOrder(replaced: Bool, recorded: Bool) async throws {
    let mac = try await mac(replaced: replaced, recorded: recorded)
    #expect(hex(try await mac.launch.loadOrPairAgain(commonName: "A") { true }) == hex(other))
    let leftover = replaced ? [] : ["delete a key found through its certificate"]
    let oldKey = replaced ? ["delete a key found through its certificate"] : []
    #expect(
      mac.items.writes
        == ["delete the certificates under staged"] + leftover + [
          "delete the keys under staged",
          "store the key under staged",
          "store the certificate under staged",
          "delete the certificates under current",
        ] + oldKey + [
          "delete the keys under current",
          "label the certificate current",
          "label the key current",
        ])
    #expect(try mac.record.recorded() == hex(other))
    #expect(try await mac.store.pairedDevices().isEmpty)
    expectOnlyTheNewIdentity(mac.items)
  }

  /// Cut short before each write in turn, among them the two inside the
  /// promote's delete: between the certificates and the old key, and
  /// between the delete and the relabel. Before the record names the new
  /// identity the launch asks again, and the next Pair Again finishes;
  /// after it, the launch loads the new identity without asking, finishing
  /// the promote where it must.
  @Test(arguments: starts)
  func everyCutLeavesALaunchThatAsksOrLoadsTheNewIdentity(replaced: Bool, recorded: Bool)
    async throws
  {
    let uncut = try await mac(replaced: replaced, recorded: recorded)
    _ = try await uncut.launch.loadOrPairAgain(commonName: "A") { true }
    for (index, write) in uncut.items.writes.enumerated() {
      let mac = try await mac(replaced: replaced, recorded: recorded)
      mac.items.cutBefore = index
      let cut = await #expect(throws: FakeItems.Cut.self, "cut before \(write)") {
        try await mac.launch.loadOrPairAgain(commonName: "A") { true }
      }
      #expect(cut?.write == write)
      mac.items.cutBefore = nil
      mac.items.writes = []

      let recordedNew = try mac.record.recorded() == hex(other)
      var asked = false
      do {
        let identity = try await mac.launch.loadOrPairAgain(commonName: "A") {
          asked = true
          return false
        }
        #expect(recordedNew, "loads before the record, cut before \(write)")
        #expect(hex(identity) == hex(other) && !asked, "cut before \(write)")
        #expect(!mac.items.writes.contains { $0.hasPrefix("store") }, "cut before \(write)")
        let relabelled = write != "label the key current"
        expectOnlyTheNewIdentity(mac.items, keyLabel: relabelled ? Self.label : Self.staging)
      } catch let refusal as IdentityUnavailability {
        #expect(!recordedNew, "asks after the record, cut before \(write)")
        #expect(refusal == (replaced ? .replaced : .missing) && asked, "cut before \(write)")
        #expect(mac.items.writes.isEmpty, "cut before \(write)")
        #expect(hex(try await mac.launch.loadOrPairAgain(commonName: "A") { true }) == hex(other))
        expectOnlyTheNewIdentity(mac.items)
      }
    }
  }

  /// An old login keychain added back beside the new one after a finished
  /// Pair Again: two identities under the current label, the recorded one
  /// first or second in the keychain's order. The launch loads the
  /// recorded one and asks nothing, so no Pair Again runs and no promote's
  /// delete removes the identity the phones know.
  @Test(arguments: [false, true])
  func ofTwoIdentitiesUnderTheLabelTheRecordedOneLoads(recordedFirst: Bool) async throws {
    let held = recordedFirst ? [other, known] : [known, other]
    let mac = try await mac(holding: held.map { ($0, Self.label) }, recorded: hex(other))
    var asked = false
    let identity = try await mac.launch.loadOrPairAgain(commonName: "A") {
      asked = true
      return true
    }
    #expect(hex(identity) == hex(other) && !asked)
    #expect(mac.items.writes.isEmpty)
    #expect(mac.items.certificateItems.map { hex($0.identity) } == held.map(hex))
    #expect(mac.items.keyItems.map { hex($0.identity) } == held.map(hex))
    #expect(try mac.record.recorded() == hex(other))
    #expect(try await mac.store.pairedDevices().count == 1)
  }

  /// The same with neither recorded is replaced, whichever comes first.
  @Test(arguments: [false, true])
  func ofTwoIdentitiesUnderTheLabelNeitherRecordedIsReplaced(knownFirst: Bool) async throws {
    let held = knownFirst ? [known, other] : [other, known]
    let mac = try await mac(holding: held.map { ($0, Self.label) }, recorded: Self.previous)
    await #expect(throws: IdentityUnavailability.replaced) {
      try await mac.launch.loadOrPairAgain(commonName: "A") { false }
    }
    #expect(mac.items.writes.isEmpty)
    #expect(try mac.record.recorded() == Self.previous)
    #expect(try await mac.store.pairedDevices().count == 1)
  }
}

private func hex(_ identity: HandoverIdentity) -> String { ContentHash.hex(identity.fingerprint) }
