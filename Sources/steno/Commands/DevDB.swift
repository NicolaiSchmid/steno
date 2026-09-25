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
      let url = try database.url()
      let store = try MeetingStore.onDisk(at: url)
      let applied = try await store.writer.read { db in
        try Migrations.migrator().appliedIdentifiers(db)
      }
      print("\(url.path): \(applied.sorted().joined(separator: ", "))")
    }
  }

  /// Rebuilds both FTS5 indexes from their content tables.
  struct Reindex: AsyncParsableCommand {
    static let configuration = CommandConfiguration(abstract: "Rebuild the full-text indexes.")

    @OptionGroup var database: DatabaseOptions

    func run() async throws {
      let url = try database.url()
      try await MeetingStore.onDisk(at: url).rebuildSearchIndex()
      print("reindexed \(url.path)")
    }
  }
}
