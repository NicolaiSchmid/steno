//! `AtomicFileWriterTests`.

mod common;

use std::fs;
use std::path::Path;

use common::*;
use steno_adapters::fs::AtomicFileWriter;

#[test]
fn writes_the_bytes_and_leaves_no_temporaries() {
    let directory = temp_dir("atomic");
    let target = directory.path().join("note.md");
    AtomicFileWriter::write(b"one\n", &target).unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"one\n");
    AtomicFileWriter::write(b"two\n", &target).unwrap();
    assert_eq!(
        fs::read(&target).unwrap(),
        b"two\n",
        "an existing file is replaced"
    );
    assert_eq!(list(directory.path()), ["note.md"]);
}

#[cfg(unix)]
#[test]
fn read_only_directory_fails_without_residue() {
    use std::os::unix::fs::PermissionsExt as _;
    let directory = temp_dir("atomic-ro");
    let target = directory.path().join("note.md");
    AtomicFileWriter::write(b"keep\n", &target).unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o555)).unwrap();
    let probe = directory.path().join("probe");
    if fs::write(&probe, b"").is_ok() {
        // Running as root: the permission bits do not bite.
        let _ = fs::remove_file(&probe);
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let error = AtomicFileWriter::write(b"new\n", &target).unwrap_err();
    assert!(error.path.ends_with("note.md"));
    assert!(error.underlying.starts_with("open"), "{}", error.underlying);
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        fs::read(&target).unwrap(),
        b"keep\n",
        "the target is untouched"
    );
    assert_eq!(list(directory.path()), ["note.md"]);
}

#[test]
fn stale_temporaries_are_removed_and_nothing_else() {
    let directory = temp_dir("atomic-stale");
    fs::write(directory.path().join(".steno-tmp-note.md-deadbeef"), b"").unwrap();
    fs::write(directory.path().join("notes.md"), b"").unwrap();
    fs::write(directory.path().join(".obsidian-thing"), b"").unwrap();
    AtomicFileWriter::remove_stale_temporaries(directory.path());
    assert_eq!(list(directory.path()), [".obsidian-thing", "notes.md"]);
}

#[test]
fn temporary_names_sit_beside_the_target_with_eight_hex_digits() {
    let target = std::path::Path::new("/vault/Meetings/x/note.md");
    let temporary = AtomicFileWriter::temporary_path(target);
    assert_eq!(temporary.parent(), target.parent());
    let name = temporary
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let hex = name
        .strip_prefix(".steno-tmp-")
        .unwrap()
        .strip_suffix("-note.md")
        .unwrap();
    assert_eq!(hex.len(), 8);
    assert!(
        hex.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
}

#[test]
fn temporary_names_fit_in_255_bytes_and_cut_on_a_character_boundary() {
    let long = format!("{}.md", "ü".repeat(130));
    assert!(long.len() > 255);
    let temporary = AtomicFileWriter::temporary_path(Path::new("/vault").join(&long).as_path());
    let name = temporary.file_name().unwrap().to_str().unwrap().to_owned();
    assert_eq!(name.len(), 255 - 1, "two-byte characters: one byte short");
    assert!(name.starts_with(".steno-tmp-"));
    assert!(name.ends_with("üü"), "{name}");
    let exact = "a".repeat(255);
    let temporary = AtomicFileWriter::temporary_path(Path::new("/vault").join(&exact).as_path());
    assert_eq!(temporary.file_name().unwrap().len(), 255);
}

#[cfg(unix)]
#[test]
fn a_target_name_of_255_bytes_is_written() {
    let directory = temp_dir("atomic-long");
    let name = format!("{}.md", "a".repeat(252));
    assert_eq!(name.len(), 255);
    let target = directory.path().join(&name);
    AtomicFileWriter::write(b"long\n", &target).unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"long\n");
    assert_eq!(list(directory.path()), [name]);
}

#[test]
fn a_failed_rename_removes_the_temporary_and_leaves_the_target_alone() {
    // The target is a directory, so the bytes reach the temp file and fsync,
    // and the rename fails; this path needs no permission bits, so it bites
    // as root too.
    let directory = temp_dir("atomic-rename");
    let target = directory.path().join("note.md");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("keep.txt"), b"inside\n").unwrap();
    let error = AtomicFileWriter::write(b"new\n", &target).unwrap_err();
    assert_eq!(error.path, target.to_string_lossy());
    assert!(
        error.underlying.starts_with("rename"),
        "{}",
        error.underlying
    );
    assert_eq!(list(directory.path()), ["note.md"]);
    assert_eq!(list(&target), ["keep.txt"]);
}

#[test]
fn a_missing_parent_directory_fails_naming_the_target() {
    let directory = temp_dir("atomic-parent");
    let target = directory.path().join("absent/note.md");
    let error = AtomicFileWriter::write(b"new\n", &target).unwrap_err();
    assert_eq!(
        error.path,
        target.to_string_lossy(),
        "the failure names the file the caller asked for"
    );
    assert!(error.underlying.starts_with("open"));
    assert_eq!(list(directory.path()), Vec::<String>::new());
}

/// On Windows a target another handle holds without sharing its deletion
/// (a sync or antivirus client) refuses the rename past its retries: the
/// write fails, the target keeps its bytes and no temporary is left.
#[cfg(windows)]
#[test]
fn a_target_held_past_the_retries_fails_and_keeps_its_bytes() {
    use std::os::windows::fs::OpenOptionsExt as _;
    /// `FILE_SHARE_READ | FILE_SHARE_WRITE`, without `FILE_SHARE_DELETE`.
    const SHARE_READ_WRITE: u32 = 0x1 | 0x2;
    let directory = temp_dir("atomic-held");
    let target = directory.path().join("note.md");
    AtomicFileWriter::write(b"keep\n", &target).unwrap();
    let holder = fs::OpenOptions::new()
        .read(true)
        .share_mode(SHARE_READ_WRITE)
        .open(&target)
        .unwrap();
    let error = AtomicFileWriter::write(b"new\n", &target).unwrap_err();
    drop(holder);
    assert!(
        error.underlying.starts_with("rename"),
        "{}",
        error.underlying
    );
    assert_eq!(fs::read(&target).unwrap(), b"keep\n");
    assert_eq!(list(directory.path()), ["note.md"]);
}
