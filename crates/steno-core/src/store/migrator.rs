//! The migrations as SQL files, applied in order and recorded in
//! `grdb_migrations` exactly as GRDB's `DatabaseMigrator` records them, so
//! the Swift app and this store each consider the other's work applied.
//! Append-only: a new version adds `migrations/00N_vN.sql`, its entry in
//! [`MIGRATIONS`] and the same SQL in `Migrations.swift`, in one PR.
//! An applied identifier this build does not know is ignored with a
//! warning (see `migrations/README.md`).

use rusqlite::{Connection, TransactionBehavior};

use super::{Result, StoreError, query_all};

/// One schema version: its GRDB identifier and the SQL.
#[derive(Debug)]
pub struct Migration {
    pub identifier: &'static str,
    pub sql: &'static str,
}

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        identifier: "v1",
        sql: include_str!("../../migrations/001_v1.sql"),
    },
    Migration {
        identifier: "v2",
        sql: include_str!("../../migrations/002_v2.sql"),
    },
    Migration {
        identifier: "v3",
        sql: include_str!("../../migrations/003_v3.sql"),
    },
    Migration {
        identifier: "v4",
        sql: include_str!("../../migrations/004_v4.sql"),
    },
    Migration {
        identifier: "v5",
        sql: include_str!("../../migrations/005_v5.sql"),
    },
];

/// GRDB's own table, DDL for DDL.
const MIGRATIONS_TABLE: &str =
    "CREATE TABLE IF NOT EXISTS grdb_migrations (identifier TEXT NOT NULL PRIMARY KEY)";

/// The identifiers recorded so far, in application order.
pub(crate) fn applied(connection: &Connection) -> Result<Vec<String>> {
    query_all(
        connection,
        "SELECT identifier FROM grdb_migrations ORDER BY rowid",
        [],
        |row| row.get(0),
    )
}

/// Applies every migration not yet recorded, each in its own `IMMEDIATE`
/// transaction with foreign keys off and a foreign key check before the
/// commit: a violating migration rolls back with
/// [`StoreError::ForeignKeyViolations`]. The transaction covers the read of
/// what is applied, so two processes opening a fresh database at once
/// cannot both apply the same version.
pub(crate) fn migrate(connection: &mut Connection) -> Result<()> {
    migrate_with(connection, MIGRATIONS)
}

/// Applies nothing: fails when the database lacks a migration this build
/// would apply ([`StoreError::PendingMigration`]) and ignores unknown ones.
pub(crate) fn check(connection: &Connection) -> Result<()> {
    check_with(connection, MIGRATIONS)
}

fn check_with(connection: &Connection, migrations: &[Migration]) -> Result<()> {
    let has_table: bool = connection.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'grdb_migrations'",
        [],
        |row| row.get::<_, i64>(0).map(|count| count > 0),
    )?;
    let applied = if has_table {
        applied(connection)?
    } else {
        Vec::new()
    };
    ignore_unknown(&applied, migrations);
    match migrations
        .iter()
        .find(|migration| !applied.iter().any(|a| a == migration.identifier))
    {
        Some(pending) => Err(StoreError::PendingMigration(pending.identifier.to_owned())),
        None => Ok(()),
    }
}

/// Logs a warning for each of `applied` that `migrations` lacks: a newer
/// build migrated the database. Safe while every migration only adds
/// tables and columns (with a default or allowing null) and never alters,
/// drops or constrains an existing one.
fn ignore_unknown(applied: &[String], migrations: &[Migration]) {
    let known = |identifier: &str| migrations.iter().any(|m| m.identifier == identifier);
    for identifier in applied.iter().filter(|identifier| !known(identifier)) {
        tracing::warn!(
            target: "steno::store",
            "the database records migration {identifier}, which this version does not know; \
             a newer version migrated it, and this one leaves it as it is"
        );
    }
}

/// [`migrate`] over an explicit list, for tests.
pub(crate) fn migrate_with(connection: &mut Connection, migrations: &[Migration]) -> Result<()> {
    connection.execute_batch(MIGRATIONS_TABLE)?;
    let applied = applied(connection)?;
    ignore_unknown(&applied, migrations);
    if migrations
        .iter()
        .all(|migration| applied.iter().any(|a| a == migration.identifier))
    {
        return Ok(());
    }

    // `PRAGMA foreign_keys` is a no-op inside a transaction, so it is
    // switched around the whole run, as GRDB does.
    connection.pragma_update(None, "foreign_keys", false)?;
    let outcome = apply(connection, migrations);
    connection.pragma_update(None, "foreign_keys", true)?;
    outcome
}

fn apply(connection: &mut Connection, migrations: &[Migration]) -> Result<()> {
    for migration in migrations {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let recorded: bool = transaction.query_row(
            "SELECT count(*) FROM grdb_migrations WHERE identifier = ?1",
            [migration.identifier],
            |row| row.get::<_, i64>(0).map(|count| count > 0),
        )?;
        if recorded {
            continue;
        }
        transaction.execute_batch(migration.sql)?;
        transaction.execute(
            "INSERT INTO grdb_migrations (identifier) VALUES (?1)",
            [migration.identifier],
        )?;
        // One row per violating row, its `"table"` column the child table.
        let violations = query_all(
            &transaction,
            "SELECT \"table\" FROM pragma_foreign_key_check",
            [],
            |row| row.get::<_, String>(0),
        )?;
        if let Some(table) = violations.first() {
            // Dropping the transaction rolls it back.
            return Err(StoreError::ForeignKeyViolations {
                table: table.clone(),
                count: i64::try_from(violations.len()).unwrap_or(i64::MAX),
            });
        }
        transaction.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A version whose rows break a foreign key, rolled back before the
    /// commit.
    const BROKEN: &[Migration] = &[
        Migration {
            identifier: "v1",
            sql: "CREATE TABLE parent (id INTEGER PRIMARY KEY);
                  CREATE TABLE child (id INTEGER PRIMARY KEY,
                      parentID INTEGER NOT NULL REFERENCES parent(id));",
        },
        Migration {
            identifier: "v2",
            sql: "INSERT INTO child (id, parentID) VALUES (1, 42), (2, 43);",
        },
    ];

    const GOOD: &[Migration] = &[
        Migration {
            identifier: "v1",
            sql: "CREATE TABLE first (id INTEGER PRIMARY KEY);",
        },
        Migration {
            identifier: "v2",
            sql: "CREATE TABLE second (id INTEGER PRIMARY KEY);",
        },
    ];

    /// The check applies nothing, names the first version missing and lets
    /// one this build does not know pass.
    #[test]
    fn the_check_names_what_is_missing_ignores_what_is_unknown_and_applies_nothing() {
        let mut connection = Connection::open_in_memory().unwrap();
        assert!(matches!(
            check_with(&connection, BROKEN),
            Err(StoreError::PendingMigration(first)) if first == "v1"
        ));
        migrate_with(&mut connection, &BROKEN[..1]).unwrap();
        assert!(matches!(
            check_with(&connection, BROKEN),
            Err(StoreError::PendingMigration(next)) if next == "v2"
        ));
        assert_eq!(applied(&connection).unwrap(), ["v1"], "nothing applied");
        check_with(&connection, &BROKEN[..1]).unwrap();
        connection
            .execute("INSERT INTO grdb_migrations (identifier) VALUES ('v9')", [])
            .unwrap();
        check_with(&connection, &BROKEN[..1]).unwrap();
        assert!(matches!(
            check_with(&connection, BROKEN),
            Err(StoreError::PendingMigration(next)) if next == "v2"
        ));
        assert_eq!(
            applied(&connection).unwrap(),
            ["v1", "v9"],
            "nothing applied"
        );
    }

    /// A known version counts as applied also when the database records an
    /// unknown one.
    #[test]
    fn an_unknown_migration_does_not_stand_in_for_a_missing_one() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrate_with(&mut connection, &GOOD[..1]).unwrap();
        connection
            .execute("INSERT INTO grdb_migrations (identifier) VALUES ('v9')", [])
            .unwrap();
        migrate_with(&mut connection, GOOD).unwrap();
        assert_eq!(applied(&connection).unwrap(), ["v1", "v9", "v2"]);
        let second: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'second'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(second, 1, "v2 ran");
    }

    /// Opening without migrating reads a database at this build's version,
    /// refuses an older one (naming the version it lacks) without changing
    /// it, and does not create a missing file.
    #[test]
    fn opening_without_migrating_applies_nothing() {
        use crate::Store;
        use crate::testing::{database_one_version_behind, recorded_migrations};

        let directory = tempfile::tempdir().unwrap();
        let current = directory.path().join("current.sqlite");
        drop(Store::open(&current).unwrap());
        let store = Store::open_without_migrating(&current).unwrap();
        assert_eq!(
            store.applied_migrations().unwrap(),
            ["v1", "v2", "v3", "v4", "v5"]
        );

        let behind = directory.path().join("behind.sqlite");
        database_one_version_behind(&behind);
        let error = Store::open_without_migrating(&behind).expect_err("an older schema is refused");
        assert!(
            matches!(error, StoreError::PendingMigration(ref id) if id == "v5"),
            "{error}"
        );
        assert_eq!(
            recorded_migrations(&behind),
            ["v1", "v2", "v3", "v4"],
            "nothing was migrated"
        );

        let missing = directory.path().join("missing.sqlite");
        assert!(Store::open_without_migrating(&missing).is_err());
        assert!(!missing.exists(), "a missing database is not created");
    }

    #[test]
    fn a_migration_breaking_a_foreign_key_rolls_back() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        let error = migrate_with(&mut connection, BROKEN).unwrap_err();
        assert!(
            matches!(
                &error,
                StoreError::ForeignKeyViolations { table, count: 2 } if table == "child"
            ),
            "{error}"
        );
        assert_eq!(
            error.to_string(),
            "2 foreign key violations after migrating, the first in child"
        );
        assert_eq!(applied(&connection).unwrap(), ["v1"]);
        let rows: i64 = connection
            .query_row("SELECT count(*) FROM child", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 0, "v2 was rolled back");
        let foreign_keys: bool = connection
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert!(foreign_keys, "foreign keys are back on after a failed run");
    }
}
