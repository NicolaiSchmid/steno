//! Linux only: the `AppImage` runtime's mount server, found by its
//! keepalive pipe (X1 of `.plans/2026-10-07-stable-promotion.md`).
//!
//! Run from an `AppImage`, the app reads its files from the image's mount,
//! `$APPDIR`, which the runtime's own process, the mount server, serves.
//! The runtime forks the mount server, which starts a session of its own,
//! and its first process then becomes the app. The two share one link, the
//! keepalive pipe: the app inherits its read end, the mount server holds
//! its write end and ends once no process holds the read end any more.
//! Nothing else names the mount server: the mount's entry carries only the
//! image's file name, and its environment has no `$APPDIR`. So the mount
//! server is the process whose executable is the image (`$APPIMAGE`) and
//! that holds, open for writing, a pipe the app holds. Every instance has a
//! pipe of its own, so another instance's mount server never matches, and
//! the app's children hold only the read end. Reading another process's
//! `fd` directory needs what reading its `exe` needs: the same user.
//!
//! `own_scope` moves the mount server into a scope of its own, so the stop
//! of the unit it runs in cannot end the mount while the app saves.
//!
//! - [`mount_server`]: the one entry point, from the environment; a warning
//!   when the app runs from a mount whose server it cannot find.
//! - [`runs_from_mount`]: whether the app's executable is inside the mount.
//! - [`server_of`]: the scan of a `/proc` for the process that holds the
//!   pipe's write end.
//!
//! Swift: none; the Mac app ships no `AppImage`.

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

/// The mount server of the `AppImage` the app `pid` runs from, `None` when
/// the app does not run from one, or when no process holds the other end
/// of its keepalive pipe, which is logged.
pub fn mount_server(pid: u32) -> Option<u32> {
    let appdir = std::env::var_os("APPDIR").map(PathBuf::from);
    let exe = std::env::current_exe().ok()?;
    if !runs_from_mount(appdir.as_deref(), &exe) {
        return None;
    }
    let image = std::env::var_os("APPIMAGE").map(PathBuf::from);
    let server = image
        .as_deref()
        .and_then(|image| server_of(Path::new("/proc"), pid, image));
    if server.is_none() {
        tracing::warn!(
            appdir = ?appdir,
            image = ?image,
            "Steno runs from an AppImage whose mount server it cannot find, so the server stays where it runs, and the session's end may stop it while Steno saves a recording"
        );
    }
    server
}

/// Whether the app's executable `exe` is inside the mount `appdir`
/// (`$APPDIR`): not only inherited from a launcher that is an `AppImage`.
fn runs_from_mount(appdir: Option<&Path>, exe: &Path) -> bool {
    appdir.is_some_and(|mount| exe.starts_with(mount))
}

/// The process under `proc` other than `pid` whose executable is the file
/// `image` and that holds open for writing a pipe that `pid` holds open for
/// reading.
fn server_of(proc: &Path, pid: u32, image: &Path) -> Option<u32> {
    let image = std::fs::metadata(image).ok()?;
    let read = pipes(&proc.join(pid.to_string()), libc::O_RDONLY);
    if read.is_empty() {
        return None;
    }
    std::fs::read_dir(proc)
        .ok()?
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|&other| other != pid)
        .find(|other| {
            let process = proc.join(other.to_string());
            std::fs::metadata(process.join("exe"))
                .is_ok_and(|exe| (exe.dev(), exe.ino()) == (image.dev(), image.ino()))
                && !pipes(&process, libc::O_WRONLY).is_disjoint(&read)
        })
}

/// The pipes the process at `process` (its `/proc` directory) holds open
/// with the access mode `mode`, as their `fd` links name them
/// (`pipe:[<inode>]`).
fn pipes(process: &Path, mode: i32) -> HashSet<PathBuf> {
    std::fs::read_dir(process.join("fd"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|fd| {
            let link = std::fs::read_link(fd.path()).ok()?;
            let fdinfo = std::fs::read_to_string(process.join("fdinfo").join(fd.file_name()));
            (link.as_os_str().as_encoded_bytes().starts_with(b"pipe:")
                && access(&fdinfo.ok()?) == Some(mode))
            .then_some(link)
        })
        .collect()
}

/// The access mode (`O_RDONLY`, `O_WRONLY`, `O_RDWR`) of an `fdinfo`
/// file's `flags` (octal).
fn access(fdinfo: &str) -> Option<i32> {
    let flags = fdinfo
        .lines()
        .find_map(|line| line.strip_prefix("flags:"))?;
    Some(i32::from_str_radix(flags.trim(), 8).ok()? & libc::O_ACCMODE)
}

#[cfg(test)]
mod tests {
    use std::process::{Child, Command, Stdio};

    use super::*;

    #[test]
    fn the_app_runs_from_the_mount_only_when_its_executable_is_inside_it() {
        let mount = Path::new("/tmp/.mount_stenoAb12");
        assert!(runs_from_mount(
            Some(mount),
            &mount.join("usr/bin/steno-desktop")
        ));
        // Inherited from a launcher that is an AppImage; a sibling mount;
        // no mount.
        assert!(!runs_from_mount(
            Some(mount),
            Path::new("/usr/bin/steno-desktop")
        ));
        assert!(!runs_from_mount(
            Some(mount),
            Path::new("/tmp/.mount_stenoAb123/usr/bin/steno-desktop")
        ));
        assert!(!runs_from_mount(None, &mount.join("usr/bin/steno-desktop")));
    }

    #[test]
    fn the_access_mode_is_read_from_the_flags() {
        let fdinfo = |flags: &str| format!("pos:\t0\nflags:\t{flags}\nmnt_id:\t15\nino:\t42\n");
        assert_eq!(access(&fdinfo("00")), Some(libc::O_RDONLY));
        // Close-on-exec and non-blocking do not count.
        assert_eq!(access(&fdinfo("02000000")), Some(libc::O_RDONLY));
        assert_eq!(access(&fdinfo("01")), Some(libc::O_WRONLY));
        assert_eq!(access(&fdinfo("02004001")), Some(libc::O_WRONLY));
        assert_eq!(access(&fdinfo("02")), Some(libc::O_RDWR));
        assert_eq!(access("pos:\t0\n"), None);
        assert_eq!(access("flags:\tx\n"), None);
    }

    /// `sleep` with `stdin` and `stdout`, and no stderr, which may be a
    /// pipe this test holds too.
    fn sleeping(stdin: Stdio, stdout: Stdio) -> Child {
        Command::new("sleep")
            .arg("30")
            .stdin(stdin)
            .stdout(stdout)
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    /// Real processes stand in: this test is the app and holds the read
    /// end of a pipe, and `sleep` is the image. The one that holds the
    /// write end is the server. One that holds the read end too (as the
    /// app's children do), one that holds the write end of a pipe this
    /// test writes to as well (a shared stderr), and one with no pipe are
    /// not.
    #[test]
    fn the_mount_server_holds_the_write_end_of_the_apps_pipe() {
        let (reader, writer) = std::io::pipe().unwrap();
        let (shared_reader, shared) = std::io::pipe().unwrap();
        drop(shared_reader);
        let mut children = vec![
            sleeping(Stdio::null(), Stdio::null()),
            sleeping(Stdio::from(reader.try_clone().unwrap()), Stdio::null()),
            sleeping(Stdio::null(), Stdio::from(shared.try_clone().unwrap())),
        ];
        let server = sleeping(Stdio::null(), Stdio::from(writer));
        let id = server.id();
        children.push(server);
        let proc = Path::new("/proc");
        let image = std::fs::read_link(format!("/proc/{id}/exe")).unwrap();
        let own = std::process::id();

        assert_eq!(server_of(proc, own, &image), Some(id));
        // Another image; a missing one; seen from a process with no pipe
        // of the server's; a `/proc` with no processes.
        let test = std::env::current_exe().unwrap();
        assert_eq!(server_of(proc, own, &test), None);
        assert_eq!(server_of(proc, own, Path::new("/nonexistent")), None);
        assert_eq!(server_of(proc, children[0].id(), &image), None);
        let empty = std::env::temp_dir().join(format!("steno-appimage-{own}"));
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(server_of(&empty, own, &image), None);
        std::fs::remove_dir(&empty).unwrap();

        // Once the app holds no end of the pipe, nothing matches.
        drop(reader);
        assert_eq!(server_of(proc, own, &image), None);
        for mut child in children {
            child.kill().unwrap();
            child.wait().unwrap();
        }
    }
}
