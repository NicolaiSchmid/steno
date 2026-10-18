//! Linux only: the `AppImage` runtime's mount servers. A mount server is
//! any other process that holds a `/dev/fuse` descriptor for the mount's
//! FUSE connection (or any, before Linux 6.16) and the write end of a pipe
//! the app reads. `own_scope` moves each into a scope of its own, so the
//! stop of the app's unit cannot end the mount while the app saves.

use std::os::fd::RawFd;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

const FUSE_SUPER_MAGIC: u32 = 0x6573_5546;

/// The mount servers of the `AppImage` the app runs from: none outside
/// one, or when none is found, which is logged.
pub fn mount_servers() -> Vec<u32> {
    let appdir = std::env::var_os("APPDIR").map(PathBuf::from);
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    servers_at(
        Path::new("/proc"),
        std::process::id(),
        appdir.as_deref(),
        &exe,
        fuse_connection,
    )
}

/// The mount servers under `proc` of the app `pid` when its executable
/// `exe` is on the FUSE mount `appdir`; warns when none is found.
fn servers_at(
    proc: &Path,
    pid: u32,
    appdir: Option<&Path>,
    exe: &Path,
    connection_of: impl Fn(&Path) -> Option<u32>,
) -> Vec<u32> {
    let Some(mount) = appdir else {
        return Vec::new();
    };
    // Compared by FUSE connection, not path: `$APPDIR` may be inherited
    // from a launcher, or reached through a symlink.
    let Some(connection) = connection_of(mount).filter(|&c| connection_of(exe) == Some(c)) else {
        return Vec::new();
    };
    let servers = servers_of(proc, pid, connection);
    if servers.is_empty() {
        tracing::warn!(
            appdir = %mount.display(),
            connection,
            "Steno runs from an AppImage whose mount server it cannot find, so the server stays where it runs, and the session's end may stop it while Steno saves a recording"
        );
    }
    servers
}

/// The FUSE connection of the file system `path` is on, as `/dev/fuse`'s
/// `fdinfo` names it; `None` when it is no FUSE mount (an extracted image).
fn fuse_connection(path: &Path) -> Option<u32> {
    let (magic, dev) = file_system(path)?;
    (magic == FUSE_SUPER_MAGIC).then(|| kernel_dev(dev))
}

/// `statfs`'s file system type and `stat`'s device of `path`.
fn file_system(path: &Path) -> Option<(u32, u64)> {
    let stats = rustix::fs::statfs(path).ok()?;
    // The cast keeps the bits of magic numbers above `i32::MAX` where
    // `f_type` is signed.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::unnecessary_cast,
        reason = "f_type's width and sign differ between targets"
    )]
    let magic = stats.f_type as u32;
    Some((magic, std::fs::metadata(path).ok()?.dev()))
}

/// A device number in the kernel's internal encoding, as `fdinfo` prints a
/// FUSE connection.
fn kernel_dev(dev: u64) -> u32 {
    rustix::fs::major(dev) << 20 | rustix::fs::minor(dev)
}

/// The processes under `proc` other than `pid` that serve `connection` and
/// write to a pipe that `pid` reads.
fn servers_of(proc: &Path, pid: u32, connection: u32) -> Vec<u32> {
    let read = pipes(&proc.join(pid.to_string()), libc::O_RDONLY);
    if read.is_empty() {
        return Vec::new();
    }
    let others = std::fs::read_dir(proc)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|&other| other != pid);
    let mut servers = Vec::new();
    for other in others {
        let process = proc.join(other.to_string());
        if !serves(&process, connection) {
            continue;
        }
        let written = pipes(&process, libc::O_WRONLY);
        if read.iter().any(|pipe| written.contains(pipe)) {
            servers.push(other);
        }
    }
    servers.sort_unstable();
    servers
}

/// Whether the process at `process` holds `/dev/fuse` for `connection`, or
/// with no connection named (before Linux 6.16).
fn serves(process: &Path, connection: u32) -> bool {
    descriptors(process).any(|(fd, link)| {
        link == Path::new("/dev/fuse")
            && fdinfo(process, fd).is_some_and(|info| match field(&info, "fuse_connection") {
                Some(named) => named.parse() == Ok(connection),
                None => true,
            })
    })
}

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

fn fdinfo(process: &Path, fd: RawFd) -> Option<String> {
    std::fs::read_to_string(process.join("fdinfo").join(fd.to_string())).ok()
}

/// The pipes the process holds open with the access mode `mode`, as their
/// `fd` links name them.
fn pipes(process: &Path, mode: i32) -> Vec<PathBuf> {
    descriptors(process)
        .filter(|(fd, link)| {
            link.as_os_str().as_encoded_bytes().starts_with(b"pipe:")
                && fdinfo(process, *fd).is_some_and(|info| access(&info) == Some(mode))
        })
        .map(|(_, link)| link)
        .collect()
}

fn field<'a>(fdinfo: &'a str, name: &str) -> Option<&'a str> {
    fdinfo
        .lines()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
        .map(str::trim)
}

/// The access mode of an `fdinfo` file's octal `flags`.
fn access(fdinfo: &str) -> Option<i32> {
    Some(i32::from_str_radix(field(fdinfo, "flags")?, 8).ok()? & libc::O_ACCMODE)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::os::unix::fs::symlink;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// The keepalive pipe's `fd` link.
    const KEEPALIVE: &str = "pipe:[100]";
    const CONNECTION: u32 = 79;
    const NO_SERVER: [u32; 0] = [];

    /// A fake `/proc` in a temporary directory.
    struct FakeProc(tempfile::TempDir);

    #[derive(Clone, Copy)]
    enum Fd<'a> {
        /// A pipe's end, read or written.
        Reads(&'a str),
        Writes(&'a str),
        /// `/dev/fuse`, with or without the `fuse_connection` line.
        Fuse(Option<u32>),
        /// `/dev/fuse` with no `fdinfo`.
        FuseGone,
        /// A file.
        File(&'a str),
    }

    impl FakeProc {
        fn new() -> Self {
            let proc = Self(tempfile::tempdir().unwrap());
            std::fs::create_dir(proc.0.path().join("sys")).unwrap();
            symlink("4242", proc.0.path().join("self")).unwrap();
            proc
        }

        /// Adds process `pid` with `exe` and descriptors from 3.
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

        fn servers(&self, pid: u32) -> Vec<u32> {
            servers_of(self.0.path(), pid, CONNECTION)
        }
    }

    /// The fake app 4242's descriptors: the keepalive pipe, stderr, the
    /// mount's folder.
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

    /// The app's own mount server is found whatever its executable.
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
                assert_eq!(proc.servers(4242), [77], "{connection:?} {exe}");
            }
        }
    }

    /// Processes that hold only one of the connection and the pipe's write
    /// end, or the wrong end, are not mount servers.
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
        assert_eq!(proc.servers(4242), NO_SERVER);
    }

    /// A stale server of the same image is left out from Linux 6.16 and
    /// found beside the app's own before.
    #[test]
    fn every_mount_server_is_found_and_a_stale_one_only_before_linux_6_16() {
        let stale = "pipe:[150]";
        for (named, servers) in [(true, vec![77]), (false, vec![60, 77])] {
            let connection = |connection| named.then_some(connection);
            let proc = FakeProc::new();
            proc.process(4242, IMAGE, &app(&[Fd::Reads(stale)]))
                .process(60, IMAGE, &[Fd::Fuse(connection(75)), Fd::Writes(stale)])
                .process(
                    77,
                    IMAGE,
                    &[Fd::Fuse(connection(CONNECTION)), Fd::Writes(KEEPALIVE)],
                );
            assert_eq!(proc.servers(4242), servers, "connection named: {named}");
        }
    }

    #[test]
    fn an_app_without_a_pipe_has_no_mount_server() {
        let proc = FakeProc::new();
        proc.process(4242, IMAGE, &[Fd::File("/tmp/.mount_stenoAb12")])
            .process(
                77,
                IMAGE,
                &[Fd::Fuse(Some(CONNECTION)), Fd::Writes(KEEPALIVE)],
            );
        assert_eq!(proc.servers(4242), NO_SERVER);
        assert_eq!(proc.servers(9), NO_SERVER);
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(servers_of(empty.path(), 4242, CONNECTION), NO_SERVER);
    }

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

    /// What `f` logged at warn and above, including on threads `f` starts
    /// through `own_scope`'s spawn.
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

    /// The scan runs only when the executable is on the FUSE mount
    /// `$APPDIR` names, and warns when it finds no server.
    #[test]
    fn the_scan_runs_and_warns_only_for_an_app_on_the_fuse_mount() {
        let proc = FakeProc::new();
        proc.process(4242, IMAGE, &app(&[])).process(
            77,
            IMAGE,
            &[Fd::Fuse(Some(CONNECTION)), Fd::Writes(KEEPALIVE)],
        );
        let mount = Path::new("/tmp/.mount_stenoAb12");
        let through_symlink = Path::new("/run/user/1000/tmp/.mount_stenoAb12");
        let other = Path::new("/tmp/.mount_otherCd34");
        let inside = mount.join("usr/bin/steno-desktop");
        let on_other = other.join("usr/bin/steno-desktop");
        let fuse: &dyn Fn(&Path) -> Option<u32> = &|path| {
            if path.starts_with(mount) || path.starts_with(through_symlink) {
                Some(CONNECTION)
            } else if path.starts_with(other) {
                Some(80)
            } else {
                None
            }
        };
        let scan =
            |appdir: Option<&Path>, exe: &Path, connection_of: &dyn Fn(&Path) -> Option<u32>| {
                warnings(|| servers_at(proc.0.path(), 4242, appdir, exe, connection_of))
            };

        for appdir in [mount, through_symlink] {
            let (found, logged) = scan(Some(appdir), &inside, fuse);
            assert_eq!(found, [77], "{}", appdir.display());
            assert_eq!(logged, "", "{}", appdir.display());
        }
        for (appdir, exe, connection_of) in [
            (Some(mount), Path::new("/usr/bin/steno-desktop"), fuse),
            (Some(mount), on_other.as_path(), fuse),
            (None, inside.as_path(), fuse),
            (Some(mount), inside.as_path(), &|_: &Path| None),
        ] {
            let (found, logged) = scan(appdir, exe, connection_of);
            assert_eq!(found, NO_SERVER, "{appdir:?} {}", exe.display());
            assert_eq!(logged, "", "{appdir:?} {}", exe.display());
        }

        let (found, logged) = scan(Some(mount), &inside, &|_: &Path| Some(80));
        assert_eq!(found, NO_SERVER);
        assert!(
            logged.contains("WARN")
                && logged.contains("cannot find")
                && logged.contains("connection=80"),
            "{logged}"
        );
    }

    /// File system and device-number helpers.
    #[test]
    fn a_folder_has_no_fuse_connection() {
        let folder = tempfile::tempdir().unwrap();
        assert_eq!(fuse_connection(folder.path()), None);
        assert_eq!(fuse_connection(Path::new("/nonexistent")), None);
        assert_eq!(file_system(Path::new("/nonexistent")), None);
        assert_eq!(u32::try_from(libc::FUSE_SUPER_MAGIC), Ok(FUSE_SUPER_MAGIC));
        let proc = Path::new("/proc");
        assert_eq!(
            file_system(proc),
            Some((
                u32::try_from(libc::PROC_SUPER_MAGIC).unwrap(),
                std::fs::metadata(proc).unwrap().dev()
            ))
        );
        let (_, dev) = file_system(folder.path()).unwrap();
        assert_eq!(dev, std::fs::metadata(folder.path()).unwrap().dev());
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
}
