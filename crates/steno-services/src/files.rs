//! Replacing a file in one step, for the secrets file and the CLI's
//! `meeting.json`. Swift: `Data.write(to:options: .atomic)`.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, SystemTime};

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
    // Dropping the temporary on an error below removes it.
    let mut temporary = builder.tempfile_in(directory)?;
    temporary.write_all(data)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    sync_directory(directory);
    Ok(())
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

/// Makes a rename in `directory` durable where the platform can sync a
/// directory; best effort.
fn sync_directory(directory: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = std::fs::File::open(directory) {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = directory;
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::{Arc, Barrier};

    use super::*;

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
