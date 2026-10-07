//! Durable file writes: replacing a file in one step (the secrets file,
//! `preferences.json`, the CLI's `meeting.json`; Swift:
//! `Data.write(to:options: .atomic)`), copying a recording so it survives a
//! power loss (the phone intake), creating folders whose entries survive
//! one, and setting aside a file that does not parse (`preferences.json`).
//! The services and the CLI use these too, so there is one implementation.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The syncs a durable write makes; the disk in the product, a recorder in
/// the tests, which check what is synced and when.
trait Syncs {
    /// Flushes `file`, written at `path`, to the disk.
    fn file(&self, file: &File, path: &Path) -> std::io::Result<()>;
    /// Makes the entries of `directory` (a rename, a new folder) durable;
    /// best effort where the platform cannot sync a folder.
    fn directory(&self, directory: &Path);
}

/// The product's syncs.
struct Disk;

impl Syncs for Disk {
    fn file(&self, file: &File, _path: &Path) -> std::io::Result<()> {
        file.sync_all()
    }

    fn directory(&self, directory: &Path) {
        #[cfg(unix)]
        if let Ok(handle) = File::open(directory) {
            let _ = handle.sync_all();
        }
        #[cfg(not(unix))]
        let _ = directory;
    }
}

/// Who may read a file [`replace_file`] writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Mode 0600 where the platform has modes: the secrets file.
    OwnerOnly,
    /// The mode any new file gets (0666 less the umask), as Swift's atomic
    /// write gave `meeting.json`.
    Default,
}

/// The suffix of the temporary files [`replace_file`] writes.
const TEMPORARY_SUFFIX: &str = ".partial";

/// How old a temporary must be before a later write removes it as one a
/// killed writer left behind; a live writer's lives for milliseconds.
const STALE_AFTER: Duration = Duration::from_secs(60);

/// Replaces `path` with `data`. The bytes go to a temporary file of this
/// writer's own beside `path` (`.<name>.<random>.partial`), are synced, and
/// a rename puts them in place, so a reader, a crash or a full disk sees the
/// old file or the new one, never a torn one, and two writers into one
/// folder never share a temporary. The sync comes before the rename, so a
/// crash cannot leave the new name over empty or partly written data; the
/// folder is synced after it, so the rename itself survives a crash. A
/// failure removes the temporary; temporaries a killed writer left behind
/// are removed by a later write once they are a minute old.
pub fn replace_file(path: &Path, data: &[u8], access: Access) -> std::io::Result<()> {
    write_durably(&Disk, path, access, |file| file.write_all(data))
}

/// Copies `source` to `destination` the way [`replace_file`] writes: into a
/// temporary of its own beside `destination`, synced, renamed into place,
/// the folder synced, so once this returns the copy survives a power loss.
/// The phone intake copies an upload with it before it marks the receipt
/// complete, since the phone deletes its own copy then; Swift's
/// `RecordingIntake` used `copyItem`, which syncs nothing.
pub fn copy_durably(source: &Path, destination: &Path) -> std::io::Result<()> {
    copy_durably_with(&Disk, source, destination)
}

fn copy_durably_with(syncs: &dyn Syncs, source: &Path, destination: &Path) -> std::io::Result<()> {
    let mut reader = File::open(source)?;
    write_durably(syncs, destination, Access::Default, |file| {
        std::io::copy(&mut reader, file).map(|_| ())
    })
}

/// `std::fs::create_dir_all`, with the parent of every folder it creates
/// synced, so the new folders themselves survive a power loss.
pub fn create_dir_all_durably(directory: &Path) -> std::io::Result<()> {
    create_dir_all_durably_with(&Disk, directory)
}

fn create_dir_all_durably_with(syncs: &dyn Syncs, directory: &Path) -> std::io::Result<()> {
    let missing: Vec<&Path> = directory
        .ancestors()
        .take_while(|ancestor| !ancestor.as_os_str().is_empty() && !ancestor.exists())
        .collect();
    std::fs::create_dir_all(directory)?;
    for created in missing.iter().rev() {
        if let Some(parent) = created.parent().filter(|p| !p.as_os_str().is_empty()) {
            syncs.directory(parent);
        }
    }
    Ok(())
}

/// The one durable write: `write` fills a temporary of this writer's own
/// beside `path`, which is synced, renamed onto `path`, and the folder
/// synced. Dropping the temporary on an error removes it.
fn write_durably(
    syncs: &dyn Syncs,
    path: &Path,
    access: Access,
    write: impl FnOnce(&mut File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
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
    // From here the temporary is renamed with std's `rename` rather than
    // `persist`: on Windows std replaces a target another handle has open
    // (POSIX rename semantics), where `MoveFileEx` answers "access denied".
    let (file, temporary) = temporary.keep().map_err(|error| error.error)?;
    drop(file);
    if let Err(error) = rename_over(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    syncs.directory(directory);
    Ok(())
}

/// `std::fs::rename`. Windows refuses to replace a file another writer is
/// replacing at the same instant ("access denied"), so there a refusal is
/// retried every 10 ms, 49 times at most (about half a second); elsewhere
/// the rename is tried once.
fn rename_over(from: &Path, to: &Path) -> std::io::Result<()> {
    let retries = if cfg!(windows) { 49 } else { 0 };
    for _ in 0..retries {
        match std::fs::rename(from, to) {
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                std::thread::sleep(Duration::from_millis(10));
            }
            outcome => return outcome,
        }
    }
    std::fs::rename(from, to)
}

/// Removes this file's temporaries in `directory` that are older than
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

/// Moves `path` aside to `<name>.corrupt-<UTC time>` beside it, with `-2`,
/// `-3` and so on added when that name is taken, so an earlier copy set
/// aside is never replaced; the folder is synced after the move. A reader
/// that cannot parse a file it owns calls this before it starts empty, so
/// the next write cannot replace bytes nobody has looked at. Returns the
/// new path.
///
/// The new name is claimed with a hard link, which fails when the name is
/// taken, so a copy another process sets aside in the same second is never
/// replaced; the old name is removed after. Where no hard link can be made
/// the move is a rename, which replaces a taken name, so a check that the
/// name is free comes first and is all that guards it there. If the old
/// name cannot be removed after the link, the error is returned and both
/// names hold the bytes.
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
            Ok(()) => {
                std::fs::remove_file(path)?;
                break candidate;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => attempt += 1,
            // No hard link here (FAT, some network shares, a Linux that
            // protects links to files of other users), so fall back to a
            // rename; where the link failed for another reason (a
            // read-only folder), the rename fails the same way.
            Err(_) => match candidate.symlink_metadata() {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::rename(path, &candidate)?;
                    break candidate;
                }
                Err(error) => return Err(error),
                Ok(_) => attempt += 1,
            },
        }
    };
    if let Some(directory) = aside.parent() {
        Disk.directory(directory);
    }
    Ok(aside)
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

    /// A second file set aside gets a name of its own; the first copy
    /// keeps its bytes.
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

    fn temporaries(directory: &Path) -> Vec<String> {
        std::fs::read_dir(directory)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(TEMPORARY_SUFFIX))
            .collect()
    }

    /// Writers into one folder, two files, all at once: every write lands,
    /// each file holds one writer's whole payload, and no temporary is
    /// left. With one shared temporary name this lost writes and tore a
    /// file.
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

    /// A temporary a killed writer left behind goes with the next write
    /// once it is a minute old; a fresh one, a live writer's, stays.
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

    /// What a test's syncs saw, in order: `file`, with whether the
    /// destination existed yet, and `directory`, with the same.
    struct Recorded {
        destination: std::path::PathBuf,
        events: std::sync::Mutex<Vec<(String, bool)>>,
        fail_file_sync: bool,
    }

    impl Recorded {
        fn new(destination: &Path) -> Self {
            Recorded {
                destination: destination.to_path_buf(),
                events: std::sync::Mutex::new(Vec::new()),
                fail_file_sync: false,
            }
        }

        fn events(&self) -> Vec<(String, bool)> {
            self.events.lock().unwrap().clone()
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

        fn directory(&self, directory: &Path) {
            self.events.lock().unwrap().push((
                format!("directory {}", directory.display()),
                self.destination.exists(),
            ));
        }
    }

    /// The copy is synced before the rename and the folder after it, so a
    /// complete copy is on the disk when it returns: the content is the
    /// source's and no temporary is left.
    #[test]
    fn a_durable_copy_syncs_the_file_then_renames_then_syncs_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("upload.m4a");
        std::fs::write(&source, vec![7u8; 100_000]).unwrap();
        let destination = dir.path().join("meeting").join("recording.m4a");
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let syncs = Recorded::new(&destination);
        copy_durably_with(&syncs, &source, &destination).unwrap();
        assert_eq!(
            syncs.events(),
            [
                ("file".to_owned(), false),
                (
                    format!("directory {}", destination.parent().unwrap().display()),
                    true
                ),
            ]
        );
        assert_eq!(std::fs::read(&destination).unwrap(), vec![7u8; 100_000]);
        assert_eq!(
            temporaries(destination.parent().unwrap()),
            Vec::<String>::new()
        );
    }

    /// A failure before the rename leaves no destination and no
    /// temporary, so nothing half copied is ever taken for the recording.
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

    /// Every folder created is made durable by syncing its parent; folders
    /// that existed are left alone.
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
