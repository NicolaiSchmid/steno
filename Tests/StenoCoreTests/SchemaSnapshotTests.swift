import Foundation
import GRDB
import Testing

@testable import StenoCore

/// Guards the append-only discipline of `Migrations.swift`: the
/// `sqlite_master` dump after each migration version must stay byte-identical
/// to its golden. A later migration adds its own `vN.sql` and leaves the
/// earlier goldens alone.
@Suite struct SchemaSnapshotTests {
  static func dump(_ db: Database) throws -> String {
    let statements = try String.fetchAll(
      db, sql: "SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name")
    return statements.joined(separator: ";\n\n") + ";\n"
  }

  @Test func v1MatchesItsGolden() throws {
    let queue = try DatabaseQueue()
    var migrator = DatabaseMigrator()
    migrator.registerMigration("v1", migrate: Migrations.v1)
    try migrator.migrate(queue)
    let dump = try queue.read(Self.dump)
    try Snapshot.assert(dump, matches: "snapshots/schema/v1.sql")
  }

  @Test func v2MatchesItsGolden() throws {
    let queue = try DatabaseQueue()
    var migrator = DatabaseMigrator()
    migrator.registerMigration("v1", migrate: Migrations.v1)
    migrator.registerMigration("v2", migrate: Migrations.v2)
    try migrator.migrate(queue)
    let dump = try queue.read(Self.dump)
    try Snapshot.assert(dump, matches: "snapshots/schema/v2.sql")
  }

  @Test func fullMigratorMatchesTheLatestGolden() throws {
    let queue = try DatabaseQueue()
    try Migrations.migrator().migrate(queue)
    let dump = try queue.read(Self.dump)
    let latest = try #require(Migrations.identifiers.last)
    try Snapshot.assert(dump, matches: "snapshots/schema/\(latest).sql")
  }
}
