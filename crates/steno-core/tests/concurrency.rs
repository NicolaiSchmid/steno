//! Two connections on one file, as when the Swift app and this store, or
//! two copies of this store, hold the database at the same time: writes
//! queue on the busy timeout instead of failing, and a fresh database is
//! migrated once.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use steno_core::Store;

use common::populate;

/// Each store on its own connection, each writer reading the row and
/// writing it back in one transaction (the shape that made a deferred
/// transaction fail with `database is locked` once the other side had
/// written in between), for about a second.
#[test]
fn two_stores_write_the_same_file_without_errors() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("steno.sqlite");
    let meeting = populate(&Store::open(&path).unwrap());
    let writes = Arc::new(AtomicUsize::new(0));

    let workers: Vec<_> = (0..2)
        .map(|worker| {
            let path = path.clone();
            let writes = Arc::clone(&writes);
            thread::spawn(move || {
                let store = Store::open(&path).unwrap();
                let deadline = Instant::now() + Duration::from_millis(800);
                let mut errors = Vec::new();
                while Instant::now() < deadline {
                    let now = chrono::Utc::now();
                    let outcome = store.update_meeting(meeting.id, now, |meeting| {
                        meeting.scratchpad = format!("worker {worker} at {now}");
                        Ok(())
                    });
                    match outcome {
                        Ok(_) => {
                            writes.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(error) => errors.push(error.to_string()),
                    }
                }
                errors
            })
        })
        .collect();
    let errors: Vec<String> = workers
        .into_iter()
        .flat_map(|worker| worker.join().unwrap())
        .collect();

    assert!(
        errors.is_empty(),
        "{} failed writes: {errors:?}",
        errors.len()
    );
    assert!(writes.load(Ordering::Relaxed) > 2, "both writers wrote");
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
