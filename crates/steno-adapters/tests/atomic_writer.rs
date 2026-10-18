//! The atomic file writer: a failed write never loses or truncates the
//! target and leaves no temp file.

mod common;

use std::fs;
use std::path::Path;

use common::*;
use steno_adapters::fs::AtomicFileWriter;

#[test]
fn writes_and_replaces_the_bytes_and_leaves_no_temporaries() {
    let directory = temp_dir();
    let target = directory.path().join("note.md");
    AtomicFileWriter::write(b"one\n", &target).unwrap();
    AtomicFileWriter::write(b"two\n", &target).unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"two\n");
    assert_eq!(list(directory.path()), ["note.md"]);
}

/// Each failure names the caller's target and its step, leaves the target
/// as it was and removes the temp file.
#[test]
fn a_failed_write_leaves_the_target_and_no_temporary() {
    let directory = temp_dir();
    // A directory at the target fails the rename after the fsync.
    let target = directory.path().join("note.md");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("keep.txt"), b"inside\n").unwrap();
    let error = AtomicFileWriter::write(b"new\n", &target).unwrap_err();
    assert_eq!(error.path, target.to_string_lossy());
    assert!(error.underlying.starts_with("rename"), "{error}");
    assert_eq!(list(directory.path()), ["note.md"]);
    assert_eq!(list(&target), ["keep.txt"]);

    let orphan = directory.path().join("absent/note.md");
    let error = AtomicFileWriter::write(b"new\n", &orphan).unwrap_err();
    assert_eq!(error.path, orphan.to_string_lossy());
    assert!(error.underlying.starts_with("open"), "{error}");
}

#[cfg(unix)]
#[test]
fn a_read_only_directory_fails_and_keeps_the_target() {
    use std::os::unix::fs::PermissionsExt as _;
    let directory = temp_dir();
    let target = directory.path().join("note.md");
    AtomicFileWriter::write(b"keep\n", &target).unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o555)).unwrap();
    let written = AtomicFileWriter::write(b"new\n", &target);
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
    if written.is_ok() {
        return; // root: the permission bits do not bite
    }
    assert!(written.unwrap_err().underlying.starts_with("open"));
    assert_eq!(fs::read(&target).unwrap(), b"keep\n");
    assert_eq!(list(directory.path()), ["note.md"]);
}

/// On Windows a target held without delete sharing refuses the rename past
/// its retries and keeps its bytes.
#[cfg(windows)]
#[test]
fn a_target_held_past_the_retries_fails_and_keeps_its_bytes() {
    use std::os::windows::fs::OpenOptionsExt as _;
    /// `FILE_SHARE_READ | FILE_SHARE_WRITE`, without `FILE_SHARE_DELETE`.
    const SHARE_READ_WRITE: u32 = 0x1 | 0x2;
    let directory = temp_dir();
    let target = directory.path().join("note.md");
    AtomicFileWriter::write(b"keep\n", &target).unwrap();
    let holder = fs::OpenOptions::new()
        .read(true)
        .share_mode(SHARE_READ_WRITE)
        .open(&target)
        .unwrap();
    let error = AtomicFileWriter::write(b"new\n", &target).unwrap_err();
    drop(holder);
    assert!(error.underlying.starts_with("rename"), "{error}");
    assert_eq!(fs::read(&target).unwrap(), b"keep\n");
    assert_eq!(list(directory.path()), ["note.md"]);
}

#[test]
fn stale_temporaries_are_removed_and_nothing_else() {
    let directory = temp_dir();
    for name in [".steno-tmp-deadbeef-note.md", "notes.md", ".obsidian-thing"] {
        fs::write(directory.path().join(name), b"").unwrap();
    }
    AtomicFileWriter::remove_stale_temporaries(directory.path());
    assert_eq!(list(directory.path()), [".obsidian-thing", "notes.md"]);
}

/// `.steno-tmp-<8 hex>-<name>` beside the target, cut on a character
/// boundary to fit 255 bytes; a 255-byte target name is still written.
#[test]
fn temporary_names_sit_beside_the_target_and_fit_255_bytes() {
    let temporary = AtomicFileWriter::temporary_path(Path::new("/vault/Meetings/x/note.md"));
    assert_eq!(temporary.parent(), Some(Path::new("/vault/Meetings/x")));
    let name = temporary.file_name().unwrap().to_str().unwrap();
    let hex = &name[".steno-tmp-".len()..][..8];
    assert_eq!(name, format!(".steno-tmp-{hex}-note.md"));
    assert!(
        hex.bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
    );

    let long = format!("{}.md", "ü".repeat(130));
    let temporary = AtomicFileWriter::temporary_path(&Path::new("/vault").join(&long));
    let name = temporary.file_name().unwrap().to_str().unwrap();
    assert_eq!(name.len(), 254, "two-byte characters: one byte short");
    assert!(name.ends_with("üü"), "{name}");

    // Windows' path limit would refuse the full path, not the name.
    #[cfg(unix)]
    {
        let directory = temp_dir();
        let name = format!("{}.md", "a".repeat(252));
        AtomicFileWriter::write(b"long\n", &directory.path().join(&name)).unwrap();
        assert_eq!(list(directory.path()), [name]);
    }
}
