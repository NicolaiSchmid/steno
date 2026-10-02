//! The GRDB migrations as SQL files, applied in order and recorded in
//! `grdb_migrations` exactly as GRDB's `DatabaseMigrator` records them, so
//! either side considers the other's work applied. Append-only: a new
//! version is a new file and a new entry below the last one, written once
//! and applied by both sides until cutover.
//! Swift: `Sources/StenoCore/Storage/Migrations.swift`.

use rusqlite::{Connection, TransactionBehavior};

use super::{Result, StoreError, query_all};

/// One schema version: its GRDB identifier and the SQL.
#[derive(Debug)]
pub struct Migration {
    pub identifier: &'static str,
    pub sql: &'static str,
}

/// Every version in registration order.
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
];

/// GRDB's own table, DDL for DDL.
const MIGRATIONS_TABLE: &str =
    "CREATE TABLE IF NOT EXISTS grdb_migrations (identifier TEXT NOT NULL PRIMARY KEY)";

/// The identifiers recorded so far, in application order.
pub fn applied(connection: &Connection) -> Result<Vec<String>> {
    connection.execute_batch(MIGRATIONS_TABLE)?;
    query_all(
        connection,
        "SELECT identifier FROM grdb_migrations ORDER BY rowid",
        [],
        |row| row.get(0),
    )
}

/// Applies every migration not yet recorded, each in its own `IMMEDIATE`
/// transaction with foreign keys off and GRDB's check before the commit:
/// a migration whose rows no longer satisfy their foreign keys rolls back
/// and the open fails with [`StoreError::ForeignKeyViolations`]. Fails
/// before touching anything when the database records an identifier this
/// build does not know.
///
/// The transaction also covers the read of what is applied, so two
/// processes opening a fresh database at once (the Swift app and this one,
/// or two copies of this one) cannot both apply the same version: the
/// second waits on the busy timeout, then finds the identifier recorded
/// and moves on.
pub fn migrate(connection: &mut Connection) -> Result<()> {
    let applied = applied(connection)?;
    let known = |identifier: &str| MIGRATIONS.iter().any(|m| m.identifier == identifier);
    if let Some(unknown) = applied.iter().find(|identifier| !known(identifier)) {
        return Err(StoreError::UnknownMigration(unknown.clone()));
    }
    if applied.len() == MIGRATIONS.len() {
        return Ok(());
    }

    // `PRAGMA foreign_keys` is a no-op inside a transaction, so it is
    // switched around the whole run, as GRDB does.
    connection.pragma_update(None, "foreign_keys", false)?;
    let outcome = apply(connection);
    connection.pragma_update(None, "foreign_keys", true)?;
    outcome
}

fn apply(connection: &mut Connection) -> Result<()> {
    for migration in MIGRATIONS {
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
        let violations: i64 =
            transaction.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
        if violations > 0 {
            // Dropping the transaction rolls it back.
            return Err(StoreError::ForeignKeyViolations(violations));
        }
        transaction.commit()?;
    }
    Ok(())
}
