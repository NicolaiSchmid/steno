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
fn reopening_applies_nothing_and_sets_the_grdb_pragmas() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("nested").join("steno.sqlite");
    drop(Store::open(&path).unwrap());
    let store = Store::open(&path).unwrap();
    assert_eq!(store.applied_migrations().unwrap().len(), 4);
    let (journal, foreign_keys, synchronous, busy_timeout): (String, i64, i64, i64) = store
        .read(|connection| {
            Ok((
                connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?,
                connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?,
                connection.query_row("PRAGMA synchronous", [], |row| row.get(0))?,
                connection.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?,
            ))
        })
        .unwrap();
    assert_eq!(journal, "wal");
    assert_eq!(foreign_keys, 1);
    // 1 is NORMAL, which GRDB's `DatabasePool` sets on its writer.
    assert_eq!(synchronous, 1);
    // Milliseconds; `MeetingStore.onDisk`'s `.timeout(5)`.
    assert_eq!(busy_timeout, 5000);
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

/// A database every migration but the newest has reached, in WAL mode as
/// the app leaves it.
fn database_one_version_behind(path: &std::path::Path) {
    let mut connection = rusqlite::Connection::open(path).unwrap();
    let _: String = connection
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    let transaction = connection.transaction().unwrap();
    transaction
        .execute_batch("CREATE TABLE grdb_migrations (identifier TEXT NOT NULL PRIMARY KEY)")
        .unwrap();
    let (_, older) = migrator::MIGRATIONS.split_last().unwrap();
    for migration in older {
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

/// Opening without migrating reads a database at this build's version,
/// refuses one an older build left (naming the version it lacks) and one a
/// newer build migrated, and changes neither; a missing file is not
/// created.
#[test]
fn opening_without_migrating_applies_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let current = directory.path().join("current.sqlite");
    drop(Store::open(&current).unwrap());
    let store = Store::open_without_migrating(&current).unwrap();
    assert_eq!(
        store.applied_migrations().unwrap(),
        ["v1", "v2", "v3", "v4"]
    );

    let behind = directory.path().join("behind.sqlite");
    database_one_version_behind(&behind);
    let error = Store::open_without_migrating(&behind).expect_err("an older schema is refused");
    assert!(
        matches!(error, steno_core::StoreError::PendingMigration(ref id) if id == "v4"),
        "{error}"
    );
    let applied: Vec<String> = rusqlite::Connection::open(&behind)
        .unwrap()
        .prepare("SELECT identifier FROM grdb_migrations ORDER BY rowid")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(applied, ["v1", "v2", "v3"], "nothing was migrated");

    rusqlite::Connection::open(&current)
        .unwrap()
        .execute(
            "INSERT INTO grdb_migrations (identifier) VALUES ('v99')",
            [],
        )
        .unwrap();
    assert!(matches!(
        Store::open_without_migrating(&current),
        Err(steno_core::StoreError::UnknownMigration(ref id)) if id == "v99"
    ));

    let missing = directory.path().join("missing.sqlite");
    assert!(Store::open_without_migrating(&missing).is_err());
    assert!(!missing.exists(), "a missing database is not created");
}
