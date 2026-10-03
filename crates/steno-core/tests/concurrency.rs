//! Two connections on one file, as when the Swift app and this store, or
//! two copies of this store, hold the database at the same time: writes
//! queue on the busy timeout instead of failing, a fresh database is
//! migrated once, and an export reads past a held write lock.

mod common;

use std::thread;
use std::time::{Duration, Instant};

use steno_core::Store;

use common::populate;

/// Writes each worker makes: enough for the two to collide on the lock
/// many times over, few enough that the loser's busy-handler sleeps (up to
/// 100 ms a turn) keep the test under a few seconds on a CI runner.
const WRITES_PER_WORKER: usize = 40;

/// Past this a worker stops and the test fails on its count, so a stuck
/// lock shows up as a failure rather than a hang.
const GUARD: Duration = Duration::from_secs(60);

/// Each store on its own connection, each writer reading the row and
/// writing it back in one transaction (the shape that made a deferred
/// transaction fail with `database is locked` once the other side had
/// written in between), a fixed number of times each.
#[test]
fn two_stores_write_the_same_file_without_errors() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("steno.sqlite");
    let meeting = populate(&Store::open(&path).unwrap());

    let workers: Vec<_> = (0..2)
        .map(|worker| {
            let path = path.clone();
            thread::spawn(move || {
                let store = Store::open(&path).unwrap();
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

/// `Store::export` reads under a deferred transaction, which in WAL mode
/// is a snapshot that neither waits for nor blocks a writer: it returns
/// while the other store holds the write lock. An immediate transaction
/// would queue on the busy timeout instead and fail after it.
#[test]
fn an_export_reads_while_the_other_store_holds_the_write_lock() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("steno.sqlite");
    let writer = Store::open(&path).unwrap();
    let meeting = populate(&writer);
    let reader = Store::open(&path).unwrap();

    let started = Instant::now();
    writer
        .write(|_| {
            let export = reader.export(meeting.id).unwrap();
            assert_eq!(export.meeting.id, meeting.id);
            assert_eq!(export.meeting.title, meeting.title);
            Ok(())
        })
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "the export waited on the writer's lock"
    );
}
