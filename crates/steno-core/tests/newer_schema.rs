//! A database a newer build migrated opens: the unknown migration stays
//! recorded, a warning names it, and the store reads and writes. A binary of
//! its own because `tracing` caches a callsite's interest when first hit.

mod common;

use common::logged;
use steno_core::Store;

/// Here a v7 that adds a table; opening without migrating passes too.
#[test]
fn a_migration_this_build_does_not_know_is_ignored_with_a_warning() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("steno.sqlite");
    drop(Store::open(&path).unwrap());
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE later (id INTEGER PRIMARY KEY);
             INSERT INTO grdb_migrations (identifier) VALUES ('v7');",
        )
        .unwrap();

    let (store, log) = logged(|| Store::open(&path).unwrap());
    assert!(
        log.contains("WARN") && log.contains("migration v7"),
        "the warning names v7: {log}"
    );
    assert_eq!(
        store.applied_migrations().unwrap(),
        ["v1", "v2", "v3", "v4", "v5", "v6", "v7"]
    );
    let meeting = common::meeting();
    store.save_meeting(&meeting).unwrap();
    assert_eq!(store.meeting(meeting.id).unwrap(), Some(meeting));
    drop(store);

    let (checked, log) = logged(|| Store::open_without_migrating(&path));
    checked.unwrap();
    assert!(log.contains("migration v7"), "{log}");
}
