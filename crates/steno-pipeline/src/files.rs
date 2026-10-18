//! Durable file writes: replacing a file in one step, copying a recording
//! so it survives a power loss, creating and removing durably, and reading
//! and writing a JSON file this process owns, set aside when it does not
//! parse.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Makes the entries of `directory` durable, best effort: Windows opens no
/// folder for a flush through std, so there the rename alone counts.
fn sync_directory(directory: &Path) {
    if let Ok(handle) = File::open(directory) {
        let _ = handle.sync_all();
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
    write_durably(path, access, |file| file.write_all(data))
}

/// Copies `source` to `destination` the way [`replace_file`] writes, so once
/// this returns the copy survives a power loss. The phone intake needs that
/// before it marks the receipt complete, since the phone then deletes its
/// copy.
pub fn copy_durably(source: &Path, destination: &Path) -> std::io::Result<()> {
    let mut reader = File::open(source)?;
    write_durably(destination, Access::Default, |file| {
        std::io::copy(&mut reader, file).map(|_| ())
    })
}

/// Removes the file at `path` and syncs its folder; a file that is not there
/// is removed already.
pub fn remove_durably(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {
            sync_directory(folder_of(path));
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// `std::fs::create_dir_all`, with the parent of every folder it creates
/// synced.
pub fn create_dir_all_durably(directory: &Path) -> std::io::Result<()> {
    let missing: Vec<&Path> = directory
        .ancestors()
        .take_while(|ancestor| !ancestor.as_os_str().is_empty() && !ancestor.exists())
        .collect();
    std::fs::create_dir_all(directory)?;
    for created in missing.iter().rev() {
        sync_directory(folder_of(created));
    }
    Ok(())
}

/// Creates the folder `directory`, which must not exist yet, with its
/// missing parents, synced as [`create_dir_all_durably`] does. An existing
/// entry fails with `AlreadyExists` before anything is made, so a caller
/// that cleans up after a later failure removes only what it made.
pub fn create_new_dir_durably(directory: &Path) -> std::io::Result<()> {
    let parent = folder_of(directory);
    create_dir_all_durably(parent)?;
    std::fs::create_dir(directory)?;
    sync_directory(parent);
    Ok(())
}

fn folder_of(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// The one durable write: `write` fills a temporary beside `path`, which is
/// synced, renamed onto `path` and its folder synced.
fn write_durably(
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
    temporary.as_file().sync_all()?;
    // `std::fs::rename` rather than `persist`: std's rename replaces a target
    // another handle has open on Windows.
    let (file, temporary) = temporary.keep().map_err(|error| error.error)?;
    drop(file);
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    sync_directory(directory);
    Ok(())
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
            Ok(()) => match std::fs::remove_file(path) {
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
                    std::fs::rename(path, &candidate)?;
                    break candidate;
                }
                Err(error) => return Err(error),
                Ok(_) => attempt += 1,
            },
        }
    };
    if let Some(directory) = aside.parent() {
        sync_directory(directory);
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

    /// A missing source leaves no destination and no temporary.
    #[test]
    fn a_copy_that_fails_leaves_no_destination() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("recording.m4a");
        assert!(copy_durably(&dir.path().join("missing.m4a"), &destination).is_err());
        assert!(!destination.exists());
        assert_eq!(temporaries(dir.path()), Vec::<String>::new());
    }

    /// Missing parents are made; an existing folder fails with
    /// `AlreadyExists` and keeps what it holds.
    #[test]
    fn a_new_folder_is_made_and_an_existing_one_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let meeting = dir.path().join("audio").join("0B6F4B1E");
        create_new_dir_durably(&meeting).unwrap();
        assert!(meeting.is_dir());
        std::fs::write(meeting.join("recording.m4a"), b"an earlier recording").unwrap();
        let error = create_new_dir_durably(&meeting).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(meeting.join("recording.m4a")).unwrap(),
            b"an earlier recording"
        );
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
