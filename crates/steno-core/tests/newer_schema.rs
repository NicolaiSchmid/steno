//! A database a newer build migrated opens as GRDB opens it: the
//! migration this build does not know stays recorded, a warning in the log
//! names it, and the store reads and writes. A test binary of its own, so
//! no other test reaches the warning's callsite on another thread while
//! this one listens: `tracing` caches whether a callsite is wanted when it
//! is first hit. Swift: `AdmissionLedgerStoreTests.aMigrationThisBuildDoesNotKnowIsIgnored`.

mod common;

use std::sync::{Arc, Mutex};

use steno_core::Store;

/// Runs `body` with a log subscriber and returns what it returned and what
/// was logged.
fn logged<T>(body: impl FnOnce() -> T) -> (T, String) {
    #[derive(Clone, Default)]
    struct Lines(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Lines {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let lines = Lines::default();
    let writer = lines.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let value = tracing::subscriber::with_default(subscriber, body);
    let text = String::from_utf8(lines.0.lock().unwrap().clone()).unwrap();
    (value, text)
}

/// Here a v6 that adds a table: nothing is applied twice, and opening the
/// database without migrating passes too.
#[test]
fn a_migration_this_build_does_not_know_is_ignored_with_a_warning() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("steno.sqlite");
    drop(Store::open(&path).unwrap());
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE later (id INTEGER PRIMARY KEY);
             INSERT INTO grdb_migrations (identifier) VALUES ('v6');",
        )
        .unwrap();

    let (store, log) = logged(|| Store::open(&path).unwrap());
    assert!(
        log.contains("WARN") && log.contains("migration v6"),
        "the warning names v6: {log}"
    );
    assert_eq!(
        store.applied_migrations().unwrap(),
        ["v1", "v2", "v3", "v4", "v5", "v6"]
    );
    let meeting = common::meeting();
    store.save_meeting(&meeting).unwrap();
    assert_eq!(store.meeting(meeting.id).unwrap(), Some(meeting));
    drop(store);

    let (checked, log) = logged(|| Store::open_without_migrating(&path));
    checked.unwrap();
    assert!(log.contains("migration v6"), "{log}");
}
