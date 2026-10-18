//! The Windows install's watcher: one `cmd.exe` the app leaves running
//! that starts the installer, waits for it, and starts the running version
//! again when it did not install, so a declined consent prompt or a failed
//! setup never leaves Steno down. Paths reach `cmd.exe` only in quoted
//! variables, so no character in a path is read as syntax.

use std::ffi::OsString;
use std::path::Path;
#[cfg(windows)]
use std::process::Command;
use std::time::Duration;

pub const SETUP_VARIABLE: &str = "STENO_UPDATE_SETUP";
/// The variable that holds the binary the watcher starts when the
/// installer did not install.
pub const RELAUNCH_VARIABLE: &str = "STENO_UPDATE_RELAUNCH";
/// The variable that holds the file the watcher creates first, to say that
/// it runs ([`wait_for_start`]).
pub const STARTED_VARIABLE: &str = "STENO_UPDATE_STARTED";
pub const STARTED_FILE: &str = "watcher-started";
pub const WATCHER_START_LIMIT: Duration = Duration::from_secs(10);

/// The first bytes of a compound file, which an MSI is.
const COMPOUND_FILE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Starts a process outside the app's job, so it outlives the app.
#[cfg(windows)]
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setup {
    /// The MSI, which asks for consent and installs for every user.
    Msi,
    /// The NSIS setup, which installs for the user alone.
    Nsis,
}

impl Setup {
    /// The installer `package` holds, from its first bytes; `None` for
    /// anything else.
    pub fn of(package: &[u8]) -> Option<Setup> {
        if package.starts_with(&COMPOUND_FILE) {
            Some(Setup::Msi)
        } else if package.starts_with(b"MZ") {
            Some(Setup::Nsis)
        } else {
            None
        }
    }

    pub fn file_name(self, version: &str) -> String {
        match self {
            Setup::Msi => format!("Steno-{version}.msi"),
            Setup::Nsis => format!("Steno-{version}-setup.exe"),
        }
    }

    /// The exit codes that mean the installer installed and started the
    /// new version itself; any other starts the version that ran again.
    pub fn installed(self) -> &'static [u32] {
        match self {
            Setup::Msi => &[0, 1641, 3010],
            Setup::Nsis => &[0],
        }
    }
}

/// The installer's command line, as `tauri-plugin-updater`'s passive
/// install with restart builds it.
pub fn setup_command(setup: Setup, installer: &Path, msiexec: &Path) -> OsString {
    let quoted = |path: &Path| {
        let mut quoted = OsString::from("\"");
        quoted.push(path);
        quoted.push("\"");
        quoted
    };
    let mut command = OsString::new();
    if setup == Setup::Msi {
        command.push(quoted(msiexec));
        command.push(" /i ");
    }
    command.push(quoted(installer));
    command.push(match setup {
        Setup::Msi => " /passive /promptrestart AUTOLAUNCHAPP=True",
        Setup::Nsis => " /P /UPDATE /R",
    });
    command
}

/// The watcher's arguments to `cmd.exe`: create the start file (or end),
/// run the installer and wait; end on one of [`Setup::installed`], else
/// clear the variables and start [`RELAUNCH_VARIABLE`].
pub fn watcher_arguments(setup: Setup) -> String {
    use std::fmt::Write as _;
    let mut line = format!(
        "type nul > \"%{STARTED_VARIABLE}%\" || exit 1 & start \"\" /wait %{SETUP_VARIABLE}%"
    );
    for code in setup.installed() {
        let above = code + 1;
        let _ = write!(
            line,
            " & (if errorlevel {code} if not errorlevel {above} exit)"
        );
    }
    for variable in [SETUP_VARIABLE, RELAUNCH_VARIABLE, STARTED_VARIABLE] {
        let _ = write!(line, " & set \"{variable}=\"");
    }
    let _ = write!(line, " & start \"\" \"%{RELAUNCH_VARIABLE}%\"");
    format!("/d /e:on /v:off /s /c \"{line}\"")
}

#[cfg(windows)]
pub fn system32() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()))
        .join("System32")
}

/// The watcher for `installer`: `cmd.exe` with [`watcher_arguments`], the
/// paths in their variables, no window or streams, out of the app's job,
/// in `folder`.
#[cfg(windows)]
pub fn watcher_command(
    setup: Setup,
    installer: &Path,
    msiexec: &Path,
    relaunch: &Path,
    started: &Path,
    folder: &Path,
) -> Command {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;

    let mut watcher = Command::new(system32().join("cmd.exe"));
    watcher
        .raw_arg(watcher_arguments(setup))
        .env(SETUP_VARIABLE, setup_command(setup, installer, msiexec))
        .env(RELAUNCH_VARIABLE, relaunch)
        .env(STARTED_VARIABLE, started)
        .current_dir(folder)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB);
    watcher
}

/// Empties the update's `folder` before a new installer is written. A
/// start file left behind would pass [`wait_for_start`] at once, so one
/// that cannot be removed fails the install.
pub fn empty_update_folder(folder: &Path) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(folder);
    if folder.join(STARTED_FILE).exists() {
        return Err(format!(
            "Steno could not empty its update folder ({}).",
            folder.display()
        ));
    }
    std::fs::create_dir_all(folder).map_err(|error| error.to_string())
}

/// Waits up to `limit` for `watcher` to create `started`. A watcher that
/// exits first fails the wait, and one silent at the limit is ended; either
/// way it has started no installer, so the app can start again.
pub fn wait_for_start(
    watcher: &mut std::process::Child,
    started: &Path,
    limit: Duration,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + limit;
    let error = loop {
        if started.exists() {
            return Ok(());
        }
        match watcher.try_wait() {
            Ok(Some(_)) if started.exists() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!(
                    "The installer's watcher ended before it ran ({status})."
                ));
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                break format!(
                    "The installer's watcher did not run within {} seconds, and was ended.",
                    limit.as_secs()
                );
            }
            Err(error) => break error.to_string(),
        }
    };
    let _ = watcher.kill();
    let _ = watcher.wait();
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_package_says_which_installer_it_is() {
        let mut msi = COMPOUND_FILE.to_vec();
        msi.extend_from_slice(&[0; 504]);
        assert_eq!(Setup::of(&msi), Some(Setup::Msi));
        assert_eq!(Setup::of(b"MZ\x90\x00\x03"), Some(Setup::Nsis));
        for other in [&b"PK\x03\x04"[..], b"", b"M", &COMPOUND_FILE[..7]] {
            assert_eq!(Setup::of(other), None, "{other:?}");
        }
        assert_eq!(Setup::Msi.file_name("0.12.0"), "Steno-0.12.0.msi");
        assert_eq!(Setup::Nsis.file_name("0.12.0"), "Steno-0.12.0-setup.exe");
    }

    /// Paths with spaces stay one quoted argument each.
    #[test]
    fn the_installer_runs_as_the_plugin_ran_it() {
        let msiexec = Path::new(r"C:\Windows\System32\msiexec.exe");
        let msi = Path::new(
            r"C:\Users\Ada Lovelace\AppData\Local\uno.schmid.steno.desktop\update\Steno-0.12.0.msi",
        );
        assert_eq!(
            setup_command(Setup::Msi, msi, msiexec),
            OsString::from(
                r#""C:\Windows\System32\msiexec.exe" /i "C:\Users\Ada Lovelace\AppData\Local\uno.schmid.steno.desktop\update\Steno-0.12.0.msi" /passive /promptrestart AUTOLAUNCHAPP=True"#
            )
        );
        let setup = Path::new(r"C:\Users\Ada Lovelace\update\Steno-0.12.0-setup.exe");
        assert_eq!(
            setup_command(Setup::Nsis, setup, msiexec),
            OsString::from(
                r#""C:\Users\Ada Lovelace\update\Steno-0.12.0-setup.exe" /P /UPDATE /R"#
            )
        );
    }

    /// The watcher's whole command line.
    #[test]
    fn the_watcher_relaunches_unless_the_installer_installed() {
        let start = concat!(
            r#"/d /e:on /v:off /s /c "type nul > "%STENO_UPDATE_STARTED%" || exit 1"#,
            r#" & start "" /wait %STENO_UPDATE_SETUP%"#,
        );
        let relaunch = concat!(
            r#" & set "STENO_UPDATE_SETUP=" & set "STENO_UPDATE_RELAUNCH=""#,
            r#" & set "STENO_UPDATE_STARTED=" & start "" "%STENO_UPDATE_RELAUNCH%"""#,
        );
        assert_eq!(
            watcher_arguments(Setup::Msi),
            [
                start,
                " & (if errorlevel 0 if not errorlevel 1 exit)",
                " & (if errorlevel 1641 if not errorlevel 1642 exit)",
                " & (if errorlevel 3010 if not errorlevel 3011 exit)",
                relaunch,
            ]
            .concat()
        );
        assert_eq!(
            watcher_arguments(Setup::Nsis),
            [
                start,
                " & (if errorlevel 0 if not errorlevel 1 exit)",
                relaunch,
            ]
            .concat()
        );
    }

    /// Every exit code but the installer's successes relaunches.
    #[test]
    fn a_declined_or_failed_install_starts_the_old_version() {
        let relaunches = |setup: Setup, code: u32| !setup.installed().contains(&code);
        for code in [0, 1641, 3010] {
            assert!(!relaunches(Setup::Msi, code), "{code}");
        }
        for code in [1, 1223, 1602, 1603, 1618, 1625, 1642, 3011] {
            assert!(relaunches(Setup::Msi, code), "{code}");
        }
        assert!(!relaunches(Setup::Nsis, 0));
        for code in [1, 2, 1641, 3010] {
            assert!(relaunches(Setup::Nsis, code), "{code}");
        }
    }

    #[test]
    fn no_path_is_in_the_command_line() {
        for setup in [Setup::Msi, Setup::Nsis] {
            let arguments = watcher_arguments(setup);
            assert!(!arguments.contains(":\\"), "{arguments}");
            assert_eq!(arguments.matches('"').count() % 2, 0, "{arguments}");
        }
    }

    #[test]
    fn the_app_waits_ten_seconds_for_the_watcher() {
        assert_eq!(WATCHER_START_LIMIT, Duration::from_secs(10));
    }

    /// A limit only a hang reaches, for tests that check why the wait
    /// ended rather than how fast.
    const HANG_GUARD: Duration = Duration::from_secs(120);

    /// A watcher under `sh` running `script` with its start file as `$1`.
    #[cfg(unix)]
    fn sh(script: &str) -> (tempfile::TempDir, std::path::PathBuf, Watcher) {
        let folder = tempfile::tempdir().unwrap();
        let started = folder.path().join(STARTED_FILE);
        let watcher = std::process::Command::new("sh")
            .args(["-c", script, "sh"])
            .arg(&started)
            .spawn()
            .expect("sh starts");
        (folder, started, Watcher(watcher))
    }

    /// A test's watcher, ended and reaped when the test ends.
    #[cfg(unix)]
    struct Watcher(std::process::Child);

    #[cfg(unix)]
    impl Drop for Watcher {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// The wait ends on the start file, while the watcher still runs.
    #[cfg(unix)]
    #[test]
    fn a_watcher_that_says_it_runs_ends_the_wait() {
        let (_folder, started, mut watcher) = sh(r#"touch "$1"; exec sleep 120"#);
        assert_eq!(wait_for_start(&mut watcher.0, &started, HANG_GUARD), Ok(()));
        assert!(started.exists(), "ended on the file");
        assert!(watcher.0.try_wait().unwrap().is_none(), "left running");
    }

    /// A watcher that ends without the file fails the wait at its exit.
    #[cfg(unix)]
    #[test]
    fn a_watcher_that_ends_without_saying_so_fails_the_wait() {
        let (_folder, started, mut watcher) = sh("exit 3");
        let error = wait_for_start(&mut watcher.0, &started, HANG_GUARD).unwrap_err();
        assert!(error.contains("ended before it ran"), "{error}");
    }

    /// A watcher still silent at the limit is ended, and the wait fails.
    #[cfg(unix)]
    #[test]
    fn a_silent_watcher_is_ended_at_the_limit() {
        let (_folder, started, mut watcher) = sh("exec sleep 120");
        let limit = Duration::from_millis(300);
        let begun = std::time::Instant::now();
        let error = wait_for_start(&mut watcher.0, &started, limit).unwrap_err();
        let waited = begun.elapsed();
        assert!(error.contains("did not run"), "{error}");
        assert!(waited >= limit && waited < HANG_GUARD, "{waited:?}");
        assert!(watcher.0.try_wait().unwrap().is_some(), "ended");
        assert!(!started.exists());
    }

    #[test]
    fn an_earlier_update_is_emptied_out() {
        let data = tempfile::tempdir().unwrap();
        let folder = data.path().join("update");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("Steno-0.11.0.msi"), b"old").unwrap();
        std::fs::write(folder.join(STARTED_FILE), b"").unwrap();
        assert_eq!(empty_update_folder(&folder), Ok(()));
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
        let fresh = data.path().join("fresh");
        assert_eq!(empty_update_folder(&fresh), Ok(()));
        assert!(fresh.is_dir());
    }

    /// A start file that cannot be removed fails the install.
    #[cfg(unix)]
    #[test]
    fn a_start_file_left_behind_fails_the_install() {
        use std::os::unix::fs::PermissionsExt;
        let data = tempfile::tempdir().unwrap();
        let folder = data.path().join("update");
        std::fs::create_dir_all(&folder).unwrap();
        let started = folder.join(STARTED_FILE);
        std::fs::write(&started, b"").unwrap();
        let mode = |mode| std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(&folder, mode(0o555)).unwrap();
        let result = empty_update_folder(&folder);
        std::fs::set_permissions(&folder, mode(0o755)).unwrap();
        if started.exists() {
            assert_eq!(
                result,
                Err(format!(
                    "Steno could not empty its update folder ({}).",
                    folder.display()
                ))
            );
        } else {
            assert_eq!(result, Ok(()));
        }
    }

    /// A stub installer or app: writes its directory, `STENO_UPDATE_*`
    /// variables and arguments into `<name>.ran` and exits with
    /// `STENO_TEST_EXIT_CODE`.
    #[cfg(windows)]
    const STUB: &str = r#"
#![windows_subsystem = "windows"]
fn main() {
    let exe = std::env::current_exe().unwrap();
    let mut variables: Vec<String> = std::env::vars()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("STENO_UPDATE_"))
        .collect();
    variables.sort();
    let mut record = vec![
        std::env::current_dir().unwrap().display().to_string(),
        variables.join(","),
    ];
    record.extend(std::env::args().skip(1));
    std::fs::write(exe.with_extension("ran"), record.join("\n")).unwrap();
    let code = std::env::var("STENO_TEST_EXIT_CODE").ok().and_then(|code| code.parse().ok());
    std::process::exit(code.unwrap_or(0));
}
"#;

    /// [`STUB`], compiled into `folder`.
    #[cfg(windows)]
    fn stub(folder: &Path) -> std::path::PathBuf {
        let (source, exe) = (folder.join("stub.rs"), folder.join("stub.exe"));
        std::fs::write(&source, STUB).unwrap();
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let status = std::process::Command::new(rustc)
            .arg("--edition=2021")
            .arg(&source)
            .arg("-o")
            .arg(&exe)
            .status()
            .expect("rustc runs");
        assert!(status.success(), "the stub builds");
        exe
    }

    /// What a stub wrote; `None` while it has not run.
    #[cfg(windows)]
    fn ran(stub: &Path) -> Option<(std::path::PathBuf, String, Vec<String>)> {
        let record = std::fs::read_to_string(stub.with_extension("ran")).ok()?;
        let mut lines = record.split('\n').map(str::to_owned);
        let directory = std::path::PathBuf::from(lines.next()?);
        let variables = lines.next()?;
        Some((directory, variables, lines.collect()))
    }

    /// Spawns `watcher`, only hidden where cargo's job forbids breakaway.
    #[cfg(windows)]
    fn spawn(mut watcher: Command) -> std::process::Child {
        use std::os::windows::process::CommandExt;
        match watcher.spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("breakaway refused here ({error}); the watcher runs in the job");
                watcher.creation_flags(CREATE_NO_WINDOW).spawn().unwrap()
            }
            Err(error) => panic!("the watcher starts: {error}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_watcher_command_puts_each_path_in_its_variable() {
        use std::ffi::OsStr;
        let at = |path: &str| Path::new(path).to_owned();
        let (installer, msiexec, relaunch, started, folder) = (
            at(r"C:\Data\update\Steno-0.12.0.msi"),
            at(r"C:\Windows\System32\msiexec.exe"),
            at(r"C:\Program Files\Steno\steno-desktop.exe"),
            at(r"C:\Data\update\watcher-started"),
            at(r"C:\Data"),
        );
        let watcher = watcher_command(
            Setup::Msi,
            &installer,
            &msiexec,
            &relaunch,
            &started,
            &folder,
        );
        assert_eq!(watcher.get_program(), system32().join("cmd.exe"));
        let arguments: Vec<_> = watcher.get_args().collect();
        assert_eq!(arguments, [OsStr::new(&watcher_arguments(Setup::Msi))]);
        assert_eq!(watcher.get_current_dir(), Some(folder.as_path()));
        let mut variables: Vec<_> = watcher.get_envs().collect();
        variables.sort();
        let setup = setup_command(Setup::Msi, &installer, &msiexec);
        assert_eq!(
            variables,
            [
                (OsStr::new(RELAUNCH_VARIABLE), Some(relaunch.as_os_str())),
                (OsStr::new(SETUP_VARIABLE), Some(setup.as_os_str())),
                (OsStr::new(STARTED_VARIABLE), Some(started.as_os_str())),
            ]
        );
    }

    /// The real `cmd.exe` in a folder named with every syntax character:
    /// it starts the installer, and relaunches the app unless it installed.
    #[cfg(windows)]
    #[test]
    fn the_real_watcher_relaunches_unless_the_installer_installed() {
        let root = tempfile::tempdir().unwrap();
        let odd = root.path().join("Ana (Work) 50%PATH%off ^&! x!");
        std::fs::create_dir_all(&odd).unwrap();
        let stub = stub(root.path());
        let [nsis, msiexec, app] =
            ["Steno-0.12.0-setup.exe", "msiexec.exe", "steno.exe"].map(|name| odd.join(name));
        for copy in [&nsis, &msiexec, &app] {
            std::fs::copy(&stub, copy).unwrap();
        }
        let msi = odd.join("Steno-0.12.0.msi");
        let same = |a: &Path, b: &Path| {
            std::fs::canonicalize(a).unwrap() == std::fs::canonicalize(b).unwrap()
        };
        let cases = [
            (Setup::Msi, 0, false),
            (Setup::Msi, 1641, false),
            (Setup::Msi, 3010, false),
            (Setup::Msi, 1602, true),
            (Setup::Msi, 1603, true),
            (Setup::Msi, 1, true),
            (Setup::Msi, -1, true),
            (Setup::Nsis, 0, false),
            (Setup::Nsis, 2, true),
        ];
        for (setup, code, relaunches) in cases {
            let case = format!("{setup:?} {code}");
            for stub in [&nsis, &msiexec, &app] {
                let _ = std::fs::remove_file(stub.with_extension("ran"));
            }
            let update = odd.join("update");
            let _ = std::fs::remove_dir_all(&update);
            std::fs::create_dir_all(&update).unwrap();
            let started = update.join(STARTED_FILE);
            let (installer, runs) = match setup {
                Setup::Msi => (&msi, &msiexec),
                Setup::Nsis => (&nsis, &nsis),
            };
            let mut watcher = watcher_command(setup, installer, &msiexec, &app, &started, &odd);
            watcher.env("STENO_TEST_EXIT_CODE", code.to_string());
            let mut watcher = spawn(watcher);
            assert_eq!(
                wait_for_start(&mut watcher, &started, HANG_GUARD),
                Ok(()),
                "{case}"
            );
            watcher.wait().unwrap();

            let (directory, _, arguments) = ran(runs).expect(&case);
            assert!(same(&directory, &odd), "{case}: {directory:?}");
            let expected: Vec<String> = match setup {
                Setup::Msi => vec![
                    "/i".into(),
                    msi.display().to_string(),
                    "/passive".into(),
                    "/promptrestart".into(),
                    "AUTOLAUNCHAPP=True".into(),
                ],
                Setup::Nsis => ["/P", "/UPDATE", "/R"].map(String::from).to_vec(),
            };
            assert_eq!(arguments, expected, "{case}");

            if relaunches {
                let deadline = std::time::Instant::now() + HANG_GUARD;
                while ran(&app).is_none() {
                    assert!(std::time::Instant::now() < deadline, "{case}: no relaunch");
                    std::thread::sleep(Duration::from_millis(50));
                }
                let (directory, variables, arguments) = ran(&app).unwrap();
                assert!(same(&directory, &odd), "{case}: {directory:?}");
                assert_eq!(variables, "", "{case}");
                assert_eq!(arguments, Vec::<String>::new(), "{case}");
            } else {
                // The watcher ended at `exit`, before the relaunch's start.
                std::thread::sleep(Duration::from_secs(1));
                assert!(ran(&app).is_none(), "{case}: relaunched");
            }
        }

        let started = odd.join("missing").join(STARTED_FILE);
        let mut watcher = watcher_command(Setup::Nsis, &nsis, &msiexec, &app, &started, &odd);
        watcher.env("STENO_TEST_EXIT_CODE", "2");
        for stub in [&nsis, &app] {
            let _ = std::fs::remove_file(stub.with_extension("ran"));
        }
        let error = wait_for_start(&mut spawn(watcher), &started, HANG_GUARD).unwrap_err();
        assert!(error.contains("ended before it ran"), "{error}");
        std::thread::sleep(Duration::from_secs(1));
        assert!(ran(&nsis).is_none() && ran(&app).is_none(), "nothing ran");
    }
}
