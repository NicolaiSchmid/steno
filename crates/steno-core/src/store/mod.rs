//! The SQLite store over the file the Swift app writes
//! (`Sources/StenoCore/Storage/MeetingStore.swift`): the same migrations,
//! recorded in `grdb_migrations` the way GRDB records them, the same column
//! encodings, and the read and write paths the pipeline and the host need.
//! UI-specific queries follow with the host module.

mod assets;
mod convert;
mod deliveries;
mod meetings;
pub mod migrator;
mod people;
mod settings;
mod tasks;
mod transcript;

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::{Connection, Params, Row, Transaction};
use thiserror::Error;
use uuid::Uuid;

pub use meetings::DeletedMeeting;

use crate::model::MeetingStateKind;

/// Errors a store call can raise beyond SQLite's own.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSON column: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("meeting {0} not found")]
    MeetingNotFound(Uuid),
    /// `delete_meeting` while the capture writer or the pipeline still holds
    /// the meeting's files.
    #[error("meeting {0} is {1} and cannot be deleted")]
    MeetingBusy(Uuid, MeetingStateKind),
    /// The database has a migration this build does not know: a newer app
    /// wrote it, and this one must not touch it.
    #[error("the database was migrated by a newer version ({0})")]
    UnknownMigration(String),
    /// GRDB's check after migrating: the schema went in but the rows no
    /// longer satisfy its foreign keys.
    #[error("{0} foreign key violations after migrating")]
    ForeignKeyViolations(i64),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// The one store over the database. One connection behind a mutex: SQLite
/// serialises writes anyway, and the host reads on the same thread pool.
pub struct Store {
    connection: Mutex<Connection>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

impl Store {
    /// Opens (creating) the database at `path` in WAL mode with foreign keys
    /// on and a five-second busy timeout, as GRDB's `DatabasePool` does, and
    /// applies every pending migration. The parent directory is created.
    pub fn open(path: impl AsRef<Path>) -> Result<Store> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        Self::new(connection)
    }

    /// A private in-memory database; tests use this.
    pub fn in_memory() -> Result<Store> {
        Self::new(Connection::open_in_memory()?)
    }

    fn new(mut connection: Connection) -> Result<Store> {
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        migrator::migrate(&mut connection)?;
        Ok(Store {
            connection: Mutex::new(connection),
        })
    }

    /// A panic while a caller held the connection has already rolled its
    /// transaction back, so a poisoned lock is reused, not an error.
    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `body` on the connection outside a transaction.
    pub fn read<T>(&self, body: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        body(&self.lock())
    }

    /// Runs `body` inside one transaction, committed when it returns `Ok`.
    pub fn write<T>(&self, body: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut connection = self.lock();
        let transaction = connection.transaction()?;
        let value = body(&transaction)?;
        transaction.commit()?;
        Ok(value)
    }

    /// The migration identifiers recorded in `grdb_migrations`, in
    /// application order.
    pub fn applied_migrations(&self) -> Result<Vec<String>> {
        self.read(migrator::applied)
    }

    /// The schema in the normalised form `scripts/dump-swift-schema.sh`
    /// writes to `tests/fixtures/schema.swift.sql`: one line per
    /// `sqlite_master` row, then the applied migrations.
    pub fn schema_dump(&self) -> Result<String> {
        self.read(|connection| {
            let mut lines = Vec::new();
            for query in SCHEMA_DUMP_QUERIES {
                lines.extend(query_all(connection, query, [], |row| {
                    row.get::<_, String>(0)
                })?);
            }
            Ok(lines.join("\n"))
        })
    }

    /// Rebuilds both FTS5 indexes from their content tables.
    pub fn rebuild_search_index(&self) -> Result<()> {
        self.write(|transaction| {
            transaction.execute_batch(
                "INSERT INTO transcriptSegment_ft(transcriptSegment_ft) VALUES('rebuild');
                 INSERT INTO meeting_ft(meeting_ft) VALUES('rebuild');",
            )?;
            Ok(())
        })
    }
}

/// The two statements of the schema dump; the same text as the query in
/// `scripts/dump-swift-schema.sh`.
pub const SCHEMA_DUMP_QUERIES: &[&str] = &[
    "SELECT type || '|' || name || '|' || tbl_name || '|' || coalesce(sql, '') FROM sqlite_master ORDER BY type, name",
    "SELECT 'migration|' || identifier FROM grdb_migrations ORDER BY rowid",
];

/// Every row of `sql` through `map`: GRDB's `fetchAll`.
fn query_all<T>(
    connection: &Connection,
    sql: &str,
    params: impl Params,
    map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(params, map)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `INSERT INTO table (columns) VALUES (?1, …)`: GRDB's `insert`.
fn insert_sql(table: &str, columns: &str) -> String {
    let placeholders: Vec<String> = (1..=columns.split(',').count())
        .map(|index| format!("?{index}"))
        .collect();
    format!(
        "INSERT INTO {table} ({columns}) VALUES ({})",
        placeholders.join(", ")
    )
}

/// [`insert_sql`] with `ON CONFLICT DO UPDATE` on every column but the
/// first, the primary key: GRDB's `save`, which replaces the row in place
/// when it exists (so the FTS update triggers fire) and inserts it otherwise.
fn upsert_sql(table: &str, columns: &str) -> String {
    let mut names = columns.split(',').map(str::trim);
    let key = names.next().expect("the key column comes first");
    let updates: Vec<String> = names
        .map(|column| format!("{column} = excluded.{column}"))
        .collect();
    format!(
        "{} ON CONFLICT({key}) DO UPDATE SET {}",
        insert_sql(table, columns),
        updates.join(", ")
    )
}
