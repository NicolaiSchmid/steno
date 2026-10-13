import Foundation
import StenoCore
import Testing

@testable import StenoHandover

/// Pair Again's keychain writes, cut short before each one in turn: the
/// stage's delete and store, then the promote's delete (certificates before
/// their keys) and its two relabels. Every next launch either asks again,
/// until the record names the new identity, or loads the new one; none is
/// stuck unable to read the keychain, and none mints. Of two identities
/// under the current label, the recorded one loads, and with nothing
/// recorded the one every paired phone knows, unless a tie or the clock
/// leaves the phones' pairings unclear. The fake keeps
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
  /// The clock of the launches that place phones by date: after every
  /// fixture's start and every pairing placed after them, so only a time
  /// placed after it is a clock error, whatever the day the tests run.
  static let now = Date(timeIntervalSince1970: 1_791_936_000)
  /// The record of an identity neither test identity is.
  static let previous = String(repeating: "ab", count: 32)

  let known: HandoverIdentity
  let other: HandoverIdentity
  /// A third identity, so two can sit under the current label while `other`
  /// is the staged or the minted one.
  let third: HandoverIdentity
  /// Minted to start in the same second as `known`, and as `third`.
  let tiedWithKnown: HandoverIdentity
  let tiedWithThird: HandoverIdentity
  /// Minted to start at `now`, and a day after it.
  let startsNow: HandoverIdentity
  let startsTomorrow: HandoverIdentity
  let directory: URL

  init() throws {
    known = try TestIdentity.load()
    other = try TestIdentity.load("other-identity")
    third = try TestIdentity.load("third-identity")
    tiedWithKnown = try TestIdentity.minted(startingAt: try known.notValidBefore())
    tiedWithThird = try TestIdentity.minted(startingAt: try third.notValidBefore())
    startsNow = try TestIdentity.minted(startingAt: Self.now)
    startsTomorrow = try TestIdentity.minted(startingAt: Self.now.addingTimeInterval(86400))
    directory = try Fixtures.temporaryDirectory("steno-identity-items")
  }

  deinit { try? FileManager.default.removeItem(at: directory) }

  /// One Mac: a keychain, a record and a store with its paired phones.
  struct Mac {
    let items: FakeItems
    let record: IdentityRecordFile
    let store: MeetingStore

    /// A launch: a fresh guard over the same keychain, record and store.
    var launch: HandoverIdentityGuard { launching { Date() } }

    /// The same, with this Mac's clock reading `now`.
    func launch(at now: Date) -> HandoverIdentityGuard { launching { now } }

    private func launching(_ now: @escaping @Sendable () -> Date) -> HandoverIdentityGuard {
      HandoverIdentityGuard(
        keychain: LabelledIdentityKeychain(
          items: items, label: IdentityItemsTests.label, stagingLabel: IdentityItemsTests.staging),
        record: record, store: store, now: now)
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
  /// given, with `recorded` in its record and one phone paired at each of
  /// `pairedAt`, one paired now by default.
  func mac(
    holding held: [(HandoverIdentity, String)], recorded: String?, pairedAt: [Date] = [Date()]
  ) async throws -> Mac {
    let items = FakeItems(next: other)
    for (identity, label) in held { items.add(identity, label: label) }
    let record = IdentityRecordFile(
      supportDirectory: directory.appendingPathComponent(UUID().uuidString))
    if let recorded { try record.record(recorded) }
    let store = try MeetingStore.inMemory()
    for (index, date) in pairedAt.enumerated() {
      try await store.save(
        PairedDevice(id: UUID(), name: "Phone \(index)", pairedAt: date),
        tokenHash: Data(repeating: UInt8(index + 1), count: 32))
    }
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
  /// first or second in the keychain's order, and the older or the newer
  /// of the two. The launch loads the recorded one and asks nothing, so no
  /// Pair Again runs and no promote's delete removes the identity the
  /// phones know.
  @Test(arguments: [false, true], [false, true])
  func ofTwoIdentitiesUnderTheLabelTheRecordedOneLoads(recordedFirst: Bool, recordedOlder: Bool)
    async throws
  {
    let (recorded, beside) = recordedOlder ? (known, other) : (other, known)
    let held = recordedFirst ? [recorded, beside] : [beside, recorded]
    let mac = try await mac(holding: held.map { ($0, Self.label) }, recorded: hex(recorded))
    var asked = false
    let identity = try await mac.launch.loadOrPairAgain(commonName: "A") {
      asked = true
      return true
    }
    #expect(hex(identity) == hex(recorded) && !asked)
    #expect(mac.items.writes.isEmpty)
    #expect(mac.items.certificateItems.map { hex($0.identity) } == held.map(hex))
    #expect(mac.items.keyItems.map { hex($0.identity) } == held.map(hex))
    #expect(try mac.record.recorded() == hex(recorded))
    #expect(try await mac.store.pairedDevices().count == 1)
  }

  /// The same with a record naming neither is replaced, whichever comes
  /// first.
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

  /// When the phones were paired, against `known` and `third` (older and
  /// newer): each phone knows the newer of the two it was paired after, or
  /// neither when it was paired before both.
  enum Pairing: CaseIterable {
    case none, beforeBoth, between, afterBoth, betweenAndAfter, beforeAndBetween, beforeAndAfter

    enum Point { case beforeBoth, between, afterBoth }

    var points: [Point] {
      switch self {
      case .none: []
      case .beforeBoth: [.beforeBoth]
      case .between: [.between]
      case .afterBoth: [.afterBoth]
      case .betweenAndAfter: [.between, .afterBoth]
      case .beforeAndBetween: [.beforeBoth, .between]
      case .beforeAndAfter: [.beforeBoth, .afterBoth]
      }
    }

    enum Outcome: Equatable {
      case loadsOlder, loadsNewer
      case refused(IdentityUnavailability)
    }

    /// The launch's outcome: the identity every phone knows, the newer
    /// with no phone; `partlyReplaced` when the phones know different ones
    /// or some know none, `replaced` when none knows either.
    var outcome: Outcome {
      switch self {
      case .none, .afterBoth: .loadsNewer
      case .between: .loadsOlder
      case .beforeBoth: .refused(.replaced)
      case .betweenAndAfter, .beforeAndBetween, .beforeAndAfter: .refused(.partlyReplaced)
      }
    }
  }

  /// The same with nothing recorded (an upgrade from a build without the
  /// record, or a record file lost), whichever comes first: the identity
  /// every paired phone knows is recorded, with no question and no other
  /// write. Phones that know different identities, or none, make the
  /// launch ask, and nothing is recorded; a confirmed Pair Again leaves
  /// only the new identity.
  @Test(arguments: [false, true], Pairing.allCases)
  func ofTwoIdentitiesUnderTheLabelWithNoRecordTheOneEveryPhoneKnowsIsRecorded(
    knownFirst: Bool, pairing: Pairing
  ) async throws {
    let held = knownFirst ? [known, third] : [third, known]
    let (older, newer) = (try known.notValidBefore(), try third.notValidBefore())
    let pairedAt = pairing.points.map { point in
      switch point {
      case .beforeBoth: older.addingTimeInterval(-3600)
      case .between: older.addingTimeInterval(3600)
      case .afterBoth: newer.addingTimeInterval(3600)
      }
    }
    let mac = try await mac(
      holding: held.map { ($0, Self.label) }, recorded: nil, pairedAt: pairedAt)
    var asked = false
    switch pairing.outcome {
    case .refused(let refusal):
      await #expect(throws: refusal) {
        try await mac.launch(at: Self.now).loadOrPairAgain(commonName: "A") {
          asked = true
          return false
        }
      }
      #expect(asked)
      #expect(mac.items.writes.isEmpty)
      #expect(mac.items.certificateItems.map { hex($0.identity) } == held.map(hex))
      #expect(try mac.record.recorded() == nil)
      #expect(try await mac.store.pairedDevices().count == pairedAt.count)

      #expect(
        hex(try await mac.launch(at: Self.now).loadOrPairAgain(commonName: "A") { true })
          == hex(other))
      expectOnlyTheNewIdentity(mac.items)
      #expect(try mac.record.recorded() == hex(other))
      #expect(try await mac.store.pairedDevices().isEmpty)
    case .loadsOlder, .loadsNewer:
      let chosen = pairing.outcome == .loadsOlder ? known : third
      let identity = try await mac.launch(at: Self.now).loadOrPairAgain(commonName: "A") {
        asked = true
        return true
      }
      #expect(hex(identity) == hex(chosen) && !asked)
      #expect(mac.items.writes.isEmpty)
      #expect(mac.items.certificateItems.map { hex($0.identity) } == held.map(hex))
      #expect(try mac.record.recorded() == hex(chosen))
      #expect(try await mac.store.pairedDevices().count == pairedAt.count)
    }
  }

  /// Of two under the label with nothing recorded, one without its private
  /// key is reported, whichever comes first: the launch is unreadable,
  /// asks nothing and writes nothing, so no Pair Again sweeps the complete
  /// one the phones may know.
  @Test(arguments: [false, true])
  func ofTwoIdentitiesUnderTheLabelWithNoRecordOneWithoutItsKeyIsUnreadable(keylessFirst: Bool)
    async throws
  {
    let held = keylessFirst ? [known, third] : [third, known]
    let mac = try await mac(holding: held.map { ($0, Self.label) }, recorded: nil)
    mac.items.keyItems.removeAll { hex($0.identity) == hex(known) }
    var asked = false
    let refusal = await #expect(throws: IdentityUnavailability.self) {
      try await mac.launch.loadOrPairAgain(commonName: "A") {
        asked = true
        return true
      }
    }
    guard case .unreadable = refusal else {
      Issue.record("expected unreadable, got \(String(describing: refusal))")
      return
    }
    #expect(!asked)
    #expect(mac.items.writes.isEmpty)
    #expect(try mac.record.recorded() == nil)
    #expect(try await mac.store.pairedDevices().count == 1)
  }

  /// Two identities under the label, neither recorded, beside the staged
  /// one the record names (Pair Again cut short after the record, with an
  /// old login keychain added back): the launch finishes the move without
  /// asking, and the promote sweeps both.
  @Test(arguments: [false, true])
  func ofTwoIdentitiesUnderTheLabelARecordedStagedOneIsPromoted(knownFirst: Bool) async throws {
    let held = knownFirst ? [known, third] : [third, known]
    let mac = try await mac(
      holding: held.map { ($0, Self.label) } + [(other, Self.staging)], recorded: hex(other))
    var asked = false
    let identity = try await mac.launch.loadOrPairAgain(commonName: "A") {
      asked = true
      return false
    }
    #expect(hex(identity) == hex(other) && !asked)
    #expect(!mac.items.writes.contains { $0.hasPrefix("store") })
    expectOnlyTheNewIdentity(mac.items)
    #expect(try mac.record.recorded() == hex(other))
    #expect(try await mac.store.pairedDevices().count == 1)
  }

  /// A launch with nothing recorded, at `now`: the identities under the
  /// label, when the phones were paired, and what the launch does, in every
  /// order of the identities.
  struct Launch: Sendable, CustomTestStringConvertible {
    enum Held: Sendable {
      case known, third, tiedWithKnown, tiedWithThird, startsNow, startsTomorrow
    }
    /// A pairing, seconds after an identity's start or after `now`.
    enum Point: Sendable {
      case after(Held, TimeInterval)
      case afterNow(TimeInterval)
    }
    enum Outcome: Equatable, Sendable {
      case loads(Held)
      case refused(IdentityUnavailability)
    }

    let testDescription: String
    let held: [Held]
    let paired: [Point]
    let outcome: Outcome

    init(_ description: String, _ held: [Held], _ paired: [Point], _ outcome: Outcome) {
      testDescription = description
      self.held = held
      self.paired = paired
      self.outcome = outcome
    }
  }

  func identity(_ held: Launch.Held) -> HandoverIdentity {
    switch held {
    case .known: known
    case .third: third
    case .tiedWithKnown: tiedWithKnown
    case .tiedWithThird: tiedWithThird
    case .startsNow: startsNow
    case .startsTomorrow: startsTomorrow
    }
  }

  func date(_ point: Launch.Point) throws -> Date {
    switch point {
    case .after(let held, let seconds): try identity(held).notValidBefore() + seconds
    case .afterNow(let seconds): Self.now + seconds
    }
  }

  /// `launch` in every order of its identities, each on a fresh Mac.
  func check(_ launch: Launch) async throws {
    for order in orders(launch.held) {
      let held = order.map(identity)
      let mac = try await mac(
        holding: held.map { ($0, Self.label) }, recorded: nil,
        pairedAt: try launch.paired.map(date))
      try await expect(launch.outcome, on: mac, holding: held, at: Self.now, "\(launch) \(order)")
    }
  }

  /// One launch on `mac`, whose keychain holds `held` in that order, with
  /// the clock at `now`. A load records the identity and asks nothing. A
  /// refusal records nothing and asks, declined, except the clock's, which
  /// asks nothing even where the answer would be yes. None writes to the
  /// keychain or removes a phone. After a refusal that asks, a confirmed
  /// Pair Again leaves only the new identity.
  func expect(
    _ outcome: Launch.Outcome, on mac: Mac, holding held: [HandoverIdentity], at now: Date,
    _ comment: Comment
  ) async throws {
    let phones = try await mac.store.pairedDevices().count
    var asked = false
    switch outcome {
    case .loads(let chosen):
      let identity = try await mac.launch(at: now).loadOrPairAgain(commonName: "A") {
        asked = true
        return true
      }
      #expect(hex(identity) == hex(self.identity(chosen)) && !asked, comment)
      #expect(try mac.record.recorded() == hex(self.identity(chosen)), comment)
    case .refused(let refusal):
      await #expect(throws: refusal, comment) {
        try await mac.launch(at: now).loadOrPairAgain(commonName: "A") {
          asked = true
          return refusal == .clockBehind
        }
      }
      #expect(asked == (refusal != .clockBehind), comment)
      #expect(try mac.record.recorded() == nil, comment)
    }
    #expect(mac.items.writes.isEmpty, comment)
    #expect(mac.items.certificateItems.map { hex($0.identity) } == held.map(hex), comment)
    #expect(try await mac.store.pairedDevices().count == phones, comment)
    guard case .refused(let refusal) = outcome, refusal != .clockBehind else { return }
    #expect(
      hex(try await mac.launch(at: now).loadOrPairAgain(commonName: "A") { true }) == hex(other),
      comment)
    expectOnlyTheNewIdentity(mac.items)
    #expect(try mac.record.recorded() == hex(other), comment)
    #expect(try await mac.store.pairedDevices().isEmpty, comment)
  }

  static let ties: [Launch] = [
    Launch("a tie, no phone", [.third, .tiedWithThird], [], .refused(.replaced)),
    Launch(
      "a tie, a phone after it", [.third, .tiedWithThird], [.after(.third, 3600)],
      .refused(.replaced)),
    Launch(
      "a tie, a phone at its start", [.third, .tiedWithThird], [.after(.third, 0)],
      .refused(.replaced)),
    Launch(
      "a tie above an older one, a phone between them", [.known, .third, .tiedWithThird],
      [.after(.known, 3600)], .loads(.known)),
    Launch(
      "a tie above an older one, a phone after the tie", [.known, .third, .tiedWithThird],
      [.after(.third, 3600)], .refused(.replaced)),
    Launch(
      "a tie above an older one, phones between them and after the tie",
      [.known, .third, .tiedWithThird], [.after(.known, 3600), .after(.third, 3600)],
      .refused(.partlyReplaced)),
    Launch(
      "a tie above an older one, phones before both and after the tie",
      [.known, .third, .tiedWithThird], [.after(.known, -3600), .after(.third, 3600)],
      .refused(.replaced)),
    Launch(
      "a tie below a newer one, a phone after the newer one", [.known, .tiedWithKnown, .third],
      [.after(.third, 3600)], .loads(.third)),
    Launch(
      "a tie below a newer one, no phone", [.known, .tiedWithKnown, .third], [], .loads(.third)),
    Launch(
      "a tie below a newer one, a phone between them", [.known, .tiedWithKnown, .third],
      [.after(.known, 1)], .refused(.replaced)),
  ]

  /// Two identities that start in the same second are no newest one: a
  /// phone whose newest start they share knows neither, and with no phone
  /// neither is the newest. The launch refuses as the phones dictate,
  /// whichever comes first, so the keychain's order never chooses.
  @Test(arguments: ties)
  func twoIdentitiesThatStartInTheSameSecondAreNeitherRecorded(_ launch: Launch) async throws {
    #expect(try tiedWithThird.notValidBefore() == third.notValidBefore())
    #expect(hex(tiedWithThird) != hex(third))
    try await check(launch)
  }

  static let copies: [Launch] = [
    Launch("a copy, a phone after it", [.known, .known], [.after(.known, 3600)], .loads(.known)),
    Launch("a copy, no phone", [.known, .known], [], .loads(.known)),
    Launch(
      "a copy beside a newer one, a phone between them", [.known, .third, .known],
      [.after(.known, 3600)], .loads(.known)),
    Launch(
      "a copy beside a newer one, a phone after both", [.known, .third, .known],
      [.after(.third, 3600)], .loads(.third)),
  ]

  /// A certificate in two keychains of the search list (an old login
  /// keychain restored and added back) is one identity, not a tie: the
  /// launch records it and asks nothing, so no Pair Again removes phones
  /// that know it.
  @Test(arguments: copies)
  func aCertificateInTwoKeychainsIsOneIdentity(_ launch: Launch) async throws {
    try await check(launch)
  }

  static let phonesAfterNow: [Launch] = [
    Launch("one identity, a phone at now", [.known], [.afterNow(0)], .loads(.known)),
    Launch(
      "one identity, a phone a second after now", [.known], [.afterNow(1)],
      .refused(.clockBehind)),
    Launch(
      "one identity, a phone a day after now", [.known], [.afterNow(86400)],
      .refused(.clockBehind)),
    Launch(
      "two identities, a phone a day after now", [.known, .third], [.afterNow(86400)],
      .refused(.clockBehind)),
    Launch(
      "two identities, a phone between them and one a day after now", [.known, .third],
      [.after(.known, 3600), .afterNow(86400)], .refused(.clockBehind)),
  ]

  /// A phone paired after now: this Mac's clock is behind, now or when the
  /// phone paired, so the dates cannot say which identity it knows. The
  /// launch asks nothing, records nothing and keeps the phones; a phone
  /// paired at now is no clock error.
  @Test(arguments: phonesAfterNow)
  func aPhonePairedAfterNowWaitsAndAsksNothing(_ launch: Launch) async throws {
    try await check(launch)
  }

  static let startsAfterNow: [Launch] = [
    Launch("an identity started at now, no phone", [.startsNow], [], .loads(.startsNow)),
    Launch(
      "an identity started a day after now, no phone", [.startsTomorrow], [],
      .refused(.clockBehind)),
    Launch(
      "an identity started a day after now, a phone at now", [.startsTomorrow], [.afterNow(0)],
      .refused(.clockBehind)),
    Launch(
      "an identity started a day after now, a phone after it", [.startsTomorrow],
      [.after(.startsTomorrow, 3600)], .refused(.clockBehind)),
    Launch(
      "beside an older one, no phone", [.known, .startsTomorrow], [], .refused(.clockBehind)),
    Launch(
      "beside an older one, a phone between them", [.known, .startsTomorrow],
      [.after(.known, 3600)], .refused(.clockBehind)),
    Launch(
      "beside an older one, a phone before both", [.known, .startsTomorrow],
      [.after(.known, -3600)], .refused(.clockBehind)),
  ]

  /// An identity started after now: the same clock error, so the launch
  /// neither records it nor asks to replace it; one started at now is no
  /// clock error.
  @Test(arguments: startsAfterNow)
  func anIdentityStartedAfterNowWaitsAndAsksNothing(_ launch: Launch) async throws {
    try await check(launch)
  }

  /// The wait lasts only until the clock passes those times: on the same
  /// Mac, a launch a day and an hour later decides by the rule, a load or
  /// a question.
  @Test(arguments: [
    (phonesAfterNow[3], 90000.0, Launch.Outcome.loads(.third)),
    (startsAfterNow[1], 90000.0, .loads(.startsTomorrow)),
    (startsAfterNow[2], 90000.0, .refused(.replaced)),
  ])
  func onceTheClockPassesThemTheRuleDecides(
    _ launch: Launch, secondsLater: TimeInterval, later: Launch.Outcome
  ) async throws {
    let held = launch.held.map(identity)
    let mac = try await mac(
      holding: held.map { ($0, Self.label) }, recorded: nil,
      pairedAt: try launch.paired.map(date))
    try await expect(launch.outcome, on: mac, holding: held, at: Self.now, "\(launch)")
    try await expect(
      later, on: mac, holding: held, at: Self.now + secondsLater, "\(launch), later")
  }

  /// A recorded identity loads by its fingerprint, whatever the clock says
  /// of the phones or of its start: no question and no write.
  @Test(arguments: [Launch.Held.known, .startsTomorrow])
  func aRecordedIdentityIgnoresTheClock(_ held: Launch.Held) async throws {
    let recorded = identity(held)
    let mac = try await mac(
      holding: [(recorded, Self.label)], recorded: hex(recorded),
      pairedAt: [Self.now + 86400])
    var asked = false
    let loaded = try await mac.launch(at: Self.now).loadOrPairAgain(commonName: "A") {
      asked = true
      return true
    }
    #expect(hex(loaded) == hex(recorded) && !asked)
    #expect(mac.items.writes.isEmpty)
    #expect(try await mac.store.pairedDevices().count == 1)
  }

  #if canImport(Security)
    /// The product's keychain hands the record on: of two identities under
    /// the label it loads the recorded one, and with nothing recorded it
    /// lists both.
    @Test func theProductsKeychainLoadsTheRecordedOne() throws {
      let items = FakeItems(next: other)
      for identity in [known, third] { items.add(identity, label: Self.label) }
      let store = IdentityKeychain.Store(
        keychain: LabelledIdentityKeychain(
          items: items, label: Self.label, stagingLabel: Self.staging))
      for identity in [known, third] {
        #expect(try store.load(preferring: hex(identity)).map(hex) == hex(identity))
      }
      #expect(try store.identities().map(hex) == [known, third].map(hex))
    }
  #endif
}

private func hex(_ identity: HandoverIdentity) -> String { ContentHash.hex(identity.fingerprint) }

/// Every order of `items`.
private func orders<T>(_ items: [T]) -> [[T]] {
  guard items.count > 1 else { return [items] }
  return items.indices.flatMap { index in
    var rest = items
    let first = rest.remove(at: index)
    return orders(rest).map { [first] + $0 }
  }
}
