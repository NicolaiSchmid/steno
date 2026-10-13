import Foundation
import GRDB
import StenoCore
import Testing

@testable import StenoHandover

/// The identity guard over a fake keychain, the Mac's Create New Keychain
/// case among them: nothing is minted while a fingerprint is recorded or a
/// phone is paired, another identity is not adopted, nor one a paired phone
/// predates, a read that fails is no reason to ask, and Pair Again stages
/// the new identity before it removes the phones, so a refused write or a
/// run cut short mints nothing.
/// The record is the Swift app's own file; the Rust app's is never touched.
/// Rust: `crates/steno-handover/tests/identity.rs` and
/// `crates/steno-services/src/keychain_reset_tests.rs`.
@Suite final class IdentityGuardTests {
  /// The keychain: a current identity or none, and a staged one or none; a
  /// stage puts `next` there, as the login keychain would hold the minted
  /// one, and a promote moves it to the current one. Its promote cannot be
  /// cut short; `IdentityItemsTests` cuts the real one at every write.
  final class FakeKeychain: HandoverIdentityKeychain, @unchecked Sendable {
    var identity: HandoverIdentity?
    var stagedIdentity: HandoverIdentity?
    var readError: (any Error)?
    var stagedError: (any Error)?
    var stageError: (any Error)?
    var promoteError: (any Error)?
    let next: HandoverIdentity
    var stages = 0
    var promotes = 0

    init(_ identity: HandoverIdentity?, next: HandoverIdentity) {
      self.identity = identity
      self.next = next
    }

    func load(preferring _: String) throws -> HandoverIdentity? {
      if let readError { throw readError }
      return identity
    }
    func identities() throws -> [HandoverIdentity] {
      if let readError { throw readError }
      return identity.map { [$0] } ?? []
    }
    func staged() throws -> HandoverIdentity? {
      if let stagedError { throw stagedError }
      return stagedIdentity
    }
    func stage(_: MintedIdentity) throws -> HandoverIdentity {
      if let stageError { throw stageError }
      stages += 1
      stagedIdentity = next
      return next
    }
    func promote() throws {
      if let promoteError { throw promoteError }
      promotes += 1
      guard let stagedIdentity else { throw Refused() }
      identity = stagedIdentity
      self.stagedIdentity = nil
    }
  }

  struct Refused: Error {}

  let known: HandoverIdentity
  let other: HandoverIdentity
  let directory: URL
  let store: MeetingStore

  init() throws {
    known = try TestIdentity.load()
    other = try TestIdentity.load("other-identity")
    directory = try Fixtures.temporaryDirectory("steno-identity-guard")
    store = try MeetingStore.inMemory()
  }

  deinit { try? FileManager.default.removeItem(at: directory) }

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

  /// Pairs one phone at each of `dates`, as `identityGuard(phones:)` does;
  /// the guard reads the store at launch, so this may follow it.
  func pair(at dates: [Date]) async throws {
    let paired = try await store.pairedDevices().count
    for (index, date) in dates.enumerated() {
      try await store.save(
        PairedDevice(id: UUID(), name: "Phone at \(date)", pairedAt: date),
        tokenHash: Data(repeating: UInt8(paired + index), count: 32))
    }
  }

  func hex(_ identity: HandoverIdentity) -> String { ContentHash.hex(identity.fingerprint) }

  /// Refuses to remove the phone named `name`, as a store that fails
  /// partway through Pair Again's removals.
  func refuseToRemove(_ name: String) async throws {
    // A trigger takes no bound values; the names are the helper's own.
    try await store.writer.write { db in
      try db.execute(
        sql: """
          CREATE TEMP TRIGGER refuse BEFORE DELETE ON pairedDevice WHEN old.name = '\(name)'
          BEGIN SELECT RAISE(ABORT, 'refused'); END
          """)
    }
  }

  @Test func anEmptyKeychainWithPairedPhonesMintsNothing() async throws {
    let keychain = FakeKeychain(nil, next: other)
    let identityGuard = try await identityGuard(keychain, phones: 1)
    await #expect(throws: IdentityUnavailability.missing) {
      try await identityGuard.loadOrCreate(commonName: "A")
    }
    #expect(keychain.stages == 0)
    #expect(try identityGuard.record.recorded() == nil)
    #expect(try await store.pairedDevices().count == 1)
  }

  /// A recorded fingerprint over an empty keychain is missing at every
  /// launch until the identity is back; then it loads and the phones stay.
  @Test func aRecordOverAnEmptyKeychainWaitsUntilTheIdentityIsRestored() async throws {
    let keychain = FakeKeychain(nil, next: other)
    let identityGuard = try await identityGuard(keychain, recorded: known, phones: 1)
    let bytes = try Data(contentsOf: identityGuard.record.url)
    for _ in 0..<2 {
      await #expect(throws: IdentityUnavailability.missing) {
        try await identityGuard.loadOrCreate(commonName: "A")
      }
    }
    #expect(keychain.stages == 0)
    #expect(try Data(contentsOf: identityGuard.record.url) == bytes)

    keychain.identity = known
    #expect(hex(try await identityGuard.loadOrCreate(commonName: "A")) == hex(known))
    #expect(try Data(contentsOf: identityGuard.record.url) == bytes)
    #expect(try await store.pairedDevices().count == 1)
  }

  /// Every phone removed in Settings, then the keychain reset: the record
  /// alone keeps the guard.
  @Test func aRecordOverAnEmptyKeychainWithNoPhonesMintsNothing() async throws {
    let keychain = FakeKeychain(nil, next: other)
    let identityGuard = try await identityGuard(keychain, recorded: known)
    let bytes = try Data(contentsOf: identityGuard.record.url)
    await #expect(throws: IdentityUnavailability.missing) {
      try await identityGuard.loadOrCreate(commonName: "A")
    }
    #expect(keychain.stages == 0 && keychain.identity == nil)
    #expect(try Data(contentsOf: identityGuard.record.url) == bytes)
  }

  @Test func anotherIdentityIsNotAdoptedAndNeitherIsTouched() async throws {
    let keychain = FakeKeychain(other, next: other)
    let identityGuard = try await identityGuard(keychain, recorded: known, phones: 1)
    await #expect(throws: IdentityUnavailability.replaced) {
      try await identityGuard.loadOrCreate(commonName: "A")
    }
    await #expect(throws: IdentityUnavailability.replaced) {
      try await identityGuard.loadOrPairAgain(commonName: "A") { false }
    }
    #expect(hex(try #require(keychain.identity)) == hex(other))
    #expect(keychain.stages == 0 && keychain.promotes == 0)
    #expect(try identityGuard.record.recorded() == hex(known))
    #expect(try await store.pairedDevices().count == 1)
  }

  /// A locked or denied keychain is unavailable for the run, without the
  /// question.
  @Test func aReadThatFailsAsksNothingAndMintsNothing() async throws {
    let keychain = FakeKeychain(nil, next: other)
    let readError = IdentityError.security("SecItemCopyMatching(certificate)", -25308)
    keychain.readError = readError
    let identityGuard = try await identityGuard(keychain)
    var asked = false
    await #expect(throws: IdentityUnavailability.unreadable(String(describing: readError))) {
      try await identityGuard.loadOrPairAgain(commonName: "A") {
        asked = true
        return true
      }
    }
    #expect(!asked)
    #expect(keychain.stages == 0 && keychain.promotes == 0)
  }

  /// A damaged record is no reason to ask either, and its reason reads
  /// plainly in the menu bar.
  @Test func aDamagedRecordAsksNothingAndMintsNothing() async throws {
    let keychain = FakeKeychain(nil, next: other)
    let identityGuard = try await identityGuard(keychain)
    try Data("{".utf8).write(to: identityGuard.record.url)
    var asked = false
    let error = await #expect(throws: IdentityRecordFile.Unreadable.self) {
      try await identityGuard.loadOrPairAgain(commonName: "A") {
        asked = true
        return true
      }
    }
    #expect(!asked)
    #expect(keychain.stages == 0 && keychain.promotes == 0)
    #expect(error?.description == "this Mac's record of its phone pairing could not be read")
  }

  @Test func theFirstRunMintsStagesAndRecords() async throws {
    let keychain = FakeKeychain(nil, next: other)
    let identityGuard = try await identityGuard(keychain)
    #expect(hex(try await identityGuard.loadOrCreate(commonName: "A")) == hex(other))
    #expect(try identityGuard.record.recorded() == hex(other))
    #expect(hex(try await identityGuard.loadOrCreate(commonName: "A")) == hex(other))
    #expect(keychain.stages == 1 && keychain.promotes == 1 && keychain.stagedIdentity == nil)
  }

  /// The identity of a build from before the guard, older than every
  /// paired phone, has its fingerprint recorded, not replaced.
  @Test func anUnrecordedIdentityOlderThanEveryPhoneIsRecorded() async throws {
    let keychain = FakeKeychain(known, next: other)
    let identityGuard = try await identityGuard(keychain)
    let start = try known.notValidBefore()
    try await pair(at: [start.addingTimeInterval(3600), start.addingTimeInterval(86400)])
    var asked = false
    let identity = try await identityGuard.loadOrPairAgain(commonName: "A") {
      asked = true
      return true
    }
    #expect(hex(identity) == hex(known) && !asked)
    #expect(try identityGuard.record.recorded() == hex(known))
    #expect(keychain.stages == 0)
    #expect(try await store.pairedDevices().count == 2)
  }

  /// An identity newer than every paired phone, as a shipping build's
  /// silent mint after a keychain reset left it, is not recorded: the
  /// phones cannot know it. Not Now keeps everything, and Pair Again
  /// installs a new one.
  @Test func anUnrecordedIdentityNewerThanEveryPhoneIsReplaced() async throws {
    let keychain = FakeKeychain(known, next: other)
    let identityGuard = try await identityGuard(keychain)
    let start = try known.notValidBefore()
    try await pair(at: [start.addingTimeInterval(-86400), start.addingTimeInterval(-3600)])
    var asked = false
    await #expect(throws: IdentityUnavailability.replaced) {
      try await identityGuard.loadOrPairAgain(commonName: "A") {
        asked = true
        return false
      }
    }
    #expect(asked)
    #expect(try identityGuard.record.recorded() == nil)
    #expect(keychain.stages == 0 && keychain.identity.map(hex) == hex(known))
    #expect(try await store.pairedDevices().count == 2)

    #expect(hex(try await identityGuard.loadOrPairAgain(commonName: "A") { true }) == hex(other))
    #expect(try identityGuard.record.recorded() == hex(other))
    #expect(try await store.pairedDevices().isEmpty)
  }

  /// One identity newer than some phones but not all, among them one
  /// paired a second before its start beside one paired at it: refused in
  /// part, in the words the startup warning shows, and nothing changes.
  @Test(arguments: [(-86400.0, 3600.0), (-1.0, 0.0)])
  func anUnrecordedIdentityNewerThanSomePhonesIsPartlyReplaced(
    before: TimeInterval, after: TimeInterval
  ) async throws {
    let keychain = FakeKeychain(known, next: other)
    let identityGuard = try await identityGuard(keychain)
    let start = try known.notValidBefore()
    try await pair(at: [start.addingTimeInterval(before), start.addingTimeInterval(after)])
    var asked = false
    await #expect(throws: IdentityUnavailability.partlyReplaced) {
      try await identityGuard.loadOrPairAgain(commonName: "A") {
        asked = true
        return false
      }
    }
    #expect(asked)
    #expect(try identityGuard.record.recorded() == nil)
    #expect(keychain.stages == 0 && keychain.identity.map(hex) == hex(known))
    #expect(try await store.pairedDevices().count == 2)
    #expect(
      String(describing: IdentityUnavailability.partlyReplaced)
        == "some of your phones were paired before this Mac's pairing changed and can't "
        + "connect. Choose Pair Again the next time Steno opens to reconnect them.")
  }

  /// A certificate whose start cannot be read is no reason to record it,
  /// nor to ask: the launch is unreadable and nothing changes.
  @Test func anUnrecordedIdentityWhoseStartCannotBeReadAsksNothingAndRecordsNothing()
    async throws
  {
    let unparsable = Data([0x30, 0x00])
    #if canImport(Security)
      let broken = HandoverIdentity(secIdentity: known.secIdentity, certificateDER: unparsable)
    #else
      let broken = HandoverIdentity(certificateDER: unparsable)
    #endif
    #expect(throws: (any Error).self) { try broken.notValidBefore() }
    let keychain = FakeKeychain(broken, next: other)
    let identityGuard = try await identityGuard(keychain, phones: 1)
    var asked = false
    let refusal = await #expect(throws: IdentityUnavailability.self) {
      try await identityGuard.loadOrPairAgain(commonName: "A") {
        asked = true
        return true
      }
    }
    guard case .unreadable = refusal else {
      Issue.record("expected unreadable, got \(String(describing: refusal))")
      return
    }
    #expect(!asked)
    #expect(try identityGuard.record.recorded() == nil)
    #expect(keychain.stages == 0 && keychain.promotes == 0)
    #expect(try await store.pairedDevices().count == 1)
  }

  /// A minted certificate starts a minute before its mint
  /// (`IdentityTests` pins it), so a phone paired 59 or 60 seconds before
  /// the mint counts as paired after the start and the identity is
  /// recorded, and one paired 61 seconds before it predates the identity.
  @Test(arguments: [(59.0, true), (60.0, true), (61.0, false)])
  func aPhonePairedAtTheIdentitysStartCountsAsPairedAfterIt(
    secondsBeforeTheMint: TimeInterval, recorded: Bool
  ) async throws {
    let keychain = FakeKeychain(known, next: other)
    let identityGuard = try await identityGuard(keychain)
    let mint = try known.notValidBefore().addingTimeInterval(60)
    try await pair(at: [mint.addingTimeInterval(-secondsBeforeTheMint)])
    if recorded {
      #expect(hex(try await identityGuard.loadOrCreate(commonName: "A")) == hex(known))
      #expect(try identityGuard.record.recorded() == hex(known))
    } else {
      await #expect(throws: IdentityUnavailability.replaced) {
        try await identityGuard.loadOrCreate(commonName: "A")
      }
      #expect(try identityGuard.record.recorded() == nil)
    }
  }

  /// One case of the pairing-time rule: identities that start at `starts`
  /// and phones paired at `paired`, in seconds after a whole second, and the
  /// index of the identity every phone knows or the refusal.
  struct Rule: Sendable, CustomTestStringConvertible {
    enum Known: Equatable, Sendable {
      case index(Int)
      case refused(IdentityUnavailability)
    }
    let testDescription: String
    let starts: [TimeInterval]
    let paired: [TimeInterval]
    let known: Known

    init(_ description: String, _ starts: [TimeInterval], _ paired: [TimeInterval], _ known: Known)
    {
      testDescription = description
      self.starts = starts
      self.paired = paired
      self.known = known
    }
  }

  static let rules: [Rule] = [
    Rule("no phone: the newest", [0, 1000], [], .index(1)),
    Rule("a phone between them knows the older", [1000, 0], [500], .index(1)),
    Rule("a phone at a start counts as after it", [0, 1000], [1000], .index(1)),
    Rule("a phone a fraction before a start predates it", [0, 1000], [999.9], .index(0)),
    Rule("a phone before every start", [0, 1000], [-1], .refused(.replaced)),
    Rule("phones that know different ones", [0, 1000], [500, 1500], .refused(.partlyReplaced)),
    Rule("starts in one whole second tie", [0.3, 0.8], [], .refused(.replaced)),
    Rule("a phone after a tie knows neither", [0.3, 0.8], [3600], .refused(.replaced)),
    Rule("starts a second apart do not tie", [0, 1], [], .index(1)),
    Rule("a phone between starts a second apart", [0, 1], [0.5], .index(0)),
    Rule("a tie below a newer start, no phone", [1000, 1000.5, 2000], [], .index(2)),
    Rule("a tie below a newer start, a phone after it", [1000, 1000.5, 2000], [2500], .index(2)),
    Rule(
      "a tie below a newer start, a phone between", [1000, 1000.5, 2000], [1500],
      .refused(.replaced)),
    Rule(
      "a phone between, one after a tie", [0, 1000, 1000.5], [500, 1500],
      .refused(.partlyReplaced)),
    Rule(
      "a phone before all, one after a tie", [0, 1000, 1000.5], [-500, 1500],
      .refused(.replaced)),
    Rule("a start counts from its whole second, as in Rust", [1000.9], [1000.2], .index(0)),
  ]

  /// The pairing-time rule alone (`knownByEveryPhone`), with starts in
  /// whole seconds: two in one second tie, and a phone whose newest start
  /// they share knows neither. Rust: the doctest of
  /// `HandoverIdentity::known_by_every_phone`.
  @Test(arguments: rules)
  func thePairingTimeRuleDecidesInWholeSeconds(_ rule: Rule) {
    let base = Date(timeIntervalSince1970: 1_800_000_000)
    let starts = rule.starts.map { base + $0 }
    let paired = rule.paired.map { base + $0 }
    switch rule.known {
    case .index(let index):
      #expect(
        (try? HandoverIdentityGuard.knownByEveryPhone(starts: starts, paired: paired)) == index)
    case .refused(let refusal):
      #expect(throws: refusal) {
        try HandoverIdentityGuard.knownByEveryPhone(starts: starts, paired: paired)
      }
    }
  }

  /// The record of an identity neither test identity is, so Pair Again
  /// over another identity can install `other` in place of `known`.
  static let previous = String(repeating: "ab", count: 32)

  /// The keychain Pair Again starts from, over `previous` and `phones`
  /// paired phones: empty (`missing`) or holding `known` (`replaced`).
  func pairing(replaced: Bool, phones: Int = 2) async throws -> (
    FakeKeychain, HandoverIdentityGuard
  ) {
    let keychain = FakeKeychain(replaced ? known : nil, next: other)
    let identityGuard = try await identityGuard(keychain, phones: phones)
    try identityGuard.record.record(Self.previous)
    return (keychain, identityGuard)
  }

  func refusal(replaced: Bool) -> IdentityUnavailability { replaced ? .replaced : .missing }

  @Test(arguments: [false, true])
  func pairAgainRemovesThePhonesAndInstallsANewIdentity(replaced: Bool) async throws {
    let (keychain, identityGuard) = try await pairing(replaced: replaced)
    var questions = 0
    let identity = try await identityGuard.loadOrPairAgain(commonName: "A") {
      questions += 1
      return true
    }
    #expect(questions == 1)
    #expect(hex(identity) == hex(other))
    #expect(try await store.pairedDevices().isEmpty)
    #expect(try identityGuard.record.recorded() == hex(other))
    #expect(hex(try #require(keychain.identity)) == hex(other) && keychain.stagedIdentity == nil)
    #expect(keychain.stages == 1 && keychain.promotes == 1)
    #expect(hex(try await identityGuard.loadOrCreate(commonName: "A")) == hex(other))
  }

  /// The keychain write comes first: refused, it leaves the phones, the
  /// record and the identity as they were.
  @Test(arguments: [false, true])
  func aRefusedKeychainWriteChangesNothing(replaced: Bool) async throws {
    let (keychain, identityGuard) = try await pairing(replaced: replaced)
    keychain.stageError = Refused()
    let bytes = try Data(contentsOf: identityGuard.record.url)
    await #expect(throws: Refused.self) {
      try await identityGuard.loadOrPairAgain(commonName: "A") { true }
    }
    #expect(try await store.pairedDevices().count == 2)
    #expect(try Data(contentsOf: identityGuard.record.url) == bytes)
    #expect(keychain.identity.map(hex) == (replaced ? hex(known) : nil))
    #expect(keychain.promotes == 0)
  }

  /// Pair Again cut short after the stage, before the first phone or
  /// partway through them: the record and the identity are as they were,
  /// so the next launch asks again and the staged identity is ignored; the
  /// next Pair Again replaces it.
  @Test(arguments: [false, true], ["Phone 0", "Phone 1"])
  func aPairAgainCutShortAmongThePhonesAsksAgain(replaced: Bool, refused: String) async throws {
    let (keychain, identityGuard) = try await pairing(replaced: replaced)
    let bytes = try Data(contentsOf: identityGuard.record.url)
    try await refuseToRemove(refused)
    await #expect(throws: (any Error).self) { try await identityGuard.pairAgain(commonName: "A") }
    #expect(try await store.pairedDevices().isEmpty == false)
    #expect(try Data(contentsOf: identityGuard.record.url) == bytes)
    #expect(keychain.identity.map(hex) == (replaced ? hex(known) : nil))
    #expect(keychain.stagedIdentity.map(hex) == hex(other) && keychain.promotes == 0)

    await #expect(throws: refusal(replaced: replaced)) {
      try await identityGuard.loadOrCreate(commonName: "A")
    }
    #expect(keychain.stages == 1 && keychain.promotes == 0)

    try await store.writer.write { db in try db.execute(sql: "DROP TRIGGER refuse") }
    #expect(hex(try await identityGuard.pairAgain(commonName: "A")) == hex(other))
    #expect(keychain.stages == 2 && keychain.promotes == 1)
    #expect(try await store.pairedDevices().isEmpty)
  }

  /// Cut short after the phones, before the record: the record still names
  /// the identity the keychain lost, so the launch asks again.
  @Test(arguments: [false, true])
  func aPairAgainCutShortBeforeTheRecordAsksAgain(replaced: Bool) async throws {
    let (keychain, identityGuard) = try await pairing(replaced: replaced, phones: 0)
    keychain.stagedIdentity = other
    let bytes = try Data(contentsOf: identityGuard.record.url)
    await #expect(throws: refusal(replaced: replaced)) {
      try await identityGuard.loadOrCreate(commonName: "A")
    }
    #expect(keychain.stages == 0 && keychain.promotes == 0)
    #expect(keychain.identity.map(hex) == (replaced ? hex(known) : nil))
    #expect(try Data(contentsOf: identityGuard.record.url) == bytes)
  }

  /// A staged item that cannot be read (a certificate whose key is gone)
  /// is no identity to finish with: the launch is missing or replaced as
  /// without it, and Pair Again stages over it.
  @Test(arguments: [false, true])
  func aStagedItemThatCannotBeReadIsIgnored(replaced: Bool) async throws {
    let (keychain, identityGuard) = try await pairing(replaced: replaced)
    keychain.stagedError = IdentityError.malformed("no private key")
    await #expect(throws: refusal(replaced: replaced)) {
      try await identityGuard.loadOrCreate(commonName: "A")
    }
    #expect(keychain.stages == 0 && keychain.promotes == 0)
    #expect(try await store.pairedDevices().count == 2)

    #expect(hex(try await identityGuard.loadOrPairAgain(commonName: "A") { true }) == hex(other))
    #expect(keychain.stages == 1 && keychain.promotes == 1)
    #expect(try identityGuard.record.recorded() == hex(other))
  }

  /// The same with no record (phones paired without one): the staged
  /// identity is installed, not a new one minted.
  @Test func aStagedIdentityWithNoRecordAndNoPhoneIsInstalledNotMinted() async throws {
    let keychain = FakeKeychain(nil, next: known)
    keychain.stagedIdentity = other
    let identityGuard = try await identityGuard(keychain)
    #expect(hex(try await identityGuard.loadOrCreate(commonName: "A")) == hex(other))
    #expect(keychain.stages == 0 && keychain.promotes == 1)
    #expect(hex(try #require(keychain.identity)) == hex(other))
    #expect(try identityGuard.record.recorded() == hex(other))
  }

  /// With a paired phone instead (Pair Again on an upgrade with no record
  /// yet, cut short after the stage or among the phones), the phones still
  /// pin the identity the keychain lost: the launch asks and leaves the
  /// staged identity as it is, and Pair Again stages over it.
  @Test func aStagedIdentityWithNoRecordAndAPairedPhoneAsks() async throws {
    let keychain = FakeKeychain(nil, next: known)
    keychain.stagedIdentity = other
    let identityGuard = try await identityGuard(keychain, phones: 2)
    await #expect(throws: IdentityUnavailability.missing) {
      try await identityGuard.loadOrCreate(commonName: "A")
    }
    #expect(keychain.stages == 0 && keychain.promotes == 0 && keychain.identity == nil)
    #expect(keychain.stagedIdentity.map(hex) == hex(other))
    #expect(try identityGuard.record.recorded() == nil)
    #expect(try await store.pairedDevices().count == 2)

    #expect(hex(try await identityGuard.loadOrPairAgain(commonName: "A") { true }) == hex(known))
    #expect(keychain.stages == 1 && keychain.promotes == 1)
    #expect(try identityGuard.record.recorded() == hex(known))
    #expect(try await store.pairedDevices().isEmpty)
  }

  /// A recorded identity with a leftover staged one beside it (Pair Again
  /// cut short after the stage, then the old keychain added back): the
  /// launch loads the recorded one, asks nothing and writes nothing, so no
  /// promote's delete reaches the identity the phones know.
  @Test(arguments: [0, 1])
  func aRecordedIdentityWithALeftoverStagedOneLoadsAndWritesNothing(phones: Int) async throws {
    let keychain = FakeKeychain(known, next: other)
    keychain.stagedIdentity = other
    let identityGuard = try await identityGuard(keychain, recorded: known, phones: phones)
    let bytes = try Data(contentsOf: identityGuard.record.url)
    var asked = false
    let identity = try await identityGuard.loadOrPairAgain(commonName: "A") {
      asked = true
      return true
    }
    #expect(hex(identity) == hex(known) && !asked)
    #expect(keychain.stages == 0 && keychain.promotes == 0)
    #expect(keychain.identity.map(hex) == hex(known))
    #expect(keychain.stagedIdentity.map(hex) == hex(other))
    #expect(try Data(contentsOf: identityGuard.record.url) == bytes)
    #expect(try await store.pairedDevices().count == phones)
  }

  /// Cut short after the record, before the promote: the staged identity
  /// is the recorded one, so the next launch promotes it.
  @Test(arguments: [false, true])
  func aPairAgainCutShortBeforeThePromoteFinishesAtTheNextLaunch(replaced: Bool) async throws {
    let (keychain, identityGuard) = try await pairing(replaced: replaced)
    keychain.promoteError = Refused()
    await #expect(throws: Refused.self) { try await identityGuard.pairAgain(commonName: "A") }
    #expect(try await store.pairedDevices().isEmpty)
    #expect(try identityGuard.record.recorded() == hex(other))
    #expect(keychain.identity.map(hex) == (replaced ? hex(known) : nil))

    keychain.promoteError = nil
    #expect(hex(try await identityGuard.loadOrCreate(commonName: "A")) == hex(other))
    #expect(keychain.stages == 1 && keychain.promotes == 1)
    #expect(hex(try #require(keychain.identity)) == hex(other))
  }

  @Test func aDeclinedPairAgainChangesNothing() async throws {
    let keychain = FakeKeychain(nil, next: other)
    let identityGuard = try await identityGuard(keychain, recorded: known, phones: 1)
    let bytes = try Data(contentsOf: identityGuard.record.url)
    await #expect(throws: IdentityUnavailability.missing) {
      try await identityGuard.loadOrPairAgain(commonName: "A") { false }
    }
    #expect(keychain.identity == nil && keychain.stages == 0 && keychain.promotes == 0)
    #expect(try Data(contentsOf: identityGuard.record.url) == bytes)
    #expect(try await store.pairedDevices().count == 1)
  }

  @Test func aSettingsSaveKeepsTheRecord() async throws {
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

  /// Replaced in one rename, in the Rust app's bytes; a damaged file is
  /// an error, never "nothing recorded".
  @Test func theRecordFileReplacesAndRefusesDamage() throws {
    let record = IdentityRecordFile(supportDirectory: directory)
    #expect(try record.recorded() == nil)
    try record.record("ab12")
    try record.record("cd34")
    #expect(
      try String(contentsOf: record.url, encoding: .utf8)
        == "{\n  \"steno.handoverIdentityFingerprint\": \"cd34\"\n}")
    #expect(try record.recorded() == "cd34")
    #expect(try FileManager.default.contentsOfDirectory(atPath: directory.path).count == 1)
    try Data("{".utf8).write(to: record.url)
    #expect(throws: IdentityRecordFile.Unreadable.self) { try record.recorded() }
  }
}
