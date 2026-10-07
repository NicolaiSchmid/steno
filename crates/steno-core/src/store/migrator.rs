//! The GRDB migrations as SQL files, applied in order and recorded in
//! `grdb_migrations` exactly as GRDB's `DatabaseMigrator` records them, so
//! either side considers the other's work applied. Append-only: a new
//! version is a new file and a new entry below the last one, written once
//! and applied by both sides until cutover.
//! Swift: `Sources/StenoCore/Storage/Migrations.swift`.
//!
//! A new version is one PR that adds `migrations/00N_vN.sql`, its entry in
//! [`MIGRATIONS`] and the Swift `Step` running the same SQL; neither side
//! ships alone. The order matters: GRDB ignores identifiers it does not
//! know, this module refuses them ([`StoreError::UnknownMigration`]), so a
//! Rust build shipping first would leave the Swift app silently on a newer
//! schema, while a Swift build shipping first only locks the Rust app out
//! until it catches up. `migrations/README.md` has the full procedure.

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

/// The identifiers recorded so far, in application order. A plain read:
/// the table exists once [`migrate`] has run, which every `Store` does
/// before handing out its connection.
pub(crate) fn applied(connection: &Connection) -> Result<Vec<String>> {
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
pub(crate) fn migrate(connection: &mut Connection) -> Result<()> {
    migrate_with(connection, MIGRATIONS)
}

/// Applies nothing: fails when the database records an identifier this
/// build does not know ([`StoreError::UnknownMigration`], as [`migrate`]
/// does) or lacks one this build would apply
/// ([`StoreError::PendingMigration`], the first of them). For a process
/// that must not migrate under another one; see
/// [`Store::open_without_migrating`](super::Store::open_without_migrating).
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
    refuse_unknown(&applied, migrations)?;
    match migrations
        .iter()
        .find(|migration| !applied.iter().any(|a| a == migration.identifier))
    {
        Some(pending) => Err(StoreError::PendingMigration(pending.identifier.to_owned())),
        None => Ok(()),
    }
}

/// [`StoreError::UnknownMigration`] for the first of `applied` that
/// `migrations` lacks: a newer build migrated the database.
fn refuse_unknown(applied: &[String], migrations: &[Migration]) -> Result<()> {
    let known = |identifier: &str| migrations.iter().any(|m| m.identifier == identifier);
    match applied.iter().find(|identifier| !known(identifier)) {
        Some(unknown) => Err(StoreError::UnknownMigration(unknown.clone())),
        None => Ok(()),
    }
}

/// [`migrate`] over an explicit list; tests pass one with a bad migration.
fn migrate_with(connection: &mut Connection, migrations: &[Migration]) -> Result<()> {
    connection.execute_batch(MIGRATIONS_TABLE)?;
    let applied = applied(connection)?;
    refuse_unknown(&applied, migrations)?;
    if applied.len() == migrations.len() {
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

    /// A version whose rows break a foreign key: GRDB rolls it back before
    /// the commit, and so does this migrator.
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

    /// The check applies nothing and names the first version missing, or
    /// the first one this build does not know.
    #[test]
    fn the_check_names_what_is_missing_or_unknown_and_applies_nothing() {
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
        assert!(matches!(
            check_with(&connection, &BROKEN[..1]),
            Err(StoreError::UnknownMigration(newer)) if newer == "v9"
        ));
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
