//! Databases as another build left them, written and read without the
//! store, which would migrate them, and another connection holding one.
//! Rust only.

use std::path::Path;
use std::sync::mpsc;
use std::thread::JoinHandle;

use rusqlite::Connection;

use crate::Store;
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

/// Another connection's write transaction on the database at `path`, held
/// from its own thread until the hold is dropped: a write on any other
/// connection waits out its busy timeout meanwhile. The drop ends the
/// transaction before it returns.
pub struct WriteLockHold {
    release: Option<mpsc::Sender<()>>,
    holder: Option<JoinHandle<()>>,
}

impl WriteLockHold {
    /// Returns once the transaction holds the lock.
    pub fn new(path: &Path) -> Self {
        let other = Store::open(path).unwrap();
        let (release, released) = mpsc::channel::<()>();
        let (held, holding) = mpsc::channel();
        let holder = std::thread::spawn(move || {
            other
                .write(|_| {
                    held.send(()).unwrap();
                    let _ = released.recv();
                    Ok(())
                })
                .unwrap();
        });
        holding.recv().unwrap();
        WriteLockHold {
            release: Some(release),
            holder: Some(holder),
        }
    }
}

impl Drop for WriteLockHold {
    fn drop(&mut self) {
        drop(self.release.take());
        if let Some(holder) = self.holder.take() {
            let joined = holder.join();
            if !std::thread::panicking() {
                joined.expect("the held transaction ends");
            }
        }
    }
}
