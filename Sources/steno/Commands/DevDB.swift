import ArgumentParser
import Foundation
import GRDB
import StenoCore

/// `steno dev db migrate|reindex`.
struct DevDB: AsyncParsableCommand {
  static let configuration = CommandConfiguration(
    commandName: "db",
    abstract: "Database maintenance.",
    subcommands: [Migrate.self, Reindex.self]
  )

  /// Opens the database (creating it) and applies every pending migration.
  struct Migrate: AsyncParsableCommand {
    static let configuration = CommandConfiguration(abstract: "Create or migrate the database.")

    @OptionGroup var database: DatabaseOptions

    func run() async throws {
      let opened = try Wiring.open(database)
      let applied = try await opened.store.writer.read { db in
        try Migrations.migrator().appliedIdentifiers(db)
      }
      print("\(try database.url().path): \(applied.sorted().joined(separator: ", "))")
    }
  }

  /// Rebuilds both FTS5 indexes from their content tables.
  struct Reindex: AsyncParsableCommand {
    static let configuration = CommandConfiguration(abstract: "Rebuild the full-text indexes.")

    @OptionGroup var database: DatabaseOptions

    func run() async throws {
      let opened = try Wiring.open(database)
      try await opened.store.rebuildSearchIndex()
      print("reindexed \(try database.url().path)")
    }
  }
}
