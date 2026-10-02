//! The GRDB migrations of `Sources/StenoCore/Storage/Migrations.swift` as
//! SQL files, applied in order and recorded in `grdb_migrations` exactly as
//! GRDB's `DatabaseMigrator` records them, so either side considers the
//! other's work applied. Append-only: a new version is a new file and a new
//! entry below the last one, written once and applied by both sides until
//! cutover.

use std::collections::BTreeSet;

use rusqlite::Connection;

use super::{Result, StoreError};

/// One schema version: its GRDB identifier and the SQL.
#[derive(Debug, Clone, Copy)]
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

/// Every identifier in registration order.
#[must_use]
pub fn identifiers() -> Vec<&'static str> {
    MIGRATIONS
        .iter()
        .map(|migration| migration.identifier)
        .collect()
}

/// GRDB's own table, DDL for DDL.
const MIGRATIONS_TABLE: &str =
    "CREATE TABLE IF NOT EXISTS grdb_migrations (identifier TEXT NOT NULL PRIMARY KEY)";

/// The identifiers recorded so far, in application order.
pub fn applied(connection: &Connection) -> rusqlite::Result<Vec<String>> {
    connection.execute_batch(MIGRATIONS_TABLE)?;
    let mut statement =
        connection.prepare("SELECT identifier FROM grdb_migrations ORDER BY rowid")?;
    let rows = statement.query_map([], |row| row.get(0))?;
    rows.collect()
}

/// Applies every migration not yet recorded, each in its own transaction
/// with foreign keys off and a check at the end, as GRDB does. Fails before
/// touching anything when the database records an identifier this build
/// does not know.
pub fn migrate(connection: &mut Connection) -> Result<Vec<&'static str>> {
    let applied: BTreeSet<String> = applied(connection)?.into_iter().collect();
    if let Some(unknown) = applied.iter().find(|identifier| {
        !MIGRATIONS
            .iter()
            .any(|m| m.identifier == identifier.as_str())
    }) {
        return Err(StoreError::UnknownMigration(unknown.clone()));
    }
    let pending: Vec<Migration> = MIGRATIONS
        .iter()
        .copied()
        .filter(|migration| !applied.contains(migration.identifier))
        .collect();
    if pending.is_empty() {
        return Ok(Vec::new());
    }

    connection.pragma_update(None, "foreign_keys", false)?;
    let outcome = apply(connection, &pending);
    connection.pragma_update(None, "foreign_keys", true)?;
    outcome?;
    Ok(pending
        .into_iter()
        .map(|migration| migration.identifier)
        .collect())
}

fn apply(connection: &mut Connection, pending: &[Migration]) -> Result<()> {
    for migration in pending {
        let transaction = connection.transaction()?;
        transaction.execute_batch(migration.sql)?;
        transaction.execute(
            "INSERT INTO grdb_migrations (identifier) VALUES (?1)",
            [migration.identifier],
        )?;
        transaction.commit()?;
    }
    let violations: i64 =
        connection.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    if violations > 0 {
        return Err(StoreError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
            Some(format!(
                "{violations} foreign key violations after migrating"
            )),
        )));
    }
    Ok(())
}
