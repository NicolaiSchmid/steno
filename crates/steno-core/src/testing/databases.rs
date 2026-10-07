//! Databases as another build left them, written and read without the
//! store, which would migrate them. Rust only.

use std::path::Path;

use rusqlite::Connection;

use crate::store::migrator;

/// Writes at `path` a database every migration but the newest has reached,
/// in WAL mode as the app leaves it: what an older app runs on.
pub fn database_one_version_behind(path: &Path) {
    let mut connection = Connection::open(path).unwrap();
    let _: String = connection
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    let (_, older) = migrator::MIGRATIONS.split_last().unwrap();
    migrator::migrate_with(&mut connection, older).unwrap();
}

/// The migrations the database at `path` records.
pub fn recorded_migrations(path: &Path) -> Vec<String> {
    migrator::applied(&Connection::open(path).unwrap()).unwrap()
}
