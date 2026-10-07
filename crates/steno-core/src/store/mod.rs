//! The SQLite store over the file the Swift app writes: the same
//! migrations, recorded in `grdb_migrations` the way GRDB records them, the
//! same column encodings, and the read and write paths the pipeline and the
//! host need. The queries the windows ask for (`search`, `export`,
//! `speakers_for_meetings`, `recent_persons`) live here too, written with
//! the [`convert`] codecs.
//! Swift: `Sources/StenoCore/Storage/MeetingStore.swift`.
//!
//! The migration procedure both sides follow is in
//! `crates/steno-core/migrations/README.md`.

mod assets;
pub mod convert;
mod deliveries;
mod export;
mod handover;
mod meetings;
pub mod migrator;
mod people;
mod search;
mod settings;
mod tasks;
mod timings;
mod transcript;

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use rusqlite::{Connection, ErrorCode, Params, Row, Transaction, TransactionBehavior};
use thiserror::Error;
use uuid::Uuid;

pub use meetings::DeletedMeeting;
pub use search::{SearchHit, fts5_pattern};
pub use timings::StageRateRow;

use crate::model::MeetingStateKind;

/// Errors a store call can raise beyond SQLite's own, plus the two the
/// migrator adds. Swift: `MeetingStoreError` in
/// `Sources/StenoCore/Storage/MeetingStore.swift`; the cases the Swift
/// store raises from methods not ported yet are absent here.
#[derive(Debug, Error)]
pub enum StoreError {
    /// SQLite's own errors, among them a row this build cannot decode:
    /// rusqlite's `FromSqlConversionFailure` and `InvalidColumnType` carry
    /// an enum text this build does not know, an invalid UUID, a JSON
    /// column that does not parse, a malformed embedding blob or an
    /// unparsable date. Such a row was written by another build (or by
    /// hand); the call itself was sound.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// The `setting` path only: its rows are merged into one JSON object
    /// and decoded as [`Settings`](crate::model::Settings) outside SQLite,
    /// so a value that does not fit fails here, not as a column
    /// conversion. Every other JSON column goes through
    /// [`convert::DbJson`] and surfaces as [`StoreError::Sqlite`].
    #[error("JSON column: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("meeting {0} not found")]
    MeetingNotFound(Uuid),
    #[error("speaker {0} not found")]
    SpeakerNotFound(Uuid),
    #[error("person {0} not found")]
    PersonNotFound(Uuid),
    #[error("speakers {0} and {1} belong to different meetings")]
    SpeakersInDifferentMeetings(Uuid, Uuid),
    /// `resolve_person` with nothing but whitespace.
    #[error("a person needs a name")]
    BlankPersonName,
    /// `delete_meeting` while the capture writer or the pipeline still holds
    /// the meeting's files.
    #[error("meeting {0} is {1} and cannot be deleted")]
    MeetingBusy(Uuid, MeetingStateKind),
    /// The phone intake's admission of a recording whose receipt belongs
    /// to another device, raised at the intake's own read and by
    /// `save_admission_durably`: the admitting phone was revoked and
    /// another one announced the same recording id.
    #[error("recording {0} belongs to another device")]
    ReceiptOfAnotherDevice(Uuid),
    /// The database has a migration this build does not know: a newer app
    /// wrote it, and this one must not touch it.
    #[error("the database was migrated by a newer version ({0})")]
    UnknownMigration(String),
    /// [`Store::open_without_migrating`] only: the database lacks a
    /// migration this build would apply (the payload, the first of them),
    /// so an older app still runs on it, or the app is still migrating it.
    /// Rust only.
    #[error("the database lacks migration {0}, which this version would apply")]
    PendingMigration(String),
    /// GRDB's check before a migration commits: its rows no longer satisfy
    /// the foreign keys, so it was rolled back and the database is as it
    /// was. `count` is every violating row, `table` the one the first of
    /// them is in.
    #[error("{count} foreign key violations after migrating, the first in {table}")]
    ForeignKeyViolations { table: String, count: i64 },
    /// `PRAGMA journal_mode = WAL` left the file in another mode. SQLite
    /// does that when the VFS has no shared memory, which is what a
    /// network volume gets (SQLite's `unix-none`); the file stays on its
    /// rollback journal, and the store refuses it rather than run two
    /// processes on one. The mode the pragma answered with is the payload.
    #[error("the database is in journal mode {0}, not wal")]
    JournalModeNotWal(String),
}

impl StoreError {
    /// Another connection held the lock past the busy timeout
    /// (`SQLITE_BUSY`, its WAL `SQLITE_BUSY_SNAPSHOT` form, or
    /// `SQLITE_LOCKED` from a shared-cache table lock): nothing is wrong
    /// with the call, and a caller that can wait should retry it.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        match self {
            StoreError::Sqlite(rusqlite::Error::SqliteFailure(error, _)) => matches!(
                error.code,
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
            ),
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// How long a connection waits for another process's lock before giving
/// up. Swift: `configuration.busyMode = .timeout(5)` in
/// `MeetingStore.onDisk`; GRDB's own default fails at once.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The one store over the database. One connection behind a mutex: SQLite
/// serialises writes anyway, and the host reads on the same thread pool.
///
/// Every method blocks on the lock and on SQLite; an async host calls them
/// from `spawn_blocking`. The mutex is `std`'s and not reentrant: a `Store`
/// method called from inside a [`Store::read`] or [`Store::write`] closure
/// deadlocks. Query the connection the closure was given instead: inside
/// the crate through the submodules' free functions, outside it with the
/// [`convert`] codecs.
///
/// The five-second busy timeout is a margin, not a guarantee. Measured
/// against the Swift app rebuilding its FTS index over 600,000 segments,
/// Rust writers waited up to 2.5 s of it; a Swift transaction longer than
/// the timeout surfaces here as `database is locked`, which
/// [`StoreError::is_busy`] recognises and the host (WP6) treats as
/// retryable.
pub struct Store {
    connection: Mutex<Connection>,
    /// [`Store::probe_commits`].
    #[cfg(any(test, feature = "testing"))]
    commit_probe: std::sync::OnceLock<CommitProbe>,
}

/// What [`Store::probe_commits`] runs inside each write transaction.
#[cfg(any(test, feature = "testing"))]
type CommitProbe = Box<dyn Fn(&Connection) + Send + Sync>;

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

impl Store {
    /// Opens (creating) the database at `path` and applies every pending
    /// migration. The parent directory is created. The connection is set up
    /// like the Swift app's writer: WAL mode, `synchronous = NORMAL`, foreign
    /// keys on and a five-second busy timeout. With `NORMAL` in WAL mode a
    /// commit waits for an fsync only when it runs a checkpoint or is the
    /// first commit after one, so a power loss or OS crash can roll back
    /// commits that no checkpoint has copied into the database yet; an app
    /// crash loses nothing. A commit that must be on the disk when it
    /// returns goes through [`Store::write_durably`]. `checkpoint_fullfsync`
    /// is on, as in Apple's system SQLite that GRDB uses (the bundled SQLite
    /// here defaults it off), so a checkpoint on Apple platforms flushes the
    /// drive's cache before the WAL it copied can be overwritten, and a
    /// power loss after it cannot undo a durable commit.
    /// Swift: `MeetingStore.onDisk`, whose `DatabasePool` runs GRDB's
    /// `Database.setUpWALMode`.
    ///
    /// Another process (the Swift app, a second copy of this one) may hold
    /// the file at the same time: every write here begins immediate, so one
    /// writer waits for the other on the busy timeout instead of failing.
    pub fn open(path: impl AsRef<Path>) -> Result<Store> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::new(set_up(Connection::open(path)?)?)
    }

    /// Opens the existing database at `path` as [`Store::open`] does, but
    /// applies no migration: for a process that runs beside the app that
    /// holds the database (`steno export` while Steno runs), which must
    /// not change the schema under it. Fails when the file is missing, and
    /// when the database records a migration this build does not know
    /// ([`StoreError::UnknownMigration`]) or lacks one it would apply
    /// ([`StoreError::PendingMigration`]). Rust only: the Swift CLI always
    /// migrated.
    pub fn open_without_migrating(path: impl AsRef<Path>) -> Result<Store> {
        use rusqlite::OpenFlags;
        let flags = OpenFlags::default().difference(OpenFlags::SQLITE_OPEN_CREATE);
        let connection = set_up(Connection::open_with_flags(path, flags)?)?;
        connection.pragma_update(None, "foreign_keys", true)?;
        migrator::check(&connection)?;
        Ok(Self::over(connection))
    }

    /// A private in-memory database; tests use this.
    pub fn in_memory() -> Result<Store> {
        Self::new(Connection::open_in_memory()?)
    }

    fn new(mut connection: Connection) -> Result<Store> {
        connection.pragma_update(None, "foreign_keys", true)?;
        migrator::migrate(&mut connection)?;
        Ok(Self::over(connection))
    }

    /// The store over a connection that is set up and at the current
    /// schema.
    fn over(connection: Connection) -> Store {
        Store {
            connection: Mutex::new(connection),
            #[cfg(any(test, feature = "testing"))]
            commit_probe: std::sync::OnceLock::new(),
        }
    }

    /// A panic while a caller held the connection has already rolled its
    /// transaction back, so a poisoned lock is reused, not an error.
    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `body` inside one deferred transaction, so every query it makes
    /// sees the same snapshot of the file (an export's seven selects read
    /// one meeting, not a meeting another process is rewriting between
    /// them), as GRDB's `reader.read` did. Nothing is written; the
    /// transaction is rolled back when `body` returns. `body` must not
    /// begin a transaction of its own: SQLite does not nest them, so its
    /// `BEGIN` fails.
    pub fn read<T>(&self, body: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.snapshot(|transaction| body(transaction))
    }

    /// [`Store::read`] for a body that wants the transaction itself, so its
    /// signature says every query runs inside the one snapshot.
    fn snapshot<T>(&self, body: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut connection = self.lock();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let value = body(&transaction)?;
        transaction.rollback()?;
        Ok(value)
    }

    /// Runs `body` inside one transaction, committed when it returns `Ok`.
    /// The transaction begins `IMMEDIATE`, as GRDB's `DatabasePool` writer
    /// does: a deferred one that reads first and writes second cannot be
    /// upgraded once another connection has written in between (WAL's
    /// `SQLITE_BUSY_SNAPSHOT`), and the busy handler does not retry that.
    /// Beginning immediate takes the write lock up front, so the second
    /// writer waits on the busy timeout instead of failing with
    /// `database is locked`.
    pub fn write<T>(&self, body: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        self.commit_on(&mut self.lock(), body)
    }

    /// [`Store::write`] whose commit is on the disk when it returns, for
    /// the commits an answer to another device depends on: the phone
    /// intake's admission, before `complete` tells the phone to delete its
    /// copy; the `failed` receipt after a failed admission commit, which
    /// writes over that commit's frames before the intake removes its copy;
    /// a pairing, whose token the phone keeps; and a revoke.
    /// The transaction runs under `synchronous = FULL` with `fullfsync`
    /// on, so its commit syncs the WAL (with `F_FULLFSYNC` on Apple
    /// platforms, which also flushes the drive's cache) instead of leaving
    /// that to the next checkpoint; the sync covers every earlier commit
    /// in the WAL too. That costs one WAL fsync per commit, plus a flush
    /// of the drive's cache on Apple platforms, all while the lock is held,
    /// so other writes wait for it. SQLite refuses to change `synchronous`
    /// inside a transaction, so the levels are set before `BEGIN` and set
    /// back once the transaction has ended (committed, rolled back or
    /// unwound by a panic), all under one hold of the lock: no other write
    /// runs under them. Swift: `MeetingStore.writeDurably`.
    pub fn write_durably<T>(&self, body: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut full = FullSync::hold(self.lock())?;
        self.commit_on(&mut full.connection, body)
    }

    /// Copies every commit in the WAL into the database file, syncs it, and
    /// starts the WAL over, so a power loss afterwards brings back what the
    /// store reads now. `HandoverService::checkpoint_store` runs it at
    /// launch, before the handover listener starts, and says why.
    /// `PRAGMA wal_checkpoint(RESTART)` under `synchronous = FULL` with
    /// `fullfsync` on copies the frames, syncs the database file and waits
    /// until no reader is left in the WAL. That leaves the WAL file as it
    /// was: after a failed WAL sync it can still hold older frames under
    /// their old salt, which recovery after a power loss would replay over
    /// the checkpointed pages. So one durable write that changes a page
    /// follows, under the same hold of the lock: a private table created
    /// and dropped, which leaves the schema and the applied migrations as
    /// they were. Its commit restarts the WAL with a new salt and syncs the
    /// header and its frames, and recovery skips every frame under the old
    /// salt. A failed sync is an error, and so is a checkpoint that another
    /// connection (a writer, or a reader still in the WAL) blocks when the
    /// busy timeout runs out: SQLite's `SQLITE_BUSY`, which GRDB throws too
    /// and [`StoreError::is_busy`] recognises, since the commits it could
    /// not copy are not known to be on the disk. An in-memory store has no
    /// WAL and returns at once. Swift: `MeetingStore.checkpointDurably`.
    pub fn checkpoint_durably(&self) -> Result<()> {
        let mut full = FullSync::hold(self.lock())?;
        let (blocked, wal_frames): (bool, i64) =
            full.connection
                .query_row("PRAGMA wal_checkpoint(RESTART)", [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?;
        if blocked {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some("another connection blocked the checkpoint".to_owned()),
            )
            .into());
        }
        if wal_frames < 0 {
            return Ok(());
        }
        // A commit that changes no page (an update to the same value)
        // writes no frame and leaves the WAL as it is.
        self.commit_on(&mut full.connection, |transaction| {
            transaction
                .execute_batch("CREATE TABLE stenoWalRestart(x); DROP TABLE stenoWalRestart;")?;
            Ok(())
        })
    }

    /// The `IMMEDIATE` transaction of [`Store::write`] on `connection`,
    /// committed when `body` returns `Ok`.
    fn commit_on<T>(
        &self,
        connection: &mut Connection,
        body: impl FnOnce(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value = body(&transaction)?;
        #[cfg(any(test, feature = "testing"))]
        if let Some(probe) = self.commit_probe.get() {
            probe(&transaction);
        }
        transaction.commit()?;
        Ok(value)
    }

    /// Runs `probe` inside every write transaction from now on, right
    /// before it commits, so a test can read what the commit will write
    /// and the connection's pragmas then. One probe per store; a second
    /// call is ignored. `probe` must not call the store: the lock is held.
    #[cfg(any(test, feature = "testing"))]
    #[doc(hidden)]
    pub fn probe_commits(&self, probe: impl Fn(&Connection) + Send + Sync + 'static) {
        let _ = self.commit_probe.set(Box::new(probe));
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

/// The file connection set up as [`Store::open`] says: the busy timeout,
/// WAL mode, `synchronous = NORMAL` and `checkpoint_fullfsync` on.
fn set_up(connection: Connection) -> Result<Connection> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    enable_wal(&connection)?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "checkpoint_fullfsync", true)?;
    Ok(connection)
}

/// The connection under `synchronous = FULL` and `fullfsync = ON` for as
/// long as it is held; dropping it sets back the levels it found. A
/// transaction borrows it, so the transaction has ended (a dropped one
/// rolls back) before the levels go back, which SQLite requires.
struct FullSync<'a> {
    connection: MutexGuard<'a, Connection>,
    synchronous: i64,
    fullfsync: bool,
}

impl<'a> FullSync<'a> {
    fn hold(connection: MutexGuard<'a, Connection>) -> Result<FullSync<'a>> {
        let synchronous = connection.pragma_query_value(None, "synchronous", |row| row.get(0))?;
        let fullfsync = connection.pragma_query_value(None, "fullfsync", |row| row.get(0))?;
        // Built before the first change, so a failing second pragma still
        // sets the first one back.
        let held = FullSync {
            connection,
            synchronous,
            fullfsync,
        };
        held.connection.pragma_update(None, "synchronous", "FULL")?;
        held.connection.pragma_update(None, "fullfsync", true)?;
        Ok(held)
    }
}

impl Drop for FullSync<'_> {
    fn drop(&mut self) {
        // Neither pragma fails outside a transaction, and the transaction
        // has ended by now.
        let _ = self
            .connection
            .pragma_update(None, "synchronous", self.synchronous);
        let _ = self
            .connection
            .pragma_update(None, "fullfsync", self.fullfsync);
    }
}

/// `PRAGMA journal_mode = WAL`, checked: the pragma answers with the mode
/// the file ended up in, and anything but `wal` is
/// [`StoreError::JournalModeNotWal`]. Switching a fresh file's mode
/// rewrites the header under an exclusive lock, and when two connections
/// have both read the old header and race for it SQLite hands the loser a
/// plain `SQLITE_BUSY` without consulting the busy handler. The loser
/// retries until the winner is done (then the pragma finds WAL set and
/// does nothing) or the busy timeout is spent.
fn enable_wal(connection: &Connection) -> Result<()> {
    let deadline = Instant::now() + BUSY_TIMEOUT;
    loop {
        let outcome = connection.query_row("PRAGMA journal_mode = WAL", [], |row| {
            row.get::<_, String>(0)
        });
        match outcome {
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == ErrorCode::DatabaseBusy && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(mode) if mode.eq_ignore_ascii_case("wal") => return Ok(()),
            Ok(mode) => return Err(StoreError::JournalModeNotWal(mode)),
            Err(error) => return Err(error.into()),
        }
    }
}

/// The two statements of the schema dump; the same text as the query in
/// `scripts/dump-swift-schema.sh`.
const SCHEMA_DUMP_QUERIES: &[&str] = &[
    "SELECT type || '|' || name || '|' || tbl_name || '|' || coalesce(sql, '') FROM sqlite_master ORDER BY type, name",
    "SELECT 'migration|' || identifier FROM grdb_migrations ORDER BY rowid",
];

/// `sql` with `params` through the connection's statement cache: the
/// per-row inserts of a transaction prepare their statement once.
fn execute_cached(connection: &Connection, sql: &str, params: impl Params) -> Result<()> {
    connection.prepare_cached(sql)?.execute(params)?;
    Ok(())
}

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
///
/// `upsert_sql("decision", "id, meetingID, text")` is
/// `INSERT INTO decision (id, meetingID, text) VALUES (?1, ?2, ?3)
/// ON CONFLICT(id) DO UPDATE SET meetingID = excluded.meetingID,
/// text = excluded.text`.
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

// `unix-none` exists only in SQLite's unix VFS set.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// `unix-none` is SQLite's VFS without shared memory, the one a network
    /// volume ends up with: the pragma leaves the file on its rollback
    /// journal and answers `delete`, and the store refuses the file.
    #[test]
    fn a_vfs_without_shared_memory_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("steno.sqlite");
        let connection =
            Connection::open_with_flags_and_vfs(&path, rusqlite::OpenFlags::default(), "unix-none")
                .unwrap();
        let error = enable_wal(&connection).unwrap_err();
        assert!(
            matches!(&error, StoreError::JournalModeNotWal(mode) if mode == "delete"),
            "{error}"
        );
        assert_eq!(
            error.to_string(),
            "the database is in journal mode delete, not wal"
        );
    }
}
