//! The SQLite store over the file the Swift app also writes: the same
//! migrations, recorded in `grdb_migrations` as GRDB records them, the same
//! column encodings, and the read and write paths the pipeline and the host
//! need. The migration procedure is in `crates/steno-core/migrations/README.md`.

mod assets;
pub mod convert;
mod deliveries;
mod export;
#[cfg(feature = "tokio")]
mod facade;
mod handover;
mod meetings;
pub mod migrator;
mod people;
mod search;
mod settings;
mod tasks;
mod timings;
mod transcript;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use rusqlite::{Connection, ErrorCode, Params, Row, Transaction, TransactionBehavior};
use thiserror::Error;
use uuid::Uuid;

pub use assets::ExpiredAsset;
pub use handover::AdmissionCommit;
pub use meetings::DeletedMeeting;
pub use search::{SearchHit, fts5_pattern};
pub use settings::{PARAKEET_V3_ENGINE_ID, SettingProblem};
pub use timings::StageRateRow;

use crate::model::MeetingStateKind;

/// Errors a store call can raise beyond SQLite's own, plus the migrator's.
#[derive(Debug, Error)]
pub enum StoreError {
    /// SQLite's own errors, among them a row this build cannot decode (an
    /// unknown enum text, an invalid UUID or JSON, a malformed blob or
    /// date), written by another build or by hand.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// The `setting` path only: the merged rows do not decode as
    /// [`Settings`](crate::model::Settings). Every other JSON column
    /// surfaces as [`StoreError::Sqlite`].
    #[error("JSON column: {0}")]
    Json(#[from] serde_json::Error),
    /// The `setting` path only: the row under `key` cannot be read as that
    /// setting. The value is left out: it can hold a server address or a
    /// folder path, and this text goes into the log and a dialog.
    #[error("the stored setting {key} {problem}")]
    UndecodableSetting {
        key: String,
        problem: SettingProblem,
    },
    /// The `setting` path only: every row reads on its own, but not all of
    /// them together. The text leaves the values out.
    #[error("the stored settings cannot be read together")]
    UndecodableSettings,
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
    /// [`Store::save_stopped_recording`] on a meeting no longer
    /// `recording`: nothing was written.
    #[error("meeting {0} is {1}, not recording")]
    NotRecording(Uuid, MeetingStateKind),
    /// [`Store::insert_meeting_with_asset`] on an id that already has a
    /// row: nothing was written.
    #[error("meeting {0} already exists")]
    MeetingExists(Uuid),
    /// The phone intake's admission of a recording whose receipt belongs to
    /// another device or holds other bytes.
    #[error("the receipt of recording {0} belongs to another upload")]
    ReceiptOfAnotherUpload(Uuid),
    /// [`Store::open_without_migrating`] only: the database lacks a
    /// migration this build would apply (the first of them).
    #[error("the database lacks migration {0}, which this version would apply")]
    PendingMigration(String),
    /// A migration left foreign key violations and was rolled back. `count`
    /// is every violating row, `table` the first one's.
    #[error("{count} foreign key violations after migrating, the first in {table}")]
    ForeignKeyViolations { table: String, count: i64 },
    /// `PRAGMA journal_mode = WAL` left the file in another mode, as on a
    /// network volume without shared memory; the store refuses it rather
    /// than run two processes on a rollback journal.
    #[error("the database is in journal mode {0}, not wal")]
    JournalModeNotWal(String),
    /// [`Store::with_own_reader`] while an earlier handle is still alive.
    #[error("the host's reader is already open")]
    OwnReaderOpen,
}

impl StoreError {
    /// Another connection held the lock past the busy timeout
    /// (`SQLITE_BUSY`, `SQLITE_BUSY_SNAPSHOT` or `SQLITE_LOCKED`): a caller
    /// that can wait should retry.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        matches!(
            self.sqlite_code(),
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
        )
    }

    /// SQLite's result code (`NotADatabase`, `CannotOpen`), which names no
    /// path and no value, so a log line may carry it.
    #[must_use]
    pub fn sqlite_code(&self) -> Option<ErrorCode> {
        match self {
            StoreError::Sqlite(rusqlite::Error::SqliteFailure(error, _)) => Some(error.code),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// How long a connection waits for another process's lock before giving
/// up.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The most reader connections a store opens: the shared reader and the
/// host's own ([`Store::with_own_reader`]).
pub const READERS: usize = 2;

const SHARED_READER: usize = 0;

const OWN_READER: usize = 1;

/// The store over the database: one writer connection and up to
/// [`READERS`] reader connections.
///
/// - **The writer** runs the migrations and every [`Store::write`],
///   [`Store::write_durably`] and [`Store::checkpoint_durably`].
/// - **A reader** runs [`Store::read`] and is `query_only`. In WAL mode it
///   reads its snapshot while another connection writes, so a read never
///   waits for a write. Each reader is one mutex, held only for its
///   closure, and nothing is taken under it except by the checkpoint. The
///   handle [`Store::open`] returns reads on the shared reader; the host's
///   ([`Store::with_own_reader`]) on its own.
/// - **The checkpoint** takes the writer, then the shared reader, then the
///   host's. Under another process's lock it holds both readers up to the
///   busy timeout; its callers are the launch and the recorder's
///   fallbacks, none on a path the UI waits on.
/// - **In memory**, one connection serves every role, since separate
///   connections to `:memory:` do not share a database.
///
/// Every method blocks; async code calls through `Store::reading` and
/// `Store::writing` (feature `tokio`). The mutexes are not reentrant: a
/// `Store` method called inside a [`Store::read`] or [`Store::write`]
/// closure deadlocks (a debug build asserts). Query the closure's
/// connection instead, with the [`convert`] codecs.
///
/// The five-second busy timeout is a margin, not a guarantee: a longer
/// transaction in another process surfaces as `database is locked`, which
/// [`StoreError::is_busy`] recognises as retryable.
pub struct Store {
    connections: Arc<Connections>,
    /// The index in [`Connections::readers`] this handle reads on.
    reader: usize,
}

/// Where a database stands ([`Store::change_mark`]): equal marks from one
/// handle mean nothing committed between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeMark {
    commits: u64,
    /// `PRAGMA data_version` on the handle's reader; 0 in memory.
    data_version: i64,
}

struct Connections {
    writer: Mutex<Connection>,
    /// The database file; `None` in memory, where the writer serves every
    /// role.
    path: Option<PathBuf>,
    /// The shared reader, then the host's; `None` until opened, and always
    /// in memory.
    readers: [Mutex<Option<Connection>>; READERS],
    /// Whether the handle [`Store::with_own_reader`] returned is alive.
    own_reader_open: AtomicBool,
    /// The writer's commits since the store opened ([`Store::change_mark`]).
    commits: Arc<AtomicU64>,
    /// [`Store::probe_commits`].
    #[cfg(any(test, feature = "testing"))]
    commit_probe: std::sync::OnceLock<CommitProbe>,
}

/// What [`Store::probe_commits`] runs inside each write transaction.
#[cfg(any(test, feature = "testing"))]
type CommitProbe = Box<dyn Fn(&Connection) + Send + Sync>;

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("own_reader", &(self.reader == OWN_READER))
            .finish_non_exhaustive()
    }
}

impl Drop for Store {
    /// The host's handle closes its reader, so a new host can open one.
    fn drop(&mut self) {
        if self.reader == OWN_READER {
            drop(lock(&self.connections.readers[OWN_READER]).take());
            self.connections
                .own_reader_open
                .store(false, Ordering::Release);
        }
    }
}

impl Store {
    /// Opens (creating) the database at `path` and its parent directory,
    /// applies every pending migration and backfills the admission ledger.
    /// WAL mode, `synchronous = NORMAL`, foreign keys on, a five-second busy
    /// timeout. With `NORMAL` a power loss can roll back commits no
    /// checkpoint has copied yet (an app crash loses nothing); a commit that
    /// must be on disk goes through [`Store::write_durably`].
    /// `checkpoint_fullfsync` is on (the bundled SQLite defaults it off), so
    /// a checkpoint on Apple platforms flushes the drive's cache before the
    /// WAL it copied can be overwritten.
    ///
    /// Another process may hold the file at the same time: every write
    /// begins immediate, so one writer waits on the busy timeout instead of
    /// failing.
    pub fn open(path: impl AsRef<Path>) -> Result<Store> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::new(set_up(Connection::open(path)?)?, Some(path))
    }

    /// Opens the existing database as [`Store::open`] does, but applies no
    /// migration and no backfill: for a process beside the running app
    /// (`steno export`) that must not change the schema under it. Fails
    /// when the file is missing or lacks a migration this build would apply
    /// ([`StoreError::PendingMigration`]); unknown ones are ignored.
    pub fn open_without_migrating(path: impl AsRef<Path>) -> Result<Store> {
        use rusqlite::OpenFlags;
        let path = path.as_ref();
        let flags = OpenFlags::default().difference(OpenFlags::SQLITE_OPEN_CREATE);
        let connection = set_up(Connection::open_with_flags(path, flags)?)?;
        connection.pragma_update(None, "foreign_keys", true)?;
        migrator::check(&connection)?;
        Self::over(connection, Some(path))
    }

    /// A private in-memory database, for tests.
    pub fn in_memory() -> Result<Store> {
        Self::new(Connection::open_in_memory()?, None)
    }

    /// The store over `connection`, migrated and backfilled; `path` is
    /// `None` in memory.
    fn new(mut connection: Connection, path: Option<&Path>) -> Result<Store> {
        connection.pragma_update(None, "foreign_keys", true)?;
        migrator::migrate(&mut connection)?;
        let store = Self::over(connection, path)?;
        store.backfill_handover_admissions()?;
        Ok(store)
    }

    /// The store over a migrated writer, with the shared reader opened on
    /// `path` and the commit hook counting from now on.
    fn over(writer: Connection, path: Option<&Path>) -> Result<Store> {
        let shared = path.map(open_reader).transpose()?;
        let commits = Arc::new(AtomicU64::new(0));
        let counted = commits.clone();
        // Inside the writer's commit: takes no lock, calls nothing; `false`
        // lets the commit go on.
        writer.commit_hook(Some(move || {
            counted.fetch_add(1, Ordering::AcqRel);
            false
        }))?;
        Ok(Store {
            connections: Arc::new(Connections {
                writer: Mutex::new(writer),
                path: path.map(Path::to_owned),
                readers: [Mutex::new(shared), Mutex::new(None)],
                own_reader_open: AtomicBool::new(false),
                commits,
                #[cfg(any(test, feature = "testing"))]
                commit_probe: std::sync::OnceLock::new(),
            }),
            reader: SHARED_READER,
        })
    }

    /// A second handle whose reads use a reader connection of its own; its
    /// writes and checkpoint are the same writer's. The host holds it, so
    /// its reads never wait for a read elsewhere in the process. One at a
    /// time: while it is alive a second call fails with
    /// [`StoreError::OwnReaderOpen`].
    ///
    /// ```
    /// # fn main() -> steno_core::store::Result<()> {
    /// use steno_core::{Store, StoreError};
    ///
    /// let store = Store::in_memory()?;
    /// let host = store.with_own_reader()?;
    /// assert!(matches!(store.with_own_reader(), Err(StoreError::OwnReaderOpen)));
    /// drop(host);
    /// store.with_own_reader()?.settings()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_own_reader(&self) -> Result<Store> {
        let connections = &self.connections;
        if connections.own_reader_open.swap(true, Ordering::AcqRel) {
            return Err(StoreError::OwnReaderOpen);
        }
        // Built first, so its drop closes the flag again on a failed open.
        let own = Store {
            connections: connections.clone(),
            reader: OWN_READER,
        };
        if let Some(path) = &connections.path {
            *lock(&connections.readers[OWN_READER]) = Some(open_reader(path)?);
        }
        Ok(own)
    }

    /// Where the database stands: two marks read on this handle differ once
    /// anything committed between them. It joins the writer's commit count
    /// (all an in-memory store has) and `PRAGMA data_version` on this
    /// handle's reader, which moves for every other connection's commits,
    /// other processes' included.
    ///
    /// The hook runs just before a commit is visible, so a mark can count a
    /// commit a read still misses; `data_version` moves once it is visible,
    /// so the next mark differs again. A commit that changed nothing, or
    /// failed after the hook, moves it too. Takes this handle's reader for
    /// one pragma, never the writer.
    ///
    /// ```
    /// # fn main() -> steno_core::store::Result<()> {
    /// let store = steno_core::Store::in_memory()?;
    /// let before = store.change_mark()?;
    /// store.settings()?;
    /// assert_eq!(store.change_mark()?, before, "a read moves nothing");
    /// let settings = store.settings()?;
    /// store.save_settings(&settings)?;
    /// assert_ne!(store.change_mark()?, before);
    /// # Ok(())
    /// # }
    /// ```
    pub fn change_mark(&self) -> Result<ChangeMark> {
        let _inside = Inside::enter(&self.connections);
        let data_version = match &self.connections.path {
            None => 0,
            Some(_) => lock(&self.connections.readers[self.reader])
                .as_ref()
                .expect("a file store's handle has its reader open")
                .query_row("PRAGMA data_version", [], |row| row.get(0))?,
        };
        Ok(ChangeMark {
            commits: self.connections.commits.load(Ordering::Acquire),
            data_version,
        })
    }

    /// The writer. A panic while a caller held it already rolled its
    /// transaction back, so a poisoned lock is reused.
    fn writer(&self) -> MutexGuard<'_, Connection> {
        lock(&self.connections.writer)
    }

    /// Runs `body` inside one deferred transaction, so every query sees the
    /// same snapshot, and rolls it back. On this handle's `query_only`
    /// reader, so a write fails with `SQLITE_READONLY`. `body` must not
    /// begin a transaction of its own: SQLite does not nest them.
    pub fn read<T>(&self, body: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.snapshot(|transaction| body(transaction))
    }

    /// [`Store::read`] for a body that wants the transaction itself.
    fn snapshot<T>(&self, body: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let _inside = Inside::enter(&self.connections);
        if self.connections.path.is_none() {
            let mut held = QueryOnly::hold(self.writer())?;
            return read_on(&mut held.connection, body);
        }
        let mut reader = lock(&self.connections.readers[self.reader]);
        let reader = reader
            .as_mut()
            .expect("a file store's handle has its reader open");
        read_on(reader, body)
    }

    /// Runs `body` inside one transaction, committed when it returns `Ok`.
    /// It begins `IMMEDIATE`: a deferred one that reads then writes cannot
    /// be upgraded once another connection wrote in between
    /// (`SQLITE_BUSY_SNAPSHOT`, which the busy handler does not retry);
    /// immediate makes the second writer wait on the busy timeout instead.
    pub fn write<T>(&self, body: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let _inside = Inside::enter(&self.connections);
        self.commit_on(&mut self.writer(), body)
    }

    /// [`Store::write`] whose commit is on the disk when it returns, for
    /// commits another device or a file removal depends on: the phone
    /// intake's admission and its `failed` receipt, a pairing, a revoke,
    /// and [`Store::replace_transcript`] before the old sample clips go.
    /// Runs under `synchronous = FULL` with `fullfsync` (`F_FULLFSYNC` on
    /// Apple platforms), so the commit syncs the WAL and every earlier
    /// commit in it. SQLite refuses to change `synchronous` inside a
    /// transaction, so the levels are set before `BEGIN` and reset once it
    /// ended (even by a panic), all under one hold of the lock.
    pub fn write_durably<T>(&self, body: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let _inside = Inside::enter(&self.connections);
        let mut full = FullSync::hold(self.writer())?;
        self.commit_on(&mut full.connection, body)
    }

    /// Copies every commit in the WAL into the database file, syncs it, and
    /// starts the WAL over, so a power loss afterwards brings back what the
    /// store reads now. Runs at launch before the handover listener starts.
    ///
    /// `wal_checkpoint(TRUNCATE)` under `synchronous = FULL` with
    /// `fullfsync` truncates the WAL, but the truncation is not synced:
    /// after a power loss the old header and frames can come back, and
    /// recovery would replay older pages over the checkpointed ones. So one
    /// durable write follows under the same lock (a private table created
    /// and dropped), whose commit syncs a WAL header with the new salt, so
    /// recovery replays only its frames. A failed sync is an error, and so
    /// is a checkpoint another connection blocks past the busy timeout
    /// (`SQLITE_BUSY`, [`StoreError::is_busy`]). This process's readers
    /// cannot block it: it takes the writer, then both readers. In memory
    /// it takes its one lock and returns.
    pub fn checkpoint_durably(&self) -> Result<()> {
        let _inside = Inside::enter(&self.connections);
        let mut full = FullSync::hold(self.writer())?;
        let _readers = self.connections.readers.each_ref().map(lock);
        let (blocked, wal_frames): (bool, i64) =
            full.connection
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
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
        // A commit that changes no page writes no WAL header; creating a
        // table always changes one.
        self.commit_on(&mut full.connection, |transaction| {
            transaction
                .execute_batch("CREATE TABLE stenoWalRestart(x); DROP TABLE stenoWalRestart;")?;
            Ok(())
        })
    }

    /// The `IMMEDIATE` transaction of [`Store::write`] on `connection`.
    /// `self` carries the commit probe, which only test builds have.
    #[cfg_attr(not(any(test, feature = "testing")), allow(clippy::unused_self))]
    fn commit_on<T>(
        &self,
        connection: &mut Connection,
        body: impl FnOnce(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value = body(&transaction)?;
        #[cfg(any(test, feature = "testing"))]
        if let Some(probe) = self.connections.commit_probe.get() {
            probe(&transaction);
        }
        transaction.commit()?;
        Ok(value)
    }

    /// Runs `probe` inside every write transaction from now on, right
    /// before it commits. One probe per store; a second call is ignored.
    /// `probe` must not call the store: the lock is held.
    #[cfg(any(test, feature = "testing"))]
    #[doc(hidden)]
    pub fn probe_commits(&self, probe: impl Fn(&Connection) + Send + Sync + 'static) {
        let _ = self.connections.commit_probe.set(Box::new(probe));
    }

    /// The writer's connection, outside any transaction, for a test that
    /// tunes it or reads its pragmas. `body` must not call the store.
    #[cfg(any(test, feature = "testing"))]
    #[doc(hidden)]
    pub fn on_writer<T>(&self, body: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let _inside = Inside::enter(&self.connections);
        Ok(body(&self.writer())?)
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

/// The file connection set up as [`Store::open`] says.
fn set_up(connection: Connection) -> Result<Connection> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    enable_wal(&connection)?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "checkpoint_fullfsync", true)?;
    Ok(connection)
}

/// A reader on the file at `path`: set up as the writer is, then
/// `query_only`.
fn open_reader(path: &Path) -> Result<Connection> {
    let connection = set_up(Connection::open(path)?)?;
    connection.pragma_update(None, "query_only", true)?;
    Ok(connection)
}

/// A poisoned connection lock is reused: the panic that poisoned it has
/// already rolled its transaction back.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// [`Store::read`]'s deferred transaction on `connection`, rolled back
/// when `body` returns.
fn read_on<T>(
    connection: &mut Connection,
    body: impl FnOnce(&Transaction<'_>) -> Result<T>,
) -> Result<T> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let value = body(&transaction)?;
    transaction.rollback()?;
    Ok(value)
}

thread_local! {
    /// The databases this thread is inside a store call on ([`Inside`]),
    /// each by the address of its handles' shared [`Connections`].
    static INSIDE: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// Marks the thread as inside a store call on one database while it lives,
/// so a nested `Store` call on that database, on any handle, asserts in a
/// debug build instead of deadlocking. Another database is no nesting.
struct Inside(usize);

impl Inside {
    fn enter(connections: &Arc<Connections>) -> Inside {
        let database = Arc::as_ptr(connections).addr();
        INSIDE.with_borrow_mut(|inside| {
            debug_assert!(
                !inside.contains(&database),
                "a Store call inside a Store closure waits on its own lock"
            );
            inside.push(database);
        });
        Inside(database)
    }
}

impl Drop for Inside {
    fn drop(&mut self) {
        INSIDE.with_borrow_mut(|inside| {
            if let Some(at) = inside.iter().rposition(|database| *database == self.0) {
                inside.remove(at);
            }
        });
    }
}

/// The in-memory connection under `query_only` while held, so
/// [`Store::read`] there fails a write as a reader does.
struct QueryOnly<'a> {
    connection: MutexGuard<'a, Connection>,
}

impl<'a> QueryOnly<'a> {
    fn hold(connection: MutexGuard<'a, Connection>) -> Result<QueryOnly<'a>> {
        connection.pragma_update(None, "query_only", true)?;
        Ok(QueryOnly { connection })
    }
}

impl Drop for QueryOnly<'_> {
    fn drop(&mut self) {
        // Outside a transaction the pragma does not fail.
        let _ = self.connection.pragma_update(None, "query_only", false);
    }
}

/// The connection under `synchronous = FULL` and `fullfsync = ON` while
/// held; dropping it restores the levels it found. A transaction borrows
/// it, so the transaction has ended before the levels go back, as SQLite
/// requires.
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
        // resets the first.
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

/// `PRAGMA journal_mode = WAL`, checked: anything but `wal` is
/// [`StoreError::JournalModeNotWal`]. Two connections racing to switch a
/// fresh file get a plain `SQLITE_BUSY` without the busy handler, so the
/// loser retries until the winner is done or the busy timeout is spent.
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

/// `sql` with `params` through the statement cache.
fn execute_cached(connection: &Connection, sql: &str, params: impl Params) -> Result<()> {
    connection.prepare_cached(sql)?.execute(params)?;
    Ok(())
}

/// Every row of `sql` through `map`.
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

/// `INSERT INTO table (columns) VALUES (?1, …)`.
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
/// first, the primary key: replaces the row in place when it exists (so
/// the FTS update triggers fire) and inserts it otherwise.
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, mpsc};
    use std::thread;

    use super::*;

    /// What "live" means in the reader tests: well under the busy timeout.
    const LIVE: Duration = Duration::from_millis(200);

    fn file_store(directory: &tempfile::TempDir) -> (Arc<Store>, Store, PathBuf) {
        let path = directory.path().join("steno.sqlite");
        let store = Arc::new(Store::open(&path).unwrap());
        let host = store.with_own_reader().unwrap();
        (store, host, path)
    }

    /// A read held open on the shared reader does not hold up the host's.
    #[test]
    fn the_hosts_reads_do_not_wait_for_a_read_on_the_shared_reader() {
        let directory = tempfile::tempdir().unwrap();
        let (store, host, _) = file_store(&directory);
        let (reading, read) = mpsc::channel::<()>();
        let (end, ended) = mpsc::channel::<()>();
        let long_read = thread::spawn({
            let store = store.clone();
            move || {
                store.read(|_| {
                    reading.send(()).unwrap();
                    ended.recv().unwrap();
                    Ok(())
                })
            }
        });
        read.recv().unwrap();
        // On its own thread, so a read that waits fails the test instead of
        // hanging.
        let (answered, answer) = mpsc::channel();
        let host_read = thread::spawn(move || {
            let started = Instant::now();
            host.settings().unwrap();
            answered.send(started.elapsed()).unwrap();
        });
        let took = answer.recv_timeout(LIVE);
        end.send(()).unwrap();
        long_read.join().unwrap().unwrap();
        host_read.join().unwrap();
        let took = took.expect("the host's read waited for the shared reader");
        assert!(took < LIVE, "the host's read took {took:?}");
    }

    /// A write inside `read` fails with `SQLITE_READONLY` on every reader,
    /// and the in-memory store's `query_only` ends with the closure.
    #[test]
    fn a_write_through_read_fails_on_both_stores() {
        let directory = tempfile::tempdir().unwrap();
        let (file, file_host, _) = file_store(&directory);
        let memory = Store::in_memory().unwrap();
        let memory_host = memory.with_own_reader().unwrap();
        let stores = [
            ("the file store", &*file),
            ("the file store's host reader", &file_host),
            ("the in-memory store", &memory),
            ("the in-memory store's host handle", &memory_host),
        ];
        for (name, store) in stores {
            let error = store
                .read(|connection| {
                    connection.execute_batch("CREATE TABLE stenoProbe(x)")?;
                    Ok(())
                })
                .unwrap_err();
            assert!(
                matches!(
                    &error,
                    StoreError::Sqlite(rusqlite::Error::SqliteFailure(error, _))
                        if error.code == ErrorCode::ReadOnly
                ),
                "{name}: {error}"
            );
            store
                .write(|transaction| {
                    transaction
                        .execute_batch("CREATE TABLE stenoProbe(x); DROP TABLE stenoProbe;")?;
                    Ok(())
                })
                .unwrap_or_else(|error| panic!("{name}: {error}"));
        }
    }

    /// A `Store` call from inside a store closure asserts in a debug build.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "a Store call inside a Store closure")]
    fn a_store_call_inside_a_store_closure_asserts() {
        let directory = tempfile::tempdir().unwrap();
        let (store, host, _) = file_store(&directory);
        let _ = store.write(|_| host.settings());
    }

    /// A second host reader is refused while the first lives, and opens
    /// again once it is dropped.
    #[test]
    fn one_host_reader_at_a_time() {
        let directory = tempfile::tempdir().unwrap();
        let (store, host, _) = file_store(&directory);
        assert!(matches!(
            store.with_own_reader(),
            Err(StoreError::OwnReaderOpen)
        ));
        drop(host);
        store.with_own_reader().unwrap().settings().unwrap();
    }

    /// The in-memory store's checkpoint takes its one lock once and
    /// returns at once.
    #[test]
    fn the_in_memory_checkpoint_takes_its_one_lock_once() {
        let store = Store::in_memory().unwrap();
        let host = store.with_own_reader().unwrap();
        let (done, checkpointed) = mpsc::channel();
        // Both handles move to that thread, so a checkpoint that waits on
        // its own lock fails the test instead of hanging the drop.
        thread::spawn(move || {
            for handle in [&store, &host] {
                done.send(handle.checkpoint_durably()).unwrap();
            }
        });
        for _ in 0..2 {
            checkpointed
                .recv_timeout(Duration::from_secs(1))
                .expect("the checkpoint returns")
                .unwrap();
        }
    }

    /// The write that restarts the WAL after a durable checkpoint commits
    /// under `synchronous = FULL`, in the checkpoint's hold of the lock,
    /// and is its only commit.
    #[test]
    fn the_wal_restart_write_commits_under_full() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("steno.sqlite")).unwrap();
        let levels: Arc<Mutex<Vec<i64>>> = Arc::default();
        let seen = levels.clone();
        store.probe_commits(move |connection| {
            let level = connection
                .query_row("PRAGMA synchronous", [], |row| row.get(0))
                .unwrap();
            seen.lock().unwrap().push(level);
        });

        store.checkpoint_durably().unwrap();

        assert_eq!(*levels.lock().unwrap(), [2]);
    }

    /// `unix-none` is SQLite's VFS without shared memory, as on a network
    /// volume: the file stays on its rollback journal and the store
    /// refuses it.
    #[cfg(unix)]
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

    /// A commit on another connection to the file moves the host handle's
    /// change mark through its reader's `data_version`.
    #[test]
    fn a_commit_from_another_connection_moves_the_change_mark() {
        let directory = tempfile::tempdir().unwrap();
        let (_store, host, path) = file_store(&directory);
        let other = Store::open(&path).unwrap();
        let before = host.change_mark().unwrap();
        assert_eq!(host.change_mark().unwrap(), before, "nothing committed");
        let settings = other.settings().unwrap();
        other.save_settings(&settings).unwrap();
        assert_ne!(host.change_mark().unwrap(), before);
    }

    /// This process's commit moves the mark on every handle, in memory
    /// through the commit hook alone. A read moves nothing.
    #[test]
    fn an_in_process_commit_moves_the_change_mark_and_a_read_does_not() {
        let directory = tempfile::tempdir().unwrap();
        let (file, file_host, _) = file_store(&directory);
        let memory = Arc::new(Store::in_memory().unwrap());
        let memory_host = memory.with_own_reader().unwrap();
        for (name, store, host) in [
            ("the file store", &file, &file_host),
            ("the in-memory store", &memory, &memory_host),
        ] {
            let before = host.change_mark().unwrap();
            let settings = store.settings().unwrap();
            host.settings().unwrap();
            assert_eq!(host.change_mark().unwrap(), before, "{name}: a read");
            store.save_settings(&settings).unwrap();
            let after = host.change_mark().unwrap();
            assert_ne!(after, before, "{name}: a commit");
        }
    }
}
