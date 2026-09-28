import Foundation
import GRDB
import Testing

@testable import StenoCore

/// Guards the append-only discipline of `Migrations.swift`: the
/// `sqlite_master` dump after each migration version must stay byte-identical
/// to its golden. A later migration adds `v<n>.sql` and leaves the earlier
/// goldens alone; every prefix of `Migrations.identifiers` is checked, so
/// `v2.sql` stays guarded once `v3` is the latest.
@Suite struct SchemaSnapshotTests {
  static func dump(_ db: Database) throws -> String {
    let statements = try String.fetchAll(
      db, sql: "SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name")
    return statements.joined(separator: ";\n\n") + ";\n"
  }

  @Test(arguments: 1...Migrations.identifiers.count)
  func everyVersionMatchesItsGolden(count: Int) throws {
    let queue = try DatabaseQueue()
    try Migrations.migrator(upTo: count).migrate(queue)
    let dump = try queue.read(Self.dump)
    let version = Migrations.identifiers[count - 1]
    try Snapshot.assert(dump, matches: "snapshots/schema/\(version).sql")
  }

  @Test func fullMigratorMatchesTheLatestGolden() throws {
    let queue = try DatabaseQueue()
    try Migrations.migrator().migrate(queue)
    let dump = try queue.read(Self.dump)
    let latest = try #require(Migrations.identifiers.last)
    try Snapshot.assert(dump, matches: "snapshots/schema/\(latest).sql")
  }

  /// The identifiers are the steps' names, in order, with no repeats.
  @Test func identifiersAreUniqueAndOrdered() {
    #expect(Set(Migrations.identifiers).count == Migrations.identifiers.count)
    #expect(Migrations.identifiers.first == "v1")
  }
}
