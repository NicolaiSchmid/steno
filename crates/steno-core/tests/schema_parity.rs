//! Invariant 2 of the plan: the Rust migrations produce the schema the Swift
//! CLI produces. `fixtures/schema.swift.sql` is the normalised dump of a
//! database `steno dev db migrate` created (`scripts/dump-swift-schema.sh`,
//! macOS); the Rust store dumps a fresh database the same way.

use similar::TextDiff;
use steno_core::Store;
use steno_core::store::migrator;

/// Comments out, CRLF to LF, trailing whitespace off: what the generating
/// script and git on Windows might change without changing the schema.
fn normalise(text: &str) -> String {
    text.lines()
        .map(|line| line.trim_end_matches('\r').trim_end())
        .filter(|line| !line.starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

#[test]
fn rust_migrations_produce_the_swift_schema() {
    let store = Store::in_memory().unwrap();
    let swift = normalise(include_str!("fixtures/schema.swift.sql"));
    let rust = normalise(&store.schema_dump().unwrap());
    assert!(
        rust == swift,
        "schema differs from the Swift one; regenerate the fixture with \
         scripts/dump-swift-schema.sh if Migrations.swift changed.\n{}",
        TextDiff::from_lines(&swift, &rust)
            .unified_diff()
            .header("swift", "rust")
    );
}

#[test]
fn identifiers_match_migrations_swift() {
    let identifiers: Vec<_> = migrator::MIGRATIONS
        .iter()
        .map(|migration| migration.identifier)
        .collect();
    assert_eq!(identifiers, ["v1", "v2", "v3", "v4"]);
    let store = Store::in_memory().unwrap();
    assert_eq!(
        store.applied_migrations().unwrap(),
        ["v1", "v2", "v3", "v4"]
    );
}

#[test]
fn reopening_applies_nothing_and_keeps_wal_foreign_keys_and_synchronous_normal() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("nested").join("steno.sqlite");
    drop(Store::open(&path).unwrap());
    let store = Store::open(&path).unwrap();
    assert_eq!(store.applied_migrations().unwrap().len(), 4);
    let (journal, foreign_keys, synchronous): (String, i64, i64) = store
        .read(|connection| {
            Ok((
                connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?,
                connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?,
                connection.query_row("PRAGMA synchronous", [], |row| row.get(0))?,
            ))
        })
        .unwrap();
    assert_eq!(journal, "wal");
    assert_eq!(foreign_keys, 1);
    // NORMAL, as GRDB's `DatabasePool` sets it in WAL mode.
    assert_eq!(synchronous, 1);
}

/// A database the Swift app migrated already records every identifier;
/// opening it must not re-run anything. A database with an identifier this
/// build does not know belongs to a newer app and must be left alone.
#[test]
fn recorded_identifiers_are_honoured() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("steno.sqlite");
    {
        // One transaction: on a rollback journal every statement would
        // otherwise commit, and fsync, on its own.
        let mut connection = rusqlite::Connection::open(&path).unwrap();
        let transaction = connection.transaction().unwrap();
        transaction
            .execute_batch("CREATE TABLE grdb_migrations (identifier TEXT NOT NULL PRIMARY KEY)")
            .unwrap();
        for migration in migrator::MIGRATIONS {
            transaction.execute_batch(migration.sql).unwrap();
            transaction
                .execute(
                    "INSERT INTO grdb_migrations (identifier) VALUES (?1)",
                    [migration.identifier],
                )
                .unwrap();
        }
        transaction.commit().unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store.applied_migrations().unwrap(),
        ["v1", "v2", "v3", "v4"]
    );
    drop(store);

    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO grdb_migrations (identifier) VALUES ('v99')",
            [],
        )
        .unwrap();
    let error = Store::open(&path).expect_err("a newer schema is refused");
    assert!(
        matches!(error, steno_core::StoreError::UnknownMigration(ref id) if id == "v99"),
        "{error}"
    );
}
