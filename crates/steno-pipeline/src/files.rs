//! Durable file writes: replacing a file in one step, copying a recording
//! so it survives a power loss, creating and removing durably, and reading
//! and writing a JSON file this process owns, set aside when it does not
//! parse. On Windows a folder flush alone does not make a rename durable, so
//! renames are written through and the renamed file is flushed too.

#[cfg(any(target_os = "linux", target_os = "macos", test))]
mod mount;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use serde::Serialize;
use serde::de::DeserializeOwned;
use steno_core::busy_file::{self, is_busy};
use uuid::Uuid;

/// The syncs a durable write makes and its waits before a retry: the disk
/// and the clock in the product, a recorder in the tests.
pub(crate) trait Syncs {
    /// Flushes `file`, written at `path`, to the disk.
    fn file(&self, file: &File, path: &Path) -> std::io::Result<()>;
    /// Makes the entries of `directory` durable. Best effort on Linux and
    /// macOS. On Windows a failed open or flush is an error, so the phone
    /// intake answers 500 and the phone keeps its copy; a drive that refuses
    /// to flush a folder is logged and passed over.
    fn directory(&self, directory: &Path) -> std::io::Result<()>;
    fn wait(&self, delay: Duration) {
        std::thread::sleep(delay);
    }
}

pub(crate) struct Disk;

impl Syncs for Disk {
    fn file(&self, file: &File, _path: &Path) -> std::io::Result<()> {
        file.sync_all()
    }

    fn directory(&self, directory: &Path) -> std::io::Result<()> {
        #[cfg(windows)]
        {
            windows::flush_directory(directory)
        }
        #[cfg(not(windows))]
        {
            if let Ok(handle) = File::open(directory) {
                let _ = handle.sync_all();
            }
            Ok(())
        }
    }
}

/// Whether a power cut may lose a recording just written into `folder`, for
/// the warning in Settings: true on a Windows drive that is neither NTFS nor
/// `ReFS`, and on a network drive or mount anywhere; false elsewhere and
/// where the file system cannot be read.
pub fn may_lose_recent_writes(folder: &Path) -> bool {
    #[cfg(windows)]
    {
        windows::is_on_a_network_drive(folder)
            || windows::file_system_name(folder)
                .is_ok_and(|name| !matches!(name.as_str(), "NTFS" | "ReFS"))
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        mount::is_on_a_network_mount(folder)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = folder;
        false
    }
}

/// Who may read a file [`replace_file`] writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Mode 0600 where the platform has modes: the secrets file.
    OwnerOnly,
    /// The mode any new file gets (0666 less the umask).
    Default,
}

const TEMPORARY_SUFFIX: &str = ".partial";

/// How old a temporary must be before a later write removes it as one a
/// killed writer left behind.
const STALE_AFTER: Duration = Duration::from_secs(60);

/// Replaces `path` with `data`: a temporary of this writer's own beside
/// `path` is written, synced and renamed into place, then the folder is
/// synced, so a reader or a crash sees the old file or the new one, never a
/// torn one. Temporaries a killed writer left behind are removed by a later
/// write once they are a minute old.
pub fn replace_file(path: &Path, data: &[u8], access: Access) -> std::io::Result<()> {
    replace_file_with(&Disk, path, data, access)
}

/// [`replace_file`] through `syncs`.
pub(crate) fn replace_file_with(
    syncs: &dyn Syncs,
    path: &Path,
    data: &[u8],
    access: Access,
) -> std::io::Result<()> {
    write_durably(syncs, path, access, |file| file.write_all(data))
}

/// Copies `source` to `destination` the way [`replace_file`] writes, so once
/// this returns the copy survives a power loss. The phone intake needs that
/// before it marks the receipt complete, since the phone then deletes its
/// copy.
pub fn copy_durably(source: &Path, destination: &Path) -> std::io::Result<()> {
    copy_durably_with(&Disk, source, destination)
}

fn copy_durably_with(syncs: &dyn Syncs, source: &Path, destination: &Path) -> std::io::Result<()> {
    let mut reader = File::open(source)?;
    write_durably(syncs, destination, Access::Default, |file| {
        std::io::copy(&mut reader, file).map(|_| ())
    })
}

/// Removes the file at `path` and syncs its folder; a file that is not there
/// is removed already. On Windows a busy file is tried again.
pub fn remove_durably(path: &Path) -> std::io::Result<()> {
    remove_durably_with(&Disk, path)
}

pub(crate) fn remove_durably_with(syncs: &dyn Syncs, path: &Path) -> std::io::Result<()> {
    match retried(syncs, || std::fs::remove_file(path)) {
        Ok(()) => syncs.directory(folder_of(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// `std::fs::create_dir_all`, with the parent of every folder it creates
/// synced. On Windows a failed folder flush is an error.
pub fn create_dir_all_durably(directory: &Path) -> std::io::Result<()> {
    create_dir_all_durably_with(&Disk, directory)
}

/// Creates the folder `directory`, which must not exist yet, with its
/// missing parents, synced as [`create_dir_all_durably`] does. An existing
/// entry fails with `AlreadyExists` before anything is made, so a caller
/// that cleans up after a later failure removes only what it made; a failed
/// sync removes the new folder again.
pub fn create_new_dir_durably(directory: &Path) -> std::io::Result<()> {
    create_new_dir_durably_with(&Disk, directory)
}

fn create_new_dir_durably_with(syncs: &dyn Syncs, directory: &Path) -> std::io::Result<()> {
    let parent = folder_of(directory);
    create_dir_all_durably_with(syncs, parent)?;
    std::fs::create_dir(directory)?;
    syncs.directory(parent).inspect_err(|_| {
        let _ = std::fs::remove_dir(directory);
    })
}

fn folder_of(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

pub(crate) fn create_dir_all_durably_with(
    syncs: &dyn Syncs,
    directory: &Path,
) -> std::io::Result<()> {
    let missing: Vec<&Path> = directory
        .ancestors()
        .take_while(|ancestor| !ancestor.as_os_str().is_empty() && !ancestor.exists())
        .collect();
    std::fs::create_dir_all(directory)?;
    for created in missing.iter().rev() {
        syncs.directory(folder_of(created))?;
    }
    Ok(())
}

/// The one durable write: `write` fills a temporary beside `path`, which is
/// synced, renamed onto `path` and its folder synced. A folder sync that
/// fails is an error with the new file in place.
fn write_durably(
    syncs: &dyn Syncs,
    path: &Path,
    access: Access,
    write: impl FnOnce(&mut File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let directory = folder_of(path);
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} names no file", path.display()),
        )
    })?;
    let prefix = format!(".{}.", name.to_string_lossy());
    remove_stale_temporaries(directory, &prefix);
    let mut builder = tempfile::Builder::new();
    builder.prefix(&prefix).suffix(TEMPORARY_SUFFIX);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(std::fs::Permissions::from_mode(match access {
            Access::OwnerOnly => 0o600,
            Access::Default => 0o666,
        }));
    }
    #[cfg(not(unix))]
    let _ = access;
    let mut temporary = builder.tempfile_in(directory)?;
    write(temporary.as_file_mut())?;
    syncs.file(temporary.as_file(), temporary.path())?;
    // `rename_over` rather than `persist`: written through on Windows, and
    // std's fallback replaces a target another handle has open.
    let (file, temporary) = temporary.keep().map_err(|error| error.error)?;
    drop(file);
    if let Err(error) = rename_over(syncs, &temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    syncs.directory(directory)
}

/// Which rename [`rename_over`] made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Renamed {
    /// `MoveFileExW` with `MOVEFILE_WRITE_THROUGH`.
    #[cfg(windows)]
    WrittenThrough,
    WithStd,
}

/// Renames `from` onto `to`. On Windows, where a folder flush does not cover
/// a rename on every drive, the rename is written through and the renamed
/// file reopened and flushed, falling back to std's rename where the
/// written-through one fails. Busy files are retried, and two writers of
/// one path in this process take turns (`windows::lock_path`), since one
/// writer's flush would otherwise make the other's reopen fail. A flush that
/// fails after the rename is an error with the new file already in place.
fn rename_over(syncs: &dyn Syncs, from: &Path, to: &Path) -> std::io::Result<Renamed> {
    #[cfg(windows)]
    {
        let _writing = windows::lock_path(to);
        let renamed = if windows::rename_written_through(from, to).is_ok() {
            Renamed::WrittenThrough
        } else {
            rename_with_std(syncs, from, to)?;
            Renamed::WithStd
        };
        let renamed_file = retried(syncs, || OpenOptions::new().write(true).open(to))?;
        syncs.file(&renamed_file, to)?;
        Ok(renamed)
    }
    #[cfg(not(windows))]
    {
        rename_with_std(syncs, from, to)?;
        Ok(Renamed::WithStd)
    }
}

/// `std::fs::rename`, retried on Windows while the file is busy.
fn rename_with_std(syncs: &dyn Syncs, from: &Path, to: &Path) -> std::io::Result<()> {
    retried(syncs, || std::fs::rename(from, to))
}

/// `busy_file::retried`, waiting through `syncs`: on Windows a busy file is
/// tried again after waits of 5 ms doubling to 200 ms, about 0.9 s in all;
/// elsewhere `attempt` runs once.
fn retried<T>(
    syncs: &dyn Syncs,
    attempt: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    busy_file::retried_with(
        busy_file::RETRIES,
        is_busy,
        |delay| syncs.wait(delay),
        attempt,
    )
}

/// Removes this file's temporaries in `directory` older than
/// [`STALE_AFTER`]; best effort.
fn remove_stale_temporaries(directory: &Path, prefix: &str) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with(prefix) && name.ends_with(TEMPORARY_SUFFIX)) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| {
                now.duration_since(modified)
                    .is_ok_and(|age| age > STALE_AFTER)
            });
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Moves `path` aside to `<name>.corrupt-<UTC time>` beside it (`-2`, `-3`
/// and so on when taken) and syncs the folder, best effort. A reader that
/// cannot parse a file it owns calls this before it starts empty, so the
/// next write cannot replace bytes nobody has looked at. Returns the new
/// path.
///
/// The new name is claimed with a hard link, which fails when taken, so a
/// copy another process sets aside in the same second is never replaced.
/// Where no hard link can be made the move is a rename after a check that
/// the name is free. If the old name cannot be removed after the link, the
/// error is returned and both names hold the bytes.
pub fn set_aside(path: &Path) -> std::io::Result<PathBuf> {
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} names no file", path.display()),
        )
    })?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let base = format!("{}.corrupt-{stamp}", name.to_string_lossy());
    let mut attempt = 1_u32;
    let aside = loop {
        let candidate = if attempt == 1 {
            path.with_file_name(&base)
        } else {
            path.with_file_name(format!("{base}-{attempt}"))
        };
        match std::fs::hard_link(path, &candidate) {
            Ok(()) => match busy_file::retried(|| std::fs::remove_file(path)) {
                // Another `set_aside` removed the old name; the bytes are at
                // the link.
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
                _ => break candidate,
            },
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => attempt += 1,
            // No hard link here (FAT, some network shares), so rename; a
            // read-only folder fails the rename the same way.
            Err(_) => match candidate.symlink_metadata() {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    busy_file::rename(path, &candidate)?;
                    break candidate;
                }
                Err(error) => return Err(error),
                Ok(_) => attempt += 1,
            },
        }
    };
    if let Some(directory) = aside.parent() {
        let _ = Disk.directory(directory);
    }
    Ok(aside)
}

/// The JSON in `path`, a file this process alone writes, and whether it may
/// be written. A missing file reads as the default; one that does not parse
/// is set aside and logged first. A file that cannot be read for another
/// reason, or cannot be set aside, is logged and must never be written
/// (`false`).
pub fn read_json<T: DeserializeOwned + Default>(path: &Path) -> (T, bool) {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (T::default(), true);
        }
        Err(error) => {
            tracing::warn!(
                "{} could not be read ({error}); it stays and is not written",
                path.display()
            );
            return (T::default(), false);
        }
    };
    let error = match serde_json::from_slice(&bytes) {
        Ok(value) => return (value, true),
        Err(error) => error,
    };
    match set_aside(path) {
        Ok(aside) => {
            tracing::warn!(
                "{} did not parse ({error}); moved it to {} and started empty",
                path.display(),
                aside.display()
            );
            (T::default(), true)
        }
        Err(move_error) => {
            tracing::warn!(
                "{} did not parse ({error}) and could not be moved aside \
                 ({move_error}); it stays and is not written",
                path.display()
            );
            (T::default(), false)
        }
    }
}

/// Replaces `path` with `value` as pretty-printed JSON, creating its folder
/// first.
pub fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> std::io::Result<()> {
    let data = serde_json::to_vec_pretty(value)?;
    if let Some(parent) = path.parent() {
        create_dir_all_durably(parent)?;
    }
    replace_file(path, &data, Access::Default)
}

/// A value per meeting in a JSON file this process owns, replaced with
/// [`write_json`] on every change; a file that may not be written keeps the
/// values of this run in memory only.
#[derive(Debug)]
pub(crate) struct MeetingValues<V> {
    path: PathBuf,
    values: Mutex<BTreeMap<Uuid, V>>,
    /// False for [`in_memory`](Self::in_memory) and when the file on disk
    /// could not be read or set aside.
    writable: bool,
}

impl<V: Serialize + DeserializeOwned + Clone> MeetingValues<V> {
    /// The values in `path`: a missing or corrupt file holds none.
    pub(crate) fn new(path: PathBuf) -> Self {
        let (values, writable) = read_json(&path);
        Self::from_read(path, values, writable)
    }

    pub(crate) fn from_read(path: PathBuf, values: BTreeMap<Uuid, V>, writable: bool) -> Self {
        MeetingValues {
            path,
            values: Mutex::new(values),
            writable,
        }
    }

    pub(crate) fn in_memory() -> Self {
        Self::from_read(PathBuf::new(), BTreeMap::new(), false)
    }

    pub(crate) fn get(&self, meeting_id: Uuid) -> Option<V> {
        self.lock().get(&meeting_id).cloned()
    }

    /// Runs `change` over the values and replaces the file when it says it
    /// changed them. A failed write is logged and returned; the values stay
    /// in memory.
    pub(crate) fn change(
        &self,
        change: impl FnOnce(&mut BTreeMap<Uuid, V>) -> bool,
    ) -> std::io::Result<()> {
        let mut values = self.lock();
        if !change(&mut values) || !self.writable {
            return Ok(());
        }
        write_json(&self.path, &*values).inspect_err(|error| {
            tracing::warn!("{} could not be written: {error}", self.path.display());
        })
    }

    /// Keeps only the values of the meetings `keep` accepts, replacing the
    /// file when that drops any.
    pub(crate) fn retain(&self, mut keep: impl FnMut(Uuid) -> bool) {
        let _ = self.change(|values| {
            let before = values.len();
            values.retain(|meeting_id, _| keep(*meeting_id));
            values.len() != before
        });
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<Uuid, V>> {
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Makes `options` create the file with mode 0600 where the platform has
/// modes.
pub fn restrict_new_file(options: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::{Arc, Barrier};

    use super::*;

    /// A second file set aside gets a name of its own.
    #[test]
    fn set_aside_never_replaces_an_earlier_copy() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        std::fs::write(&path, b"first").unwrap();
        let first = set_aside(&path).unwrap();
        std::fs::write(&path, b"second").unwrap();
        let second = set_aside(&path).unwrap();
        std::fs::write(&path, b"third").unwrap();
        let third = set_aside(&path).unwrap();
        assert!(!path.exists());
        assert_eq!(std::fs::read(&first).unwrap(), b"first");
        assert_eq!(std::fs::read(&second).unwrap(), b"second");
        assert_eq!(std::fs::read(&third).unwrap(), b"third");
        assert!(
            first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("preferences.json.corrupt-")
        );
    }

    /// A file already gone is not set aside and no copy appears.
    #[test]
    fn a_missing_file_is_not_set_aside() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let error = set_aside(&path).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    fn temporaries(directory: &Path) -> Vec<String> {
        std::fs::read_dir(directory)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(TEMPORARY_SUFFIX))
            .collect()
    }

    /// Concurrent writers into one folder: every write lands whole and no
    /// temporary is left.
    #[test]
    fn parallel_writers_into_one_folder_never_lose_or_tear_a_file() {
        const WRITERS: usize = 8;
        const WRITES: usize = 25;
        let dir = tempfile::tempdir().unwrap();
        let files = [dir.path().join("a.json"), dir.path().join("b.json")];
        let payload = |file: usize, writer: usize, write: usize| {
            format!(
                "{{\"file\":{file},\"writer\":{writer},\"write\":{write},\"pad\":\"{}\"}}",
                "x".repeat(4096 * (writer + 1))
            )
        };
        let start = Arc::new(Barrier::new(WRITERS * files.len()));
        let mut threads = Vec::new();
        for (file, path) in files.iter().enumerate() {
            for writer in 0..WRITERS {
                let (path, start) = (path.clone(), start.clone());
                threads.push(std::thread::spawn(move || {
                    start.wait();
                    for write in 0..WRITES {
                        replace_file(
                            &path,
                            payload(file, writer, write).as_bytes(),
                            Access::Default,
                        )
                        .unwrap();
                    }
                }));
            }
        }
        for thread in threads {
            thread.join().expect("every write succeeded");
        }
        for (file, path) in files.iter().enumerate() {
            let text = std::fs::read_to_string(path).unwrap();
            let written: BTreeSet<String> = (0..WRITERS)
                .flat_map(|writer| (0..WRITES).map(move |write| payload(file, writer, write)))
                .collect();
            assert!(
                written.contains(&text),
                "{} is one whole payload",
                path.display()
            );
        }
        assert_eq!(temporaries(dir.path()), Vec::<String>::new());
    }

    #[cfg(unix)]
    #[test]
    fn owner_only_files_are_0600_and_default_ones_follow_the_umask() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let secret = dir.path().join("secrets.json");
        replace_file(&secret, b"{}", Access::OwnerOnly).unwrap();
        assert_eq!(mode(&secret), 0o600);
        let export = dir.path().join("meeting.json");
        replace_file(&export, b"{}", Access::Default).unwrap();
        assert_eq!(mode(&export) & 0o044, 0o044, "readable as a new file is");
    }

    /// A stale temporary goes with the next write; a fresh one stays.
    #[test]
    fn a_stale_temporary_is_removed_by_the_next_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meeting.json");
        let stale = dir.path().join(".meeting.json.abc123.partial");
        let fresh = dir.path().join(".meeting.json.def456.partial");
        let other = dir.path().join(".notes.json.abc123.partial");
        for leftover in [&stale, &fresh, &other] {
            std::fs::write(leftover, b"half").unwrap();
        }
        for old in [&stale, &other] {
            std::fs::File::options()
                .write(true)
                .open(old)
                .unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(120))
                .unwrap();
        }
        replace_file(&path, b"{}", Access::Default).unwrap();
        assert!(!stale.exists());
        assert!(fresh.exists(), "a live writer's temporary stays");
        assert!(
            other.exists(),
            "another file's temporary is not this write's"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"{}");
    }

    /// What a test's syncs saw, in order, with whether the destination
    /// existed yet; waits do not wait but run `on_first_wait` once.
    struct Recorded {
        destination: std::path::PathBuf,
        events: std::sync::Mutex<Vec<(String, bool)>>,
        waits: std::sync::Mutex<Vec<Duration>>,
        on_first_wait: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
        fail_file_sync: bool,
        fail_directory_sync: bool,
    }

    impl Recorded {
        fn new(destination: &Path) -> Self {
            Recorded {
                destination: destination.to_path_buf(),
                events: std::sync::Mutex::new(Vec::new()),
                waits: std::sync::Mutex::new(Vec::new()),
                on_first_wait: std::sync::Mutex::new(None),
                fail_file_sync: false,
                fail_directory_sync: false,
            }
        }

        fn events(&self) -> Vec<(String, bool)> {
            self.events.lock().unwrap().clone()
        }

        #[cfg(windows)]
        fn waits(&self) -> Vec<Duration> {
            self.waits.lock().unwrap().clone()
        }
    }

    impl Syncs for Recorded {
        fn file(&self, file: &File, path: &Path) -> std::io::Result<()> {
            assert_eq!(
                file.metadata()?.len(),
                std::fs::metadata(path)?.len(),
                "the handle is the temporary's"
            );
            self.events
                .lock()
                .unwrap()
                .push(("file".to_owned(), self.destination.exists()));
            if self.fail_file_sync {
                return Err(std::io::Error::other("the disk is full"));
            }
            Ok(())
        }

        fn directory(&self, directory: &Path) -> std::io::Result<()> {
            self.events.lock().unwrap().push((
                format!("directory {}", directory.display()),
                self.destination.exists(),
            ));
            if self.fail_directory_sync {
                return Err(std::io::Error::other("the drive cannot flush a folder"));
            }
            Ok(())
        }

        fn wait(&self, delay: Duration) {
            self.waits.lock().unwrap().push(delay);
            if let Some(first) = self.on_first_wait.lock().unwrap().take() {
                first();
            }
        }
    }

    /// The copy is synced before the rename and the folder after it, and
    /// holds the source's content with no temporary left.
    #[test]
    fn a_durable_copy_syncs_the_file_then_renames_then_syncs_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("upload.m4a");
        std::fs::write(&source, vec![7u8; 100_000]).unwrap();
        let destination = dir.path().join("meeting").join("recording.m4a");
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let syncs = Recorded::new(&destination);
        copy_durably_with(&syncs, &source, &destination).unwrap();
        let mut expected = vec![("file".to_owned(), false)];
        if cfg!(windows) {
            expected.push(("file".to_owned(), true));
        }
        expected.push((
            format!("directory {}", destination.parent().unwrap().display()),
            true,
        ));
        assert_eq!(syncs.events(), expected);
        assert_eq!(std::fs::read(&destination).unwrap(), vec![7u8; 100_000]);
        assert_eq!(
            temporaries(destination.parent().unwrap()),
            Vec::<String>::new()
        );
    }

    /// The product's copy replaces a file at the destination and leaves no
    /// temporary.
    #[test]
    fn a_durable_copy_replaces_the_destination_and_leaves_no_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("upload.m4a");
        std::fs::write(&source, vec![7u8; 100_000]).unwrap();
        let destination = dir.path().join("meeting").join("recording.m4a");
        create_dir_all_durably(destination.parent().unwrap()).unwrap();
        std::fs::write(&destination, b"an earlier copy").unwrap();
        copy_durably(&source, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), vec![7u8; 100_000]);
        assert_eq!(std::fs::read(&source).unwrap(), vec![7u8; 100_000]);
        assert_eq!(
            temporaries(destination.parent().unwrap()),
            Vec::<String>::new()
        );
    }

    /// A file another handle holds open is still replaced.
    #[test]
    fn a_file_held_open_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preferences.json");
        std::fs::write(&path, b"old").unwrap();
        let reader = File::open(&path).unwrap();
        replace_file(&path, b"new", Access::Default).unwrap();
        drop(reader);
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(temporaries(dir.path()), Vec::<String>::new());
    }

    /// A failure before the rename leaves no destination and no temporary.
    #[test]
    fn a_copy_that_fails_before_the_rename_leaves_no_destination() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("upload.m4a");
        std::fs::write(&source, b"aac").unwrap();
        let destination = dir.path().join("recording.m4a");
        let mut syncs = Recorded::new(&destination);
        syncs.fail_file_sync = true;
        assert!(copy_durably_with(&syncs, &source, &destination).is_err());
        assert!(!destination.exists());
        assert_eq!(temporaries(dir.path()), Vec::<String>::new());
        assert!(copy_durably(&dir.path().join("missing.m4a"), &destination).is_err());
        assert!(!destination.exists());
    }

    /// Every folder created is synced into its parent; existing ones are
    /// left alone.
    #[test]
    fn new_folders_are_synced_into_their_parents() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("audio");
        let meeting = audio.join("0B6F4B1E");
        let syncs = Recorded::new(&meeting);
        create_dir_all_durably_with(&syncs, &meeting).unwrap();
        assert!(meeting.is_dir());
        assert_eq!(
            syncs.events(),
            [
                (format!("directory {}", dir.path().display()), true),
                (format!("directory {}", audio.display()), true),
            ]
        );
        let again = Recorded::new(&meeting);
        create_dir_all_durably_with(&again, &meeting).unwrap();
        assert_eq!(again.events(), Vec::<(String, bool)>::new());
    }

    /// Missing parents are made and synced first; an existing folder fails
    /// with `AlreadyExists` and syncs nothing.
    #[test]
    fn a_new_folder_is_synced_and_an_existing_one_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("audio");
        let meeting = audio.join("0B6F4B1E");
        let syncs = Recorded::new(&meeting);
        create_new_dir_durably_with(&syncs, &meeting).unwrap();
        assert!(meeting.is_dir());
        assert_eq!(
            syncs.events(),
            [
                (format!("directory {}", dir.path().display()), false),
                (format!("directory {}", audio.display()), true),
            ]
        );
        std::fs::write(meeting.join("recording.m4a"), b"an earlier recording").unwrap();
        let again = Recorded::new(&meeting);
        let error = create_new_dir_durably_with(&again, &meeting).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(again.events(), Vec::<(String, bool)>::new());
        assert_eq!(
            std::fs::read(meeting.join("recording.m4a")).unwrap(),
            b"an earlier recording"
        );
    }

    /// A failed sync removes only the new folder.
    #[test]
    fn a_new_folder_whose_sync_fails_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("audio");
        let earlier = audio.join("5C2D9A70").join("recording.m4a");
        std::fs::create_dir_all(earlier.parent().unwrap()).unwrap();
        std::fs::write(&earlier, b"an earlier recording").unwrap();
        let meeting = audio.join("0B6F4B1E");
        let mut syncs = Recorded::new(&meeting);
        syncs.fail_directory_sync = true;
        assert!(create_new_dir_durably_with(&syncs, &meeting).is_err());
        assert!(!meeting.exists());
        assert_eq!(std::fs::read(&earlier).unwrap(), b"an earlier recording");
    }

    /// A removal is synced into its folder; a missing file syncs nothing.
    #[test]
    fn a_removal_syncs_the_folder_after_the_file_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let mark = dir.path().join("warm-up-in-progress");
        std::fs::write(&mark, b"1\n").unwrap();
        let syncs = Recorded::new(&mark);
        remove_durably_with(&syncs, &mark).unwrap();
        assert!(!mark.exists());
        assert_eq!(
            syncs.events(),
            [(format!("directory {}", dir.path().display()), false)]
        );
        let again = Recorded::new(&mark);
        remove_durably_with(&again, &mark).unwrap();
        assert_eq!(again.events(), Vec::<(String, bool)>::new());
    }

    /// A failed folder sync fails the folder creation and the copy.
    #[test]
    fn a_failed_folder_sync_fails_the_folders_and_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let meeting = dir.path().join("audio").join("0B6F4B1E");
        let mut syncs = Recorded::new(&meeting);
        syncs.fail_directory_sync = true;
        assert!(create_dir_all_durably_with(&syncs, &meeting).is_err());
        let source = dir.path().join("upload.m4a");
        std::fs::write(&source, b"aac").unwrap();
        let destination = meeting.join("recording.m4a");
        let mut syncs = Recorded::new(&destination);
        syncs.fail_directory_sync = true;
        assert!(copy_durably_with(&syncs, &source, &destination).is_err());
        assert_eq!(temporaries(&meeting), Vec::<String>::new());
    }

    /// CI's test folders are on local drives that do not warn.
    #[test]
    fn a_test_folder_is_not_on_a_drive_that_may_lose_recent_writes() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!may_lose_recent_writes(dir.path()));
        assert!(!may_lose_recent_writes(&dir.path().join("audio")));
    }

    /// On Windows a plain rename is written through and the file flushed.
    #[cfg(windows)]
    #[test]
    fn a_rename_is_written_through_and_flushed() {
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("preferences.json");
        std::fs::write(&to, b"old").unwrap();
        let from = dir.path().join(".preferences.json.a.partial");
        std::fs::write(&from, b"new").unwrap();
        let syncs = Recorded::new(&to);
        assert_eq!(
            rename_over(&syncs, &from, &to).unwrap(),
            Renamed::WrittenThrough
        );
        assert_eq!(syncs.events(), [("file".to_owned(), true)]);
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
    }

    /// On Windows a target held open falls back to std's rename, then the
    /// flush.
    #[cfg(windows)]
    #[test]
    fn a_held_open_target_is_renamed_with_std_and_flushed() {
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("preferences.json");
        std::fs::write(&to, b"old").unwrap();
        let reader = File::open(&to).unwrap();
        let from = dir.path().join(".preferences.json.a.partial");
        std::fs::write(&from, b"new").unwrap();
        let syncs = Recorded::new(&to);
        assert_eq!(rename_over(&syncs, &from, &to).unwrap(), Renamed::WithStd);
        assert_eq!(syncs.events(), [("file".to_owned(), true)]);
        drop(reader);
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
    }

    /// On Windows a target held without delete sharing refuses both
    /// renames; the replace waits and lands once the handle lets go.
    #[cfg(windows)]
    #[test]
    fn a_target_another_handle_holds_is_replaced_once_it_lets_go() {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("preferences.json");
        std::fs::write(&to, b"old").unwrap();
        let holder = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&to)
            .unwrap();
        let syncs = Recorded::new(&to);
        *syncs.on_first_wait.lock().unwrap() = Some(Box::new(move || drop(holder)));
        write_durably(&syncs, &to, Access::Default, |file| file.write_all(b"new")).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
        assert_eq!(
            syncs.waits().first(),
            Some(&busy_file::FIRST_WAIT),
            "refused at least once, then replaced"
        );
        assert_eq!(temporaries(dir.path()), Vec::<String>::new());
    }

    /// On Windows the renamed file is flushed under its path's lock.
    #[cfg(windows)]
    #[test]
    fn a_rename_and_its_flush_hold_the_path_lock() {
        struct LockSeen(std::sync::Mutex<Vec<bool>>);
        impl Syncs for LockSeen {
            fn file(&self, _file: &File, path: &Path) -> std::io::Result<()> {
                self.0.lock().unwrap().push(windows::holds_path_lock(path));
                Ok(())
            }
            fn directory(&self, _directory: &Path) -> std::io::Result<()> {
                Ok(())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("preferences.json");
        std::fs::write(&to, b"old").unwrap();
        let seen = LockSeen(std::sync::Mutex::new(Vec::new()));
        write_durably(&seen, &to, Access::Default, |file| file.write_all(b"new")).unwrap();
        let seen = seen.0.into_inner().unwrap();
        assert_eq!(
            seen.len(),
            2,
            "the temporary's sync, then the renamed file's"
        );
        assert!(seen[1], "the renamed file is flushed under the lock");
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
    }

    /// Folder creation and copy succeed on FAT32 and exFAT drives and
    /// Settings warns. CI names the drives in `STENO_FAT32_VOLUME` and
    /// `STENO_EXFAT_VOLUME`; without one the test skips.
    #[cfg(windows)]
    fn the_intake_writes_succeed_and_settings_warn(variable: &str, file_system: &str) {
        let Some(volume) = std::env::var_os(variable) else {
            eprintln!("SKIPPED: {variable} names no {file_system} drive");
            return;
        };
        let at_root = Path::new(&volume).join(format!("steno-{}", uuid::Uuid::new_v4()));
        create_dir_all_durably(&at_root).unwrap();
        std::fs::remove_dir(&at_root).unwrap();
        let root = tempfile::tempdir_in(&volume).unwrap();
        assert_eq!(windows::file_system_name(root.path()).unwrap(), file_system);
        assert!(may_lose_recent_writes(root.path()));
        let meeting = root.path().join("audio").join("0B6F4B1E");
        create_dir_all_durably(&meeting).unwrap();
        let source = root.path().join("upload.m4a");
        std::fs::write(&source, vec![7u8; 100_000]).unwrap();
        let destination = meeting.join("recording.m4a");
        copy_durably(&source, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), vec![7u8; 100_000]);
        assert_eq!(temporaries(&meeting), Vec::<String>::new());
    }

    #[cfg(windows)]
    #[test]
    fn on_a_fat32_drive_the_intake_writes_succeed_and_settings_warn() {
        the_intake_writes_succeed_and_settings_warn("STENO_FAT32_VOLUME", "FAT32");
    }

    #[cfg(windows)]
    #[test]
    fn on_an_exfat_drive_the_intake_writes_succeed_and_settings_warn() {
        the_intake_writes_succeed_and_settings_warn("STENO_EXFAT_VOLUME", "exFAT");
    }

    #[test]
    fn a_failed_rename_leaves_no_temporary() {
        let dir = tempfile::tempdir().unwrap();
        // A non-empty directory where the file should go: the rename fails.
        let path = dir.path().join("meeting.json");
        std::fs::create_dir_all(path.join("inside")).unwrap();
        assert!(replace_file(&path, b"{}", Access::Default).is_err());
        assert_eq!(temporaries(dir.path()), Vec::<String>::new());
    }
}
