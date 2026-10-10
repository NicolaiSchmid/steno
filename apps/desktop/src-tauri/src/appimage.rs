//! Linux only: the `AppImage` runtime's mount servers, found by their
//! FUSE connection and keepalive pipe (X1 of
//! `.plans/2026-10-07-stable-promotion.md`).
//!
//! Run from an `AppImage`, the app reads its files from the image's mount,
//! `$APPDIR`, which the runtime's own process, the mount server, serves.
//! The runtime forks the mount server, which starts a session of its own,
//! and its first process then becomes the app. The mount server holds
//! `/dev/fuse` and the write end of the keepalive pipe; the app inherits
//! the pipe's read end, and the mount server ends once no process holds
//! that any more. Neither the mount's entry (the image's file name) nor the
//! mount server's environment (no `$APPDIR`) names it, and its executable
//! need not be the image: a launcher may run the runtime from a copy in
//! memory (`AppImageLauncher`), and the image file may have been replaced
//! since it started (an update). So a mount server is any process other
//! than the app that holds both of these:
//!
//! - a `/dev/fuse` descriptor whose `fdinfo` names the mount's FUSE
//!   connection (`fuse_connection`, Linux 6.16 and later), or names none
//!   (older kernels);
//! - open for writing, a pipe the app holds open for reading.
//!
//! From Linux 6.16 that is the app's own mount server and no other. Before
//! 6.16 it may also be another image's mount server whose keepalive pipe
//! the app inherited from its launcher; moving that one too does no harm,
//! since it still ends once its last reader closes the pipe. Reading
//! another process's `fd` and `fdinfo` needs the same user.
//!
//! Once found, the app's read ends of those pipes are closed on exec: a
//! process the app starts inherits every descriptor that is not, so an
//! update's relaunch would keep the old mount server alive for its whole
//! session otherwise.
//!
//! `own_scope` moves each mount server into a scope of its own, so the stop
//! of the unit it runs in cannot end the mount while the app saves.
//!
//! - [`mount_servers`]: the one entry point, from the environment.
//! - [`servers_at`]: whether the app runs from a FUSE mount, the scan, and
//!   a warning when the app runs from one whose server it cannot find.
//! - [`fuse_connection`]: the FUSE connection of a mount, `None` for any
//!   other file system (an image extracted and run from a folder).
//! - [`servers_of`]: the scan of a `/proc` for the processes that hold the
//!   connection and the write end of the app's pipe.
//!
//! Swift: none; the Mac app ships no `AppImage`.

use std::collections::HashSet;
use std::os::fd::{BorrowedFd, RawFd};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

/// `FUSE_SUPER_MAGIC`, the file system type `statfs` gives a FUSE mount.
const FUSE_SUPER_MAGIC: u32 = 0x6573_5546;

/// What the scan found: the mount servers, and the app's descriptors that
/// hold the read ends of their pipes.
#[derive(Debug, Default, PartialEq, Eq)]
struct Found {
    servers: Vec<u32>,
    read_ends: Vec<RawFd>,
}

/// The mount servers of the `AppImage` the app `pid` runs from: none when
/// the app does not run from one, or when no process holds its FUSE
/// connection and the other end of its keepalive pipe, which is logged.
/// The app's read ends of their pipes are closed on exec from then on.
pub fn mount_servers(pid: u32) -> Vec<u32> {
    let appdir = std::env::var_os("APPDIR").map(PathBuf::from);
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    let found = servers_at(
        Path::new("/proc"),
        pid,
        appdir.as_deref(),
        &exe,
        fuse_connection,
    );
    close_on_exec(&found.read_ends);
    found.servers
}

/// The mount servers under `proc` of the app `pid`, whose executable is
/// `exe`, when it runs from the mount `appdir` (`$APPDIR`) and
/// `connection_of` gives that mount's FUSE connection; a warning when it
/// does and no server is found.
fn servers_at(
    proc: &Path,
    pid: u32,
    appdir: Option<&Path>,
    exe: &Path,
    connection_of: impl Fn(&Path) -> Option<u32>,
) -> Found {
    let Some(mount) = appdir.filter(|mount| runs_from_mount(mount, exe)) else {
        return Found::default();
    };
    let Some(connection) = connection_of(mount) else {
        return Found::default();
    };
    let found = servers_of(proc, pid, connection);
    if found.servers.is_empty() {
        tracing::warn!(
            appdir = %mount.display(),
            connection,
            "Steno runs from an AppImage whose mount server it cannot find, so the server stays where it runs, and the session's end may stop it while Steno saves a recording"
        );
    }
    found
}

/// Whether the app's executable `exe` is inside the mount `appdir`: not
/// only inherited from a launcher that is an `AppImage`.
fn runs_from_mount(appdir: &Path, exe: &Path) -> bool {
    exe.starts_with(appdir)
}

/// The FUSE connection of the mount at `mount`, as `/dev/fuse`'s `fdinfo`
/// names it; `None` when it is no FUSE mount, as when the runtime extracted
/// the image into a folder and runs it from there.
fn fuse_connection(mount: &Path) -> Option<u32> {
    let stats = rustix::fs::statfs(mount).ok()?;
    // The magic numbers are 32 bits; where `f_type` is a signed 32-bit word
    // the cast keeps the bits of the ones above `i32::MAX`.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::unnecessary_cast,
        reason = "f_type's width and sign differ between targets"
    )]
    let magic = stats.f_type as u32;
    if magic != FUSE_SUPER_MAGIC {
        return None;
    }
    Some(kernel_dev(std::fs::metadata(mount).ok()?.dev()))
}

/// A device number as the kernel encodes it inside, `major << 20 | minor`,
/// which is how `fdinfo` prints a FUSE connection; `stat`'s `st_dev` is the
/// user-space encoding of the same number.
fn kernel_dev(dev: u64) -> u32 {
    rustix::fs::major(dev) << 20 | rustix::fs::minor(dev)
}

/// The processes under `proc` other than `pid` that serve the FUSE
/// `connection` and hold open for writing a pipe that `pid` holds open for
/// reading, and `pid`'s descriptors of those pipes.
fn servers_of(proc: &Path, pid: u32, connection: u32) -> Found {
    let read = pipes(&proc.join(pid.to_string()), libc::O_RDONLY);
    let mut found = Found::default();
    if read.is_empty() {
        return found;
    }
    let mut kept = HashSet::new();
    let others = std::fs::read_dir(proc)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|&other| other != pid);
    for other in others {
        let process = proc.join(other.to_string());
        if !serves(&process, connection) {
            continue;
        }
        let written: HashSet<PathBuf> = pipes(&process, libc::O_WRONLY)
            .into_iter()
            .map(|(_, pipe)| pipe)
            .collect();
        let shared: Vec<&PathBuf> = read
            .iter()
            .map(|(_, pipe)| pipe)
            .filter(|pipe| written.contains(*pipe))
            .collect();
        if !shared.is_empty() {
            found.servers.push(other);
            kept.extend(shared);
        }
    }
    found.servers.sort_unstable();
    found.read_ends = read
        .iter()
        .filter(|(_, pipe)| kept.contains(pipe))
        .map(|&(fd, _)| fd)
        .collect();
    found.read_ends.sort_unstable();
    found
}

/// Whether the process at `process` (its `/proc` directory) holds
/// `/dev/fuse` for the FUSE `connection`. Before Linux 6.16 `fdinfo` names
/// no connection, so any `/dev/fuse` counts; the pipe still has to match.
fn serves(process: &Path, connection: u32) -> bool {
    descriptors(process).any(|(fd, link)| {
        link == Path::new("/dev/fuse")
            && fdinfo(process, fd).is_some_and(|info| match field(&info, "fuse_connection") {
                Some(named) => named.parse() == Ok(connection),
                None => true,
            })
    })
}

/// The descriptors the process at `process` holds, each with its number
/// and what its `fd` link names.
fn descriptors(process: &Path) -> impl Iterator<Item = (RawFd, PathBuf)> {
    std::fs::read_dir(process.join("fd"))
        .into_iter()
        .flatten()
        .filter_map(|fd| {
            let fd = fd.ok()?;
            let number = fd.file_name().to_str()?.parse().ok()?;
            Some((number, std::fs::read_link(fd.path()).ok()?))
        })
}

/// The `fdinfo` of the descriptor `fd` of the process at `process`.
fn fdinfo(process: &Path, fd: RawFd) -> Option<String> {
    std::fs::read_to_string(process.join("fdinfo").join(fd.to_string())).ok()
}

/// The pipes the process at `process` holds open with the access mode
/// `mode`, each with its descriptor and its `fd` link (`pipe:[<inode>]`).
/// A pipe whose `fdinfo` cannot be read is left out.
fn pipes(process: &Path, mode: i32) -> Vec<(RawFd, PathBuf)> {
    descriptors(process)
        .filter(|(fd, link)| {
            link.as_os_str().as_encoded_bytes().starts_with(b"pipe:")
                && fdinfo(process, *fd).is_some_and(|info| access(&info) == Some(mode))
        })
        .collect()
}

/// The value of an `fdinfo` file's field `name`.
fn field<'a>(fdinfo: &'a str, name: &str) -> Option<&'a str> {
    fdinfo
        .lines()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
        .map(str::trim)
}

/// The access mode (`O_RDONLY`, `O_WRONLY`, `O_RDWR`) of an `fdinfo`
/// file's `flags` (octal).
fn access(fdinfo: &str) -> Option<i32> {
    Some(i32::from_str_radix(field(fdinfo, "flags")?, 8).ok()? & libc::O_ACCMODE)
}

/// Sets close-on-exec on each of this process's descriptors `fds`; a
/// descriptor it cannot be set on is logged.
fn close_on_exec(fds: &[RawFd]) {
    for &fd in fds {
        // SAFETY: `fd` was read from this process's own `fd` directory a
        // moment ago, and it is a pipe end the runtime left open across
        // its exec, which nothing in the app owns or closes, so it stays
        // open while it is borrowed here. Only its descriptor flags change.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let set = rustix::io::fcntl_getfd(borrowed).and_then(|flags| {
            rustix::io::fcntl_setfd(borrowed, flags | rustix::io::FdFlags::CLOEXEC)
        });
        if let Err(error) = set {
            tracing::warn!(
                fd,
                %error,
                "the AppImage's keepalive pipe stays open across exec, so a process Steno starts may keep the mount server alive after Steno exits"
            );
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::symlink;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// The pipe the fake app reads from its runtime, as its `fd` link
    /// names it.
    const KEEPALIVE: &str = "pipe:[100]";
    /// The FUSE connection of the fake app's mount.
    const CONNECTION: u32 = 79;

    /// A fake `/proc` in a temporary directory, its processes made of
    /// `fd` links, `fdinfo` files and an `exe` link.
    struct FakeProc(tempfile::TempDir);

    /// One descriptor of a fake process: its link and its `fdinfo`.
    #[derive(Clone, Copy)]
    enum Fd<'a> {
        /// A pipe's end, read or written.
        Reads(&'a str),
        Writes(&'a str),
        /// `/dev/fuse`, with the `fuse_connection` line of Linux 6.16 and
        /// later, or without it.
        Fuse(Option<u32>),
        /// `/dev/fuse` with no `fdinfo` (the descriptor closed meanwhile).
        FuseGone,
        /// A file.
        File(&'a str),
    }

    impl FakeProc {
        fn new() -> Self {
            let proc = Self(tempfile::tempdir().unwrap());
            // Entries that are no process.
            std::fs::create_dir(proc.0.path().join("sys")).unwrap();
            symlink("4242", proc.0.path().join("self")).unwrap();
            proc
        }

        /// The process `pid` with the executable `exe` and the
        /// descriptors `fds`, numbered from 3.
        fn process(&self, pid: u32, exe: &str, fds: &[Fd]) -> &Self {
            let process = self.0.path().join(pid.to_string());
            std::fs::create_dir_all(process.join("fd")).unwrap();
            std::fs::create_dir_all(process.join("fdinfo")).unwrap();
            symlink(exe, process.join("exe")).unwrap();
            for (number, fd) in (3..).zip(fds) {
                let (link, info) = match fd {
                    Fd::Reads(pipe) => (*pipe, Some("flags:\t02000000\n".to_owned())),
                    Fd::Writes(pipe) => (*pipe, Some("flags:\t02000001\n".to_owned())),
                    Fd::Fuse(None) => ("/dev/fuse", Some("flags:\t02100002\n".to_owned())),
                    Fd::Fuse(Some(connection)) => (
                        "/dev/fuse",
                        Some(format!(
                            "flags:\t02100002\nfuse_connection:\t{connection}\n"
                        )),
                    ),
                    Fd::FuseGone => ("/dev/fuse", None),
                    Fd::File(path) => (*path, Some("flags:\t02100000\n".to_owned())),
                };
                symlink(link, process.join("fd").join(number.to_string())).unwrap();
                if let Some(info) = info {
                    let info = format!("pos:\t0\n{info}mnt_id:\t15\nino:\t42\n");
                    std::fs::write(process.join("fdinfo").join(number.to_string()), info).unwrap();
                }
            }
            self
        }

        fn servers(&self, pid: u32) -> Found {
            servers_of(self.0.path(), pid, CONNECTION)
        }
    }

    /// The fake app 4242: the keepalive pipe's read end at fd 3, a pipe
    /// it writes (stderr), and the mount's folder.
    fn app<'a>(extra: &[Fd<'a>]) -> Vec<Fd<'a>> {
        [
            Fd::Reads(KEEPALIVE),
            Fd::Writes("pipe:[200]"),
            Fd::File("/tmp/.mount_stenoAb12"),
        ]
        .into_iter()
        .chain(extra.iter().copied())
        .collect()
    }

    const IMAGE: &str = "/home/u/Applications/Steno.AppImage";

    /// The app's own mount server, its connection named or not, is found
    /// by the connection and the pipe, whatever its executable: the image,
    /// a copy in memory, an image file replaced since it started.
    #[test]
    fn the_mount_server_holds_the_mounts_connection_and_the_apps_pipe() {
        for connection in [Some(CONNECTION), None] {
            for exe in [
                IMAGE,
                "/memfd:appimagelauncher-runtime (deleted)",
                "/home/u/Applications/Steno.AppImage (deleted)",
            ] {
                let proc = FakeProc::new();
                proc.process(
                    4242,
                    "/tmp/.mount_stenoAb12/usr/bin/steno-desktop",
                    &app(&[]),
                )
                .process(77, exe, &[Fd::Fuse(connection), Fd::Writes(KEEPALIVE)]);
                assert_eq!(
                    proc.servers(4242),
                    Found {
                        servers: vec![77],
                        read_ends: vec![3]
                    },
                    "{connection:?} {exe}"
                );
            }
        }
    }

    /// Processes that are not the mount server: another FUSE connection's
    /// server that writes to the app's pipe (Linux 6.16 and later), a
    /// server with no pipe, a launcher that writes the app's pipe and holds
    /// no `/dev/fuse`, a `/dev/fuse` whose `fdinfo` is gone, the app's child
    /// that reads the pipe too, a server that reads the app's pipe instead,
    /// and another image's server that writes to its own pipe and to the
    /// app's stderr.
    #[test]
    fn a_process_without_the_connection_or_the_pipes_write_end_is_no_mount_server() {
        let proc = FakeProc::new();
        proc.process(4242, IMAGE, &app(&[]))
            .process(80, IMAGE, &[Fd::Fuse(Some(80)), Fd::Writes(KEEPALIVE)])
            .process(81, IMAGE, &[Fd::Fuse(Some(CONNECTION))])
            .process(82, "/usr/bin/launcher", &[Fd::Writes(KEEPALIVE)])
            .process(83, IMAGE, &[Fd::FuseGone, Fd::Writes(KEEPALIVE)])
            .process(84, "/usr/bin/child", &[Fd::Reads(KEEPALIVE)])
            .process(
                85,
                IMAGE,
                &[Fd::Fuse(Some(CONNECTION)), Fd::Reads(KEEPALIVE)],
            )
            .process(
                86,
                "/home/u/Other.AppImage",
                &[
                    Fd::Fuse(None),
                    Fd::Writes("pipe:[300]"),
                    Fd::Writes("pipe:[200]"),
                ],
            );
        assert_eq!(proc.servers(4242), Found::default());
    }

    /// A stale mount server, left by a crashed run of the same image, whose
    /// pipe the app inherited from its launcher: from Linux 6.16 its other
    /// connection leaves it out; before, it is found beside the app's own.
    /// Either way the app's own is found, and every read end it holds of a
    /// found server's pipe is closed on exec later.
    #[test]
    fn every_mount_server_is_found_and_a_stale_one_only_before_linux_6_16() {
        let stale = "pipe:[150]";
        for (named, servers, read_ends) in
            [(true, vec![77], vec![3]), (false, vec![60, 77], vec![3, 6])]
        {
            let connection = |connection| named.then_some(connection);
            let proc = FakeProc::new();
            proc.process(4242, IMAGE, &app(&[Fd::Reads(stale)]))
                .process(60, IMAGE, &[Fd::Fuse(connection(75)), Fd::Writes(stale)])
                .process(
                    77,
                    IMAGE,
                    &[Fd::Fuse(connection(CONNECTION)), Fd::Writes(KEEPALIVE)],
                );
            assert_eq!(
                proc.servers(4242),
                Found { servers, read_ends },
                "connection named: {named}"
            );
        }
    }

    /// An app that holds no pipe, or is not in `/proc`, has no mount server.
    #[test]
    fn an_app_without_a_pipe_has_no_mount_server() {
        let proc = FakeProc::new();
        proc.process(4242, IMAGE, &[Fd::File("/tmp/.mount_stenoAb12")])
            .process(
                77,
                IMAGE,
                &[Fd::Fuse(Some(CONNECTION)), Fd::Writes(KEEPALIVE)],
            );
        assert_eq!(proc.servers(4242), Found::default());
        assert_eq!(proc.servers(9), Found::default());
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(servers_of(empty.path(), 4242, CONNECTION), Found::default());
    }

    /// What a test's code logged, captured on its thread.
    #[derive(Clone, Default)]
    struct Logged(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Logged {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// What `f` logged at warn and above, one event per line, with the
    /// threads `f` starts through `own_scope`'s spawn.
    pub(crate) fn warnings<T>(f: impl FnOnce() -> T) -> (T, String) {
        let logged = Logged::default();
        let writer = logged.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_max_level(tracing::Level::WARN)
            .without_time()
            .with_target(false)
            .finish();
        let value = tracing::subscriber::with_default(subscriber, f);
        let text = String::from_utf8(logged.0.lock().unwrap().clone()).unwrap();
        (value, text)
    }

    /// The scan runs only for an app inside a FUSE mount, and warns when it
    /// finds no server there: not for an executable outside the mount (a
    /// launcher's `$APPDIR` inherited), a sibling mount, no `$APPDIR`, or a
    /// mount that is no FUSE mount (the image extracted and run).
    #[test]
    fn the_scan_runs_and_warns_only_for_an_app_inside_a_fuse_mount() {
        let proc = FakeProc::new();
        proc.process(4242, IMAGE, &app(&[])).process(
            77,
            IMAGE,
            &[Fd::Fuse(Some(CONNECTION)), Fd::Writes(KEEPALIVE)],
        );
        let mount = Path::new("/tmp/.mount_stenoAb12");
        let inside = mount.join("usr/bin/steno-desktop");
        let fuse = |_: &Path| Some(CONNECTION);
        let scan =
            |appdir: Option<&Path>, exe: &Path, connection_of: &dyn Fn(&Path) -> Option<u32>| {
                warnings(|| servers_at(proc.0.path(), 4242, appdir, exe, connection_of))
            };

        let (found, logged) = scan(Some(mount), &inside, &fuse);
        assert_eq!(found.servers, [77]);
        assert_eq!(logged, "");
        for (appdir, exe, connection_of) in [
            (
                Some(mount),
                Path::new("/usr/bin/steno-desktop"),
                &fuse as &dyn Fn(&Path) -> Option<u32>,
            ),
            (
                Some(mount),
                Path::new("/tmp/.mount_stenoAb123/usr/bin/steno-desktop"),
                &fuse,
            ),
            (None, inside.as_path(), &fuse),
            (Some(mount), inside.as_path(), &|_: &Path| None),
        ] {
            let (found, logged) = scan(appdir, exe, connection_of);
            assert_eq!(found, Found::default(), "{appdir:?} {}", exe.display());
            assert_eq!(logged, "", "{appdir:?} {}", exe.display());
        }

        let (found, logged) = scan(Some(mount), &inside, &|_: &Path| Some(80));
        assert_eq!(found, Found::default());
        assert!(
            logged.contains("WARN")
                && logged.contains("cannot find")
                && logged.contains("connection=80"),
            "{logged}"
        );
    }

    /// A folder is no FUSE mount; the kernel's device numbers.
    #[test]
    fn a_folder_has_no_fuse_connection() {
        let folder = tempfile::tempdir().unwrap();
        assert_eq!(fuse_connection(folder.path()), None);
        assert_eq!(fuse_connection(Path::new("/nonexistent")), None);
        assert_eq!(kernel_dev(rustix::fs::makedev(0, 79)), 79);
        assert_eq!(kernel_dev(rustix::fs::makedev(8, 1)), 8 << 20 | 1);
        assert_eq!(kernel_dev(rustix::fs::makedev(0, 0x1_2345)), 0x1_2345);
    }

    #[test]
    fn the_access_mode_and_the_connection_are_read_from_fdinfo() {
        let fdinfo = |flags: &str| format!("pos:\t0\nflags:\t{flags}\nmnt_id:\t15\nino:\t42\n");
        assert_eq!(access(&fdinfo("00")), Some(libc::O_RDONLY));
        // Close-on-exec and non-blocking do not count.
        assert_eq!(access(&fdinfo("02000000")), Some(libc::O_RDONLY));
        assert_eq!(access(&fdinfo("01")), Some(libc::O_WRONLY));
        assert_eq!(access(&fdinfo("02004001")), Some(libc::O_WRONLY));
        assert_eq!(access(&fdinfo("02")), Some(libc::O_RDWR));
        assert_eq!(access("pos:\t0\n"), None);
        assert_eq!(access("flags:\tx\n"), None);
        let fuse = "pos:\t0\nflags:\t02100002\nmnt_id:\t26\nino:\t5\nfuse_connection:\t79\n";
        assert_eq!(field(fuse, "fuse_connection"), Some("79"));
        assert_eq!(field(&fdinfo("02"), "fuse_connection"), None);
    }

    /// A read end left open across exec is closed on exec afterwards.
    #[test]
    fn the_read_ends_are_closed_on_exec() {
        let (reader, _writer) = std::io::pipe().unwrap();
        let flags = || rustix::io::fcntl_getfd(&reader).unwrap();
        rustix::io::fcntl_setfd(&reader, rustix::io::FdFlags::empty()).unwrap();
        assert!(!flags().contains(rustix::io::FdFlags::CLOEXEC));
        close_on_exec(&[reader.as_raw_fd()]);
        assert!(flags().contains(rustix::io::FdFlags::CLOEXEC));
    }
}
