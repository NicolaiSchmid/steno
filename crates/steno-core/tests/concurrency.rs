//! Two connections on one file, as when the Swift app and this store, or
//! two copies of this store, hold the database at the same time: a write
//! waits on the busy timeout instead of failing, a fresh database is
//! migrated once, an export reads past a held write lock, and an export
//! sees a concurrent commit whole or not at all.

mod common;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::TransactionBehavior;
use steno_core::Store;

use common::populate;

/// Writes each worker makes: enough that a deferred transaction fails,
/// few enough that one worker's whole run fits inside the busy timeout.
/// SQLite's busy handler sleeps and retries, so waiters do not take
/// turns: the worker that takes the write lock first usually makes all
/// of its writes back to back while the other waits once, for that
/// whole run.
const WRITES_PER_WORKER: usize = 40;

/// Past this a worker stops and the test fails on its count, so a stuck
/// lock shows up as a failure rather than a hang.
const GUARD: Duration = Duration::from_secs(60);

/// Each store on its own connection, each writer reading the row and
/// writing it back in one transaction (the shape that made a deferred
/// transaction fail with `database is locked` once the other side had
/// written in between), a fixed number of times each. A barrier starts
/// both workers together so they almost always collide.
///
/// The run fits inside the timeout because the store commits with
/// `synchronous = NORMAL`, as the Swift app does: in WAL mode a commit
/// does not wait for an fsync. The stores open on this thread, so a failed
/// open fails the test before either worker waits at the barrier.
#[test]
fn two_stores_write_the_same_file_without_errors() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("steno.sqlite");
    let meeting = populate(&Store::open(&path).unwrap());

    let start = Arc::new(Barrier::new(2));
    let stores: Vec<_> = (0..2).map(|_| Store::open(&path).unwrap()).collect();
    let workers: Vec<_> = stores
        .into_iter()
        .enumerate()
        .map(|(worker, store)| {
            let start = start.clone();
            thread::spawn(move || {
                start.wait();
                let guard = Instant::now() + GUARD;
                let mut writes = 0;
                let mut errors = Vec::new();
                for _ in 0..WRITES_PER_WORKER {
                    if Instant::now() > guard {
                        break;
                    }
                    let now = chrono::Utc::now();
                    let outcome = store.update_meeting(meeting.id, now, |meeting| {
                        meeting.scratchpad = format!("worker {worker} at {now}");
                        Ok(())
                    });
                    match outcome {
                        Ok(_) => writes += 1,
                        Err(error) => errors.push(error.to_string()),
                    }
                }
                (writes, errors)
            })
        })
        .collect();
    for (worker, handle) in workers.into_iter().enumerate() {
        let (writes, errors) = handle.join().unwrap();
        assert!(
            errors.is_empty(),
            "worker {worker}: {} failed writes: {errors:?}",
            errors.len()
        );
        assert_eq!(
            writes, WRITES_PER_WORKER,
            "worker {worker} made {writes} of {WRITES_PER_WORKER} writes within {GUARD:?}"
        );
    }
    let store = Store::open(&path).unwrap();
    let integrity: String = store
        .read(
            |connection| Ok(connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?),
        )
        .unwrap();
    assert_eq!(integrity, "ok");
    assert!(
        store
            .meeting(meeting.id)
            .unwrap()
            .unwrap()
            .scratchpad
            .starts_with("worker ")
    );
}

/// How long the other store holds the write lock in the waiting test: well
/// inside the five-second busy timeout, far longer than a commit.
const HOLD: Duration = Duration::from_secs(1);

/// A write waits out the other store's transaction instead of failing: one
/// store holds the write lock for [`HOLD`] while the other begins its own,
/// which takes the write lock up front and so waits on the busy timeout.
/// Fails when the timeout is shorter than the hold.
#[test]
fn a_write_waits_for_the_other_stores_transaction() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("steno.sqlite");
    let holder = Store::open(&path).unwrap();
    let waiter = Store::open(&path).unwrap();
    let held = Arc::new(Barrier::new(2));
    let waiting = {
        let held = held.clone();
        thread::spawn(move || {
            held.wait();
            waiter.write(|_| Ok(()))
        })
    };
    holder
        .write(|_| {
            held.wait();
            thread::sleep(HOLD);
            Ok(())
        })
        .unwrap();
    waiting.join().unwrap().unwrap();
}

/// Both opens succeed and each migration is recorded once, whichever
/// process got to the file first.
#[test]
fn two_stores_open_a_fresh_file_at_once() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("steno.sqlite");
    let openers: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            thread::spawn(move || Store::open(&path).map(|store| store.applied_migrations()))
        })
        .collect();
    for opener in openers {
        let applied = opener.join().unwrap().unwrap().unwrap();
        assert_eq!(applied, ["v1", "v2", "v3", "v4"]);
    }
    let store = Store::open(&path).unwrap();
    let identifiers: Vec<String> = store
        .read(|connection| {
            let mut statement =
                connection.prepare("SELECT identifier FROM grdb_migrations ORDER BY identifier")?;
            let rows = statement.query_map([], |row| row.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .unwrap();
    assert_eq!(identifiers, ["v1", "v2", "v3", "v4"]);
}

/// `Store::export` takes no write lock: it returns while the other store
/// holds one. An immediate transaction (or `Store::write`) would queue on
/// the busy timeout instead and fail with `database is locked` once it ran
/// out. A plain read passes this too; the snapshot is checked by
/// `an_export_never_sees_half_of_a_concurrent_commit`.
#[test]
fn an_export_reads_while_the_other_store_holds_the_write_lock() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("steno.sqlite");
    let writer = Store::open(&path).unwrap();
    let meeting = populate(&writer);
    let reader = Store::open(&path).unwrap();

    writer
        .write(|_| {
            assert_eq!(reader.export(meeting.id).unwrap().meeting.id, meeting.id);
            Ok(())
        })
        .unwrap();
}

/// Exports the race test checks: enough for a torn export to show up many
/// times over without the transaction (most exports tear then), few enough
/// to finish in well under a second.
const RACE_EXPORTS: u64 = 300;

/// Commits that must land while those exports run, so they overlap the
/// writer rather than finishing before it starts.
const RACE_COMMITS: u64 = 300;

/// A second connection commits the meeting's title, its decision and its
/// tasks together, in a loop; every export must see all three from one
/// commit. Without the export's transaction each query reads the latest
/// commit, so the title and the decision come from different ones.
#[test]
fn an_export_never_sees_half_of_a_concurrent_commit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("steno.sqlite");
    let store = Store::open(&path).unwrap();
    let meeting = populate(&store);
    let id = steno_core::json::uuid_string(meeting.id);

    let mut connection = rusqlite::Connection::open(&path).unwrap();
    connection.busy_timeout(Duration::from_secs(5)).unwrap();
    connection
        .pragma_update(None, "synchronous", "OFF")
        .unwrap();
    let commit = move |connection: &mut rusqlite::Connection, text: &str| {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        for sql in [
            "UPDATE meeting SET title = ?1 WHERE id = ?2",
            "UPDATE decision SET text = ?1 WHERE meetingID = ?2",
            "UPDATE meetingTask SET text = ?1 WHERE meetingID = ?2",
        ] {
            transaction.execute(sql, (text, &id)).unwrap();
        }
        transaction.commit().unwrap();
    };
    commit(&mut connection, "0");
    let export = store.export(meeting.id).unwrap();
    assert_eq!((export.decisions.len(), export.tasks.len()), (1, 2));

    let stop = Arc::new(AtomicBool::new(false));
    let commits = Arc::new(AtomicU64::new(0));
    let writer = {
        let stop = stop.clone();
        let commits = commits.clone();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let n = commits.fetch_add(1, Ordering::Relaxed) + 1;
                commit(&mut connection, &n.to_string());
            }
        })
    };

    let guard = Instant::now() + GUARD;
    let first = commits.load(Ordering::Relaxed);
    let mut exports = 0;
    let mut torn = Vec::new();
    while (exports < RACE_EXPORTS || commits.load(Ordering::Relaxed) - first < RACE_COMMITS)
        && Instant::now() < guard
    {
        let export = store.export(meeting.id).unwrap();
        exports += 1;
        let title = &export.meeting.title;
        let texts = export.decisions.iter().map(|decision| &decision.text);
        let texts = texts.chain(export.tasks.iter().map(|task| &task.text));
        if texts.clone().any(|text| text != title) {
            torn.push(format!(
                "title {title}, texts {:?}",
                texts.collect::<Vec<_>>()
            ));
        }
    }
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();
    let during = commits.load(Ordering::Relaxed) - first;

    assert!(
        torn.is_empty(),
        "{} of {exports} exports torn, first: {}",
        torn.len(),
        torn[0]
    );
    assert!(
        exports >= RACE_EXPORTS && during >= RACE_COMMITS,
        "{exports} exports and {during} commits within {GUARD:?}"
    );
}
