//! The Windows install's command lines (`write_update` in `updater.rs`).
//! The app runs no installer itself: a watcher, one `cmd.exe` the app
//! leaves running when it ends, starts the installer as
//! `tauri-plugin-updater`'s passive install does, waits for it, and starts
//! the version that ran again when the installer did not install, so a
//! declined consent prompt or a failed setup never leaves Steno down. On
//! success the installer starts the new version itself (the MSI's
//! `AUTOLAUNCHAPP`, the setup's `/R`). Neither relaunch passes the app's
//! arguments on: a deep link the app was started with has been handled.
//!
//! The paths reach `cmd.exe` in variables ([`SETUP_VARIABLE`],
//! [`RELAUNCH_VARIABLE`]), so its command line is the same for every user
//! and a `%`, `&` or `^` in a path is never read as `cmd.exe` syntax: the
//! value of a variable is not expanded again, and every path in it is in
//! quotes, which no Windows path contains.

use std::ffi::OsString;
use std::path::Path;

/// The variable that holds the installer's command line ([`setup_command`]).
pub const SETUP_VARIABLE: &str = "STENO_UPDATE_SETUP";
/// The variable that holds the path of the binary that ran the install,
/// which the watcher starts when the installer did not install.
pub const RELAUNCH_VARIABLE: &str = "STENO_UPDATE_RELAUNCH";

/// The installer a Windows update holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setup {
    /// The MSI, run by `msiexec`, which asks for consent and installs for
    /// every user.
    Msi,
    /// The NSIS setup, which installs for the user alone.
    Nsis,
}

impl Setup {
    /// The installer `package` holds, from its first bytes: an MSI is a
    /// compound file, the setup an executable. `None` for anything else,
    /// such as a zip, which this app's releases do not publish.
    pub fn of(package: &[u8]) -> Option<Setup> {
        const COMPOUND_FILE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
        if package.starts_with(&COMPOUND_FILE) {
            Some(Setup::Msi)
        } else if package.starts_with(b"MZ") {
            Some(Setup::Nsis)
        } else {
            None
        }
    }

    /// The name the installer of `version` is written under.
    pub fn file_name(self, version: &str) -> String {
        match self {
            Setup::Msi => format!("Steno-{version}.msi"),
            Setup::Nsis => format!("Steno-{version}-setup.exe"),
        }
    }

    /// The exit codes that mean the installer installed, and so started the
    /// new version: `msiexec`'s success, and its success that needs a
    /// restart (3010) or has started one (1641); the setup's 0. Anything
    /// else, a declined consent prompt (1602 or 1223), another install
    /// under way (1618), a failure (1603) or an aborted setup, starts the
    /// version that ran again.
    pub fn installed(self) -> &'static [u32] {
        match self {
            Setup::Msi => &[0, 1641, 3010],
            Setup::Nsis => &[0],
        }
    }
}

/// The installer's command line for `installer`, the written package, as
/// `tauri-plugin-updater` 2.13's passive install with its restart builds
/// it: `msiexec` (at `msiexec`) with `/passive /promptrestart
/// AUTOLAUNCHAPP=True`, or the setup with `/P /UPDATE /R`.
pub fn setup_command(setup: Setup, installer: &Path, msiexec: &Path) -> OsString {
    let mut command = OsString::new();
    let quoted = |command: &mut OsString, path: &Path| {
        command.push("\"");
        command.push(path);
        command.push("\"");
    };
    match setup {
        Setup::Msi => {
            quoted(&mut command, msiexec);
            command.push(" /i ");
            quoted(&mut command, installer);
            command.push(" /passive /promptrestart AUTOLAUNCHAPP=True");
        }
        Setup::Nsis => {
            quoted(&mut command, installer);
            command.push(" /P /UPDATE /R");
        }
    }
    command
}

/// The watcher's arguments to `cmd.exe`, as they stand on its command line
/// (`/s` strips the outer quotes): start the installer in
/// [`SETUP_VARIABLE`] and wait for it; end when its exit code is one of
/// [`Setup::installed`] (`errorlevel N` holds for N and above); otherwise
/// start [`RELAUNCH_VARIABLE`].
pub fn watcher_arguments(setup: Setup) -> String {
    use std::fmt::Write as _;
    let mut line = format!("start \"\" /wait %{SETUP_VARIABLE}%");
    for code in setup.installed() {
        let above = code + 1;
        let _ = write!(
            line,
            " & (if errorlevel {code} if not errorlevel {above} exit)"
        );
    }
    let _ = write!(line, " & start \"\" \"%{RELAUNCH_VARIABLE}%\"");
    format!("/d /s /c \"{line}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPOUND_FILE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

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

    /// Paths with spaces stay one argument each, in quotes; the arguments
    /// are the plugin's for a passive install that starts the new version.
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

    /// The watcher's line, whole: it waits for the installer, ends on each
    /// exit code that installed, and otherwise starts the version that ran.
    #[test]
    fn the_watcher_relaunches_unless_the_installer_installed() {
        assert_eq!(
            watcher_arguments(Setup::Msi),
            concat!(
                r#"/d /s /c "start "" /wait %STENO_UPDATE_SETUP%"#,
                " & (if errorlevel 0 if not errorlevel 1 exit)",
                " & (if errorlevel 1641 if not errorlevel 1642 exit)",
                " & (if errorlevel 3010 if not errorlevel 3011 exit)",
                r#" & start "" "%STENO_UPDATE_RELAUNCH%"""#,
            )
        );
        assert_eq!(
            watcher_arguments(Setup::Nsis),
            concat!(
                r#"/d /s /c "start "" /wait %STENO_UPDATE_SETUP%"#,
                " & (if errorlevel 0 if not errorlevel 1 exit)",
                r#" & start "" "%STENO_UPDATE_RELAUNCH%"""#,
            )
        );
    }

    /// Which exit codes start the version that ran again: every one but
    /// the installer's successes.
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

    /// No path reaches the command line, so none can break its quoting.
    #[test]
    fn no_path_is_in_the_command_line() {
        for setup in [Setup::Msi, Setup::Nsis] {
            let arguments = watcher_arguments(setup);
            assert!(!arguments.contains(':'), "{arguments}");
            assert_eq!(arguments.matches('"').count() % 2, 0, "{arguments}");
        }
    }
}
