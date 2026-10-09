//! Linux: the systemd drop-ins that give Steno the time its save needs
//! when the session stops the unit Steno runs in. systemd sends SIGTERM,
//! and `SIGKILL` once the stop has waited out the unit's `TimeoutStopSec`:
//! 5 s in both units below, while the save the app runs on SIGTERM may
//! take up to `SHUTDOWN_PATIENCE` (ten seconds) and the process ends
//! `EXIT_GRACE` (two) after it at the latest. Each drop-in raises the
//! timeout to 20 s (`DropIn`):
//!
//! - **The autostart unit** (`DropIn::AUTOSTART`): a desktop that runs XDG
//!   autostart through systemd (KDE Plasma, uwsm sessions such as
//!   Omarchy's) runs Steno's entry in `~/.config/autostart` as the unit
//!   `systemd-xdg-autostart-generator` makes from it, with
//!   `TimeoutStopSec=5s`. Its drop-in follows Launch at login.
//! - **GNOME's scope** (`DropIn::GNOME_SCOPE`): gnome-session starts
//!   autostart entries, and gnome-shell apps from the dash and the app
//!   grid, in a scope of their own, `app-gnome-<id>-<pid>.scope`, which
//!   gnome-session's `app-gnome-.scope.d/override.conf` gives
//!   `TimeoutStopSec=5s`. Its drop-in is installed at every launch,
//!   whatever Launch at login says, and never removed.
//!
//! How they get in place:
//!
//! - **The `.deb`** installs both under `/usr/lib/systemd/user` (`files`
//!   in `tauri.conf.json`). Its `postinst` reloads the user managers, so
//!   an app already running when the package is upgraded gets the 20 s,
//!   and skips a user whose entry is gone unless their autostart unit is
//!   known to be stopped (`linux/deb-postinst.sh`).
//! - **The user's copies**: the app writes the same files into the user's
//!   own unit directory (`sync`, at each launch and at each switch of
//!   Launch at login), for the `AppImage` and installs from before the
//!   drop-ins. A write leaves a reload owed (`RELOAD_OWED`) until a reload
//!   goes through, so one that failed, was skipped or was cut off by a
//!   kill is made up for at the next `sync`. With nothing written and
//!   nothing owed there is no reload: each one reruns every generator of
//!   the user manager.
//! - **Managed** (`STENO_LOGIN_ITEM=managed`): the switch of Launch at
//!   login writes nothing, though it shows on. The autostart unit's
//!   drop-in is written only while the app runs as that unit and its
//!   entry stands (one an earlier build wrote, which goes at the exit, or
//!   the user's own), and the exit that removes that entry removes the
//!   drop-in with it (`autostart::at_exit`).
//! - **The reload rule** (`may_reload`): as the autostart unit
//!   (`runs_as_autostart_unit`) the app reloads only while the unit's
//!   entry stands. Once the entry is gone, a reload unloads the unit, and
//!   the session's end then stops the app without a SIGTERM. For the same
//!   reason Launch at login, turned off while the app runs as that unit,
//!   goes off at the exit (`autostart::set_enabled`).
//!
//! Rust only: the Swift app is a macOS login item.

use std::io;
use std::path::{Path, PathBuf};

use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;

/// One drop-in: the unit it is for, its file name in the unit's `.d`
/// directory, and its contents, byte for byte the file the `.deb`
/// installs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DropIn {
    /// The unit's name, or for GNOME's scopes the name systemd also reads
    /// drop-ins under for every `app-gnome-steno\x2ddesktop-<pid>.scope`:
    /// the name cut after its last `-`, plus `.scope`.
    unit: &'static str,
    file_name: &'static str,
    contents: &'static str,
}

impl DropIn {
    /// For the unit the generator makes from the plugin's entry,
    /// `steno-desktop.desktop` (the Linux product name in
    /// `tauri.linux.conf.json`): `app-<name>@autostart.service`, the name
    /// escaped as systemd escapes a unit name, `-` as `\x2d`. Named
    /// `10-` so a drop-in of the user's own (`systemctl --user edit`,
    /// `override.conf`) still wins.
    const AUTOSTART: Self = Self {
        unit: "app-steno\\x2ddesktop@autostart.service",
        file_name: "10-steno.conf",
        contents: include_str!("../linux/autostart-service-stop-timeout.conf"),
    };

    /// For every `app-gnome-steno\x2ddesktop-<pid>.scope`, GNOME's scope
    /// for the app id `steno-desktop`. Drop-ins apply in file name order
    /// across the directories, so the name sorts after gnome-session's
    /// `override.conf`; naming it `override.conf` would replace that file
    /// and drop its `PartOf=graphical-session.target`.
    const GNOME_SCOPE: Self = Self {
        unit: "app-gnome-steno\\x2ddesktop-.scope",
        file_name: "zz-steno.conf",
        contents: include_str!("../linux/gnome-scope-stop-timeout.conf"),
    };

    /// Both, as the `.deb` ships them.
    #[cfg(test)]
    const ALL: [Self; 2] = [Self::AUTOSTART, Self::GNOME_SCOPE];

    /// The drop-in's path below a unit directory: `<unit>.d/<file name>`.
    fn relative_path(&self) -> String {
        format!("{}.d/{}", self.unit, self.file_name)
    }

    /// The user's own copy: `$HOME/.config/systemd/user/<unit>.d/<file
    /// name>`, whatever `XDG_CONFIG_HOME` says. The plugin writes the
    /// autostart entry to `$HOME/.config/autostart` (auto-launch), so the
    /// generator sees it only in a user manager whose config home is
    /// `$HOME/.config`, which is where that manager reads drop-ins; an
    /// `XDG_CONFIG_HOME` the app sees may be set for the units alone
    /// (`environment.d`, `systemctl --user set-environment`).
    fn user_path(&self, home: &Path) -> PathBuf {
        home.join(".config/systemd/user").join(self.relative_path())
    }
}

/// The mark of a reload the user manager owes, in the support directory:
/// set when `sync` writes a drop-in, cleared only by a reload that went
/// through (`settle`). A file, so the debt outlives a kill before the
/// reload.
const RELOAD_OWED: &str = "systemd-reload-owed";

/// Brings the user's copies in line with Launch at login: installs GNOME's
/// scope drop-in, and the autostart unit's for `Some(true)`; removes the
/// autostart unit's for `Some(false)`; leaves it for `None` (the entry
/// could not be read). Has the user manager reload when it wrote a file or
/// a reload is owed (`RELOAD_OWED` in `marks`, the support directory), as
/// `may_reload` allows. A failure is logged and changes nothing else:
/// Launch at login works without the drop-ins.
pub fn sync(login_item: Option<bool>, marks: Option<&Path>) {
    let Some(home) = home() else {
        tracing::warn!("no home directory for the stop timeout drop-ins");
        return;
    };
    let owed = marks.map(|directory| directory.join(RELOAD_OWED));
    sync_in(
        &home,
        owed.as_deref(),
        login_item,
        runs_as_autostart_unit(),
        || reload_user_manager(owed.clone()),
    );
}

/// `sync` under `home`, with the owed reload's mark at `owed`, for an app
/// that runs as the autostart unit or not: a write sets the mark, and
/// `reload` runs once when the mark is there (always, with no place for
/// it), as `may_reload` allows. A removal alone owes no reload.
fn sync_in(
    home: &Path,
    owed: Option<&Path>,
    login_item: Option<bool>,
    as_autostart_unit: bool,
    reload: impl FnOnce(),
) {
    let mut wrote = change(&DropIn::GNOME_SCOPE, home, true);
    if let Some(on) = login_item {
        wrote |= change(&DropIn::AUTOSTART, home, on);
    }
    if wrote && let Some(owed) = owed {
        mark_owed(owed, true);
    }
    if !(wrote || owed.is_none_or(Path::exists)) {
        return;
    }
    if may_reload(login_item, as_autostart_unit) {
        reload();
    } else {
        tracing::debug!("no reload while the app runs as the autostart unit without its entry");
    }
}

/// Sets (`on`) or clears the owed reload's mark at `owed`; a failure is
/// logged.
fn mark_owed(owed: &Path, on: bool) {
    if let Err(error) = crate::autostart::set_mark(owed, on) {
        tracing::debug!(%error, on, "the mark of an owed reload");
    }
}

/// Whether the user manager may reload its units: always when the app
/// does not run as the autostart unit, and as that unit only while its
/// entry stands (`Some(true)`). Without the entry, or with an entry that
/// could not be read, a reload may unload the running unit, and the
/// session's end then stops the app without a SIGTERM.
fn may_reload(login_item: Option<bool>, as_autostart_unit: bool) -> bool {
    login_item == Some(true) || !as_autostart_unit
}

/// Removes the autostart unit's drop-in without a reload, at the exit
/// that removes the unit's entry (`autostart::at_exit`).
pub fn remove_autostart() {
    if let Some(home) = home() {
        change(&DropIn::AUTOSTART, &home, false);
    }
}

/// The user's home directory, if it is an absolute path.
fn home() -> Option<PathBuf> {
    std::env::home_dir().filter(|home| home.is_absolute())
}

/// Installs (`on`) or removes `drop_in`'s copy under `home`; true when it
/// wrote the file. Logs the change, and a failure at `warn` with its kind,
/// the path and the error's text at `debug` (`logs.rs`).
fn change(drop_in: &DropIn, home: &Path, on: bool) -> bool {
    let path = drop_in.user_path(home);
    let changed = if on {
        install(&path, drop_in.contents)
    } else {
        remove(&path)
    };
    match changed {
        Ok(false) => false,
        Ok(true) => {
            tracing::info!(unit = drop_in.unit, on, "a stop timeout drop-in changed");
            tracing::debug!(path = %path.display(), on, "a stop timeout drop-in changed");
            on
        }
        Err(error) => {
            let failure = if on {
                "a stop timeout drop-in could not be written; the unit keeps a 5 s stop timeout"
            } else {
                "a stop timeout drop-in could not be removed"
            };
            tracing::warn!(unit = drop_in.unit, kind = %error.kind(), "{failure}");
            tracing::debug!(path = %path.display(), %error, on, "the stop timeout drop-in");
            false
        }
    }
}

/// Writes `contents` at `path` unless it already holds them; true when it
/// wrote. Atomic: a temporary file in the same directory (`temporary`),
/// renamed over the old one. The drop-ins and the autostart entry
/// (`packaged::write_entry`) are written this way.
pub(crate) fn install(path: &Path, contents: &str) -> io::Result<bool> {
    if std::fs::read(path).is_ok_and(|current| current == contents.as_bytes()) {
        return Ok(false);
    }
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("the drop-in has no directory"))?;
    std::fs::create_dir_all(directory)?;
    let temporary = temporary(path);
    let written =
        std::fs::write(&temporary, contents).and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written.map(|()| true)
}

/// The temporary file `install` renames to `path`: beside it, hidden, and
/// ending in `.tmp`, so systemd never reads it as a drop-in, nor its
/// autostart generator as an entry.
fn temporary(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}.{}.tmp", std::process::id()))
}

/// Removes the drop-in at `path`, and its directory when that is left
/// empty; true when there was one.
fn remove(path: &Path) -> io::Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    }
    if let Some(directory) = path.parent() {
        // Fails, and stays, when it holds a drop-in of the user's own.
        let _ = std::fs::remove_dir(directory);
    }
    Ok(true)
}

/// Whether this process runs in the autostart unit (`DropIn::AUTOSTART`):
/// a component of its cgroup's path in `/proc/self/cgroup` is that unit.
/// False when the file cannot be read.
pub fn runs_as_autostart_unit() -> bool {
    std::fs::read_to_string("/proc/self/cgroup")
        .is_ok_and(|cgroup| runs_in(&cgroup, DropIn::AUTOSTART.unit))
}

/// Whether a cgroup listing, as `/proc/<pid>/cgroup` holds it (one
/// `<id>:<controllers>:<path>` line per hierarchy), puts the process in
/// `unit` or below it.
fn runs_in(cgroup: &str, unit: &str) -> bool {
    cgroup
        .lines()
        .filter_map(|line| line.splitn(3, ':').nth(2))
        .any(|path| path.split('/').any(|component| component == unit))
}

/// Asks the systemd user manager on the session bus to reload its units
/// (`systemctl --user daemon-reload`), on a thread of its own so a slow
/// bus holds nothing, then settles the owed reload at `owed` (`settle`).
fn reload_user_manager(owed: Option<PathBuf>) {
    crate::session_end::spawn_client(
        "steno-reload",
        "a stop timeout drop-in may wait for the next login",
        move || {
            let reloaded = Builder::session()
                .and_then(crate::session_end::patient)
                .and_then(|session| reload(&session));
            settle(owed.as_deref(), &reloaded);
            Ok(())
        },
    );
}

/// After a reload: one that went through clears the owed reload's mark at
/// `owed`; one that failed is logged and sets it again (a reload that went
/// through meanwhile may have cleared it), so the next `sync` asks again.
fn settle(owed: Option<&Path>, reloaded: &zbus::Result<()>) {
    match reloaded {
        Ok(()) => tracing::debug!("the systemd user manager reloaded its units"),
        Err(error) => {
            tracing::warn!(
                "the systemd user manager did not reload its units; a stop timeout drop-in waits for the next launch or login"
            );
            tracing::debug!(%error, "the systemd user manager's reload");
        }
    }
    if let Some(owed) = owed {
        mark_owed(owed, reloaded.is_err());
    }
}

// systemd's manager: its bus name, object path and interface.
const SYSTEMD: &str = "org.freedesktop.systemd1";
const SYSTEMD_PATH: &str = "/org/freedesktop/systemd1";
const SYSTEMD_MANAGER: &str = "org.freedesktop.systemd1.Manager";

/// The manager's `Reload` on `connection`, which answers once the reload
/// is done.
fn reload(connection: &Connection) -> zbus::Result<()> {
    connection.call_method(
        Some(SYSTEMD),
        SYSTEMD_PATH,
        Some(SYSTEMD_MANAGER),
        "Reload",
        &(),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("steno-stop-timeout-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// The unit names are the generator's for the entry the plugin writes
    /// (`app-`, the desktop file's name without `.desktop`, escaped, `-`
    /// as `\x2d`, `@autostart.service`) and gnome-session's for the app id
    /// (`app-gnome-`, the same escaped name, `-<pid>.scope`). The name is
    /// the Linux product name.
    #[test]
    fn the_units_are_the_ones_the_desktops_make_from_the_entry() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.linux.conf.json")).unwrap();
        let entry = config["productName"].as_str().unwrap();
        assert_eq!(entry, "steno-desktop");
        let escaped = entry.replace('-', "\\x2d");
        assert_eq!(
            DropIn::AUTOSTART.unit,
            format!("app-{escaped}@autostart.service")
        );
        assert_eq!(
            DropIn::GNOME_SCOPE.unit,
            format!("app-gnome-{escaped}-.scope")
        );
    }

    /// Each file sets the stop timeout in its unit type's section, at
    /// least half again the save and the process's end.
    #[test]
    fn each_drop_in_raises_the_stop_timeout_above_the_save() {
        for (drop_in, section) in [
            (DropIn::AUTOSTART, "[Service]"),
            (DropIn::GNOME_SCOPE, "[Scope]"),
        ] {
            let sections: Vec<_> = drop_in
                .contents
                .lines()
                .filter(|line| line.starts_with('['))
                .collect();
            assert_eq!(sections, [section], "{}", drop_in.unit);
            let line = drop_in
                .contents
                .lines()
                .skip_while(|line| *line != section)
                .find_map(|line| line.strip_prefix("TimeoutStopSec="))
                .unwrap();
            let seconds: u64 = line.strip_suffix('s').unwrap().parse().unwrap();
            let save = steno_services::app::SHUTDOWN_PATIENCE + crate::EXIT_GRACE;
            assert!(
                Duration::from_secs(seconds) >= save * 3 / 2,
                "{}",
                drop_in.unit
            );
        }
    }

    /// GNOME's file comes after gnome-session's `override.conf` without
    /// replacing it; the autostart unit's comes before a user's own.
    #[test]
    fn the_file_names_sort_where_their_values_win() {
        assert!(DropIn::GNOME_SCOPE.file_name > "override.conf");
        assert!(DropIn::AUTOSTART.file_name < "override.conf");
        for drop_in in DropIn::ALL {
            assert_eq!(
                Path::new(drop_in.file_name).extension(),
                Some("conf".as_ref())
            );
        }
    }

    /// The `.deb` installs each drop-in under `/usr/lib/systemd/user` from
    /// the file its contents come from, and no other unit file.
    #[test]
    fn the_deb_installs_both_drop_ins() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let files = config["bundle"]["linux"]["deb"]["files"]
            .as_object()
            .unwrap();
        let units = files
            .keys()
            .filter(|target| target.starts_with("/usr/lib/systemd/user/"))
            .count();
        assert_eq!(units, DropIn::ALL.len());
        for drop_in in DropIn::ALL {
            let target = format!("/usr/lib/systemd/user/{}", drop_in.relative_path());
            let source = files[&target].as_str().unwrap();
            let shipped =
                std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(source))
                    .unwrap();
            assert_eq!(shipped, drop_in.contents, "{target}");
        }
    }

    /// A `systemctl` for the postinst: logs each call to `$FAKE/calls`,
    /// lists `$FAKE/managers`, answers `is-active` with `$FAKE/<user>`
    /// (`error`: a failed call, which prints nothing on stdout), and fails
    /// the reload of the user `fails`.
    const FAKE_SYSTEMCTL: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE/calls"
case $1 in
  list-units) cat "$FAKE/managers"; exit 0 ;;
  --user) [ "$2" = -M ] || exit 9; user=${3%@}; shift 3 ;;
  *) exit 9 ;;
esac
case $1 in
  is-active)
    state=$(cat "$FAKE/$user")
    [ "$state" = error ] && { echo "Failed to connect to bus" >&2; exit 1; }
    echo "$state"
    [ "$state" = active ] ;;
  daemon-reload) [ "$user" != fails ] ;;
  *) exit 9 ;;
esac
"#;

    /// A `getent passwd <uid>` over `$FAKE/passwd`.
    const FAKE_GETENT: &str = r#"#!/bin/sh
[ "$1" = passwd ] && grep "^[^:]*:x:$2:" "$FAKE/passwd"
"#;

    /// The guard on systemd running, which the test points at `$FAKE`.
    const SYSTEMD_RUNNING: &str = "[ -d /run/systemd/system ]";

    /// The `.deb`'s postinst (`postInstallScript` in `tauri.conf.json`),
    /// with a fake `systemctl` and `getent` first on its `PATH`, the
    /// running user managers' users as `(user, autostart entry, the unit's
    /// state)`, and `$HOME` (root's) holding an entry of its own. Returns
    /// the script's exit status and the `systemctl` calls.
    fn run_postinst(
        name: &str,
        argument: Option<&str>,
        systemd: bool,
        users: &[(&str, bool, &str)],
    ) -> (std::process::ExitStatus, Vec<String>) {
        use std::fmt::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let path = config["bundle"]["linux"]["deb"]["postInstallScript"]
            .as_str()
            .unwrap();
        let script =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap();
        assert_eq!(script.matches(SYSTEMD_RUNNING).count(), 1);
        let root = scratch(name);
        let fake = root.join("fake");
        let bin = root.join("bin");
        for directory in [&fake, &bin] {
            std::fs::create_dir_all(directory).unwrap();
        }
        if systemd {
            std::fs::create_dir_all(fake.join("systemd")).unwrap();
        }
        let postinst = root.join("postinst");
        std::fs::write(
            &postinst,
            script.replace(SYSTEMD_RUNNING, r#"[ -d "$FAKE/systemd" ]"#),
        )
        .unwrap();
        for (tool, contents) in [("systemctl", FAKE_SYSTEMCTL), ("getent", FAKE_GETENT)] {
            let tool = bin.join(tool);
            std::fs::write(&tool, contents).unwrap();
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let linux: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.linux.conf.json")).unwrap();
        let entry = format!(
            ".config/autostart/{}.desktop",
            linux["productName"].as_str().unwrap()
        );
        let write_entry = |home: &Path| {
            std::fs::create_dir_all(home.join(&entry).parent().unwrap()).unwrap();
            std::fs::write(home.join(&entry), b"").unwrap();
        };
        let (mut managers, mut passwd) = (String::new(), String::new());
        // One manager whose uid has no passwd entry, which is skipped.
        managers.push_str("user@999.service loaded active running User Manager for UID 999\n");
        for (uid, (user, has_entry, state)) in (1000..).zip(users) {
            let home = root.join("home").join(user);
            if *has_entry {
                write_entry(&home);
            }
            std::fs::write(fake.join(user), state).unwrap();
            writeln!(
                managers,
                "user@{uid}.service loaded active running User Manager for UID {uid}"
            )
            .unwrap();
            writeln!(passwd, "{user}:x:{uid}:{uid}::{}:/bin/sh", home.display()).unwrap();
        }
        std::fs::write(fake.join("managers"), managers).unwrap();
        std::fs::write(fake.join("passwd"), passwd).unwrap();
        let root_home = root.join("root");
        write_entry(&root_home);

        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let status = std::process::Command::new("sh")
            .arg(&postinst)
            .args(argument)
            .env("PATH", path)
            .env("FAKE", &fake)
            .env("HOME", &root_home)
            .status()
            .unwrap();
        let calls = std::fs::read_to_string(fake.join("calls")).unwrap_or_default();
        std::fs::remove_dir_all(&root).unwrap();
        (status, calls.lines().map(str::to_owned).collect())
    }

    /// At `configure` the postinst reloads every running user manager
    /// whose user has the autostart entry, or whose autostart unit is known
    /// to be stopped (`inactive`, `failed`), without asking for the unit's
    /// state where the entry stands. It skips a user whose unit runs, is
    /// starting or stopping, or whose state the call could not read, since
    /// without the entry the reload would unload a running unit; it reads
    /// the user's home from `passwd`, not root's `$HOME`. A failed reload
    /// fails nothing.
    #[test]
    fn the_debs_postinst_reloads_only_where_the_unit_survives_it() {
        let users = [
            ("entry", true, "active"),
            ("fails", true, "active"),
            ("running", false, "active"),
            ("stopped", false, "inactive"),
            ("crashed", false, "failed"),
            ("unread", false, "error"),
            ("starting", false, "activating"),
            ("stopping", false, "deactivating"),
        ];
        let (status, calls) = run_postinst("postinst", Some("configure"), true, &users);
        assert!(status.success(), "{status}");
        let mut expected = vec![
            "list-units --type=service --state=running --plain --no-legend user@*.service"
                .to_owned(),
        ];
        for (user, has_entry, state) in users {
            if !has_entry {
                expected.push(format!(
                    "--user -M {user}@ is-active {}",
                    DropIn::AUTOSTART.unit
                ));
            }
            if has_entry || ["inactive", "failed"].contains(&state) {
                expected.push(format!("--user -M {user}@ daemon-reload"));
            }
        }
        assert_eq!(calls, expected);
    }

    /// Only `configure` acts, and only where systemd runs.
    #[test]
    fn the_debs_postinst_acts_only_at_configure_under_systemd() {
        let users = [("stopped", false, "inactive")];
        for (argument, systemd) in [
            (Some("abort-upgrade"), true),
            (None, true),
            (Some("configure"), false),
        ] {
            let (status, calls) = run_postinst("postinst-idle", argument, systemd, &users);
            assert!(status.success(), "{argument:?} {systemd}: {status}");
            assert_eq!(calls, Vec::<String>::new(), "{argument:?} {systemd}");
        }
    }

    /// Under `$HOME/.config` whatever `XDG_CONFIG_HOME` says: the second
    /// half fails for a `user_path` that follows it, where the test runs
    /// with it set elsewhere (a nix-shell, some desktops).
    #[test]
    fn the_user_copy_is_under_home_dot_config() {
        if let Some(home) = home() {
            for drop_in in DropIn::ALL {
                assert!(
                    drop_in
                        .user_path(&home)
                        .starts_with(home.join(".config/systemd/user")),
                    "{}",
                    drop_in.unit
                );
            }
        }
        assert_eq!(
            DropIn::AUTOSTART.user_path(Path::new("/home/u")),
            PathBuf::from(
                "/home/u/.config/systemd/user/app-steno\\x2ddesktop@autostart.service.d/10-steno.conf"
            )
        );
        assert_eq!(
            DropIn::GNOME_SCOPE.user_path(Path::new("/home/u")),
            PathBuf::from(
                "/home/u/.config/systemd/user/app-gnome-steno\\x2ddesktop-.scope.d/zz-steno.conf"
            )
        );
    }

    #[test]
    fn install_writes_once_and_remove_cleans_up() {
        let root = scratch("cycle");
        let file = DropIn::AUTOSTART.user_path(&root);
        let contents = DropIn::AUTOSTART.contents;
        assert!(install(&file, contents).unwrap());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), contents);
        assert!(
            !install(&file, contents).unwrap(),
            "the same contents are left alone"
        );
        let directory = file.parent().unwrap();
        assert_eq!(
            std::fs::read_dir(directory).unwrap().count(),
            1,
            "no temporary file stays"
        );

        std::fs::write(&file, "[Service]\nTimeoutStopSec=5s\n").unwrap();
        assert!(
            install(&file, contents).unwrap(),
            "other contents are replaced"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), contents);

        assert!(remove(&file).unwrap());
        assert!(!directory.exists(), "the empty directory goes with it");
        assert!(!remove(&file).unwrap(), "removing twice is fine");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A rename that fails (a directory in the drop-in's place) is an
    /// error, and the temporary file goes; the temporary file is one
    /// systemd never reads.
    #[test]
    fn a_failed_install_leaves_no_temporary_file() {
        let root = scratch("blocked");
        let file = DropIn::AUTOSTART.user_path(&root);
        std::fs::create_dir_all(file.join("in-the-way")).unwrap();
        assert!(install(&file, DropIn::AUTOSTART.contents).is_err());
        let directory = file.parent().unwrap();
        let names: Vec<_> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [file.file_name().unwrap()]);

        let temporary = temporary(&file);
        assert_eq!(temporary.parent(), Some(directory));
        let name = temporary.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with('.'), "{name}");
        assert_ne!(temporary.extension(), Some("conf".as_ref()), "{name}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn remove_leaves_the_users_own_drop_ins() {
        let root = scratch("own");
        let file = DropIn::AUTOSTART.user_path(&root);
        install(&file, DropIn::AUTOSTART.contents).unwrap();
        let own = file.with_file_name("20-mine.conf");
        std::fs::write(&own, "[Service]\nNice=5\n").unwrap();
        assert!(remove(&file).unwrap());
        assert!(own.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_unwritable_directory_is_an_error_not_a_panic() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = scratch("locked");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();
        let file = DropIn::AUTOSTART.user_path(&root);
        let result = install(&file, DropIn::AUTOSTART.contents);
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Root ignores the mode, and there the write goes through.
        match result {
            Err(_) => assert!(!file.exists()),
            Ok(wrote) => assert!(wrote && file.exists()),
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// `sync` installs GNOME's drop-in always and the autostart unit's
    /// while Launch at login is on, and reloads once per write, never
    /// after a removal alone: that reload would unload a running
    /// autostarted Steno's unit. Each reload here goes through and clears
    /// the mark.
    #[test]
    fn sync_follows_the_login_item_and_reloads_after_a_write() {
        let root = scratch("sync");
        let owed = root.join(RELOAD_OWED);
        let (scope, service) = (
            DropIn::GNOME_SCOPE.user_path(&root),
            DropIn::AUTOSTART.user_path(&root),
        );
        let reloads = Cell::new(0);
        let sync = |login_item| {
            sync_in(&root, Some(&owed), login_item, false, || {
                reloads.set(reloads.get() + 1);
                settle(Some(&owed), &Ok(()));
            });
        };

        sync(None);
        assert!(scope.exists() && !service.exists());
        assert_eq!(reloads.get(), 1);

        sync(Some(true));
        assert!(scope.exists() && service.exists());
        assert_eq!(reloads.get(), 2);

        sync(Some(true));
        sync(None);
        assert_eq!(reloads.get(), 2, "nothing changed");

        sync(Some(false));
        assert!(scope.exists() && !service.exists(), "GNOME's stays");
        assert_eq!(reloads.get(), 2, "a removal does not reload");
        assert!(!owed.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A write owes a reload until one goes through: a reload that failed,
    /// or one skipped as the autostart unit without its entry, is asked
    /// for again at the next `sync`, though it writes nothing, under the
    /// same rule (`may_reload`). With no place for the mark, every `sync`
    /// reloads.
    #[test]
    fn an_owed_reload_is_asked_for_until_one_goes_through() {
        let root = scratch("owed");
        let owed = root.join("config").join(RELOAD_OWED);
        let reloads = Cell::new(0);
        let sync = |login_item, as_unit, went_through: bool| {
            sync_in(&root, Some(&owed), login_item, as_unit, || {
                reloads.set(reloads.get() + 1);
                let reloaded = if went_through {
                    Ok(())
                } else {
                    Err(zbus::Error::Failure("no manager".into()))
                };
                settle(Some(&owed), &reloaded);
            });
        };
        sync(Some(true), false, false);
        assert_eq!(reloads.get(), 1);
        assert!(owed.exists(), "a failed reload stays owed");
        sync(None, true, true);
        assert_eq!(reloads.get(), 1, "skipped as the unit without its entry");
        assert!(owed.exists());
        sync(Some(true), true, true);
        assert_eq!(reloads.get(), 2, "the owed reload, with nothing written");
        assert!(!owed.exists());
        sync(Some(true), true, true);
        sync(Some(false), false, true);
        assert_eq!(reloads.get(), 2, "nothing written, nothing owed");

        for _ in 0..2 {
            sync_in(&root, None, Some(false), false, || {
                reloads.set(reloads.get() + 1);
            });
        }
        assert_eq!(reloads.get(), 4, "no place for the mark");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// As the autostart unit, `sync` writes GNOME's drop-in (the first
    /// launch of this build, a `.deb` install) but reloads only while the
    /// entry stands: with the entry gone, or unread, the reload could
    /// unload the unit the app runs in, and stays owed.
    #[test]
    fn as_the_autostart_unit_it_reloads_only_while_the_entry_stands() {
        for (login_item, reloaded) in [(Some(false), false), (None, false), (Some(true), true)] {
            let root = scratch("as-unit");
            let owed = root.join(RELOAD_OWED);
            let reloads = Cell::new(0);
            sync_in(&root, Some(&owed), login_item, true, || {
                reloads.set(reloads.get() + 1);
            });
            assert!(
                DropIn::GNOME_SCOPE.user_path(&root).exists(),
                "{login_item:?}"
            );
            assert_eq!(reloads.get(), usize::from(reloaded), "{login_item:?}");
            assert!(owed.exists(), "{login_item:?}");
            std::fs::remove_dir_all(&root).unwrap();
        }
    }

    /// A reload that went through clears the mark, and a failed one sets
    /// it, also after a success cleared it meanwhile.
    #[test]
    fn a_reload_settles_the_owed_mark() {
        let root = scratch("settle");
        let owed = root.join(RELOAD_OWED);
        settle(Some(&owed), &Err(zbus::Error::Failure("no manager".into())));
        assert!(owed.exists());
        settle(Some(&owed), &Ok(()));
        assert!(!owed.exists());
        settle(None, &Ok(()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_reload_may_unload_the_autostart_unit_only_without_its_entry() {
        for login_item in [Some(true), Some(false), None] {
            assert!(may_reload(login_item, false), "{login_item:?}");
        }
        assert!(may_reload(Some(true), true));
        assert!(!may_reload(Some(false), true));
        assert!(!may_reload(None, true));
    }

    #[test]
    fn the_cgroup_names_the_autostart_unit() {
        let unit = DropIn::AUTOSTART.unit;
        let v2 = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-steno\\x2ddesktop@autostart.service\n";
        assert!(runs_in(v2, unit));
        let uwsm = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-graphical.slice/app-steno\\x2ddesktop@autostart.service/sub\n";
        assert!(runs_in(uwsm, unit), "a cgroup below the unit is in it");
        let hybrid = "12:cpu,cpuacct:/\n1:name=systemd:/user.slice/user-1000.slice/user@1000.service/app.slice/app-steno\\x2ddesktop@autostart.service\n0::/\n";
        assert!(runs_in(hybrid, unit));
        let gnome = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-steno\\x2ddesktop-4242.scope\n";
        assert!(!runs_in(gnome, unit));
        let terminal = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-org.gnome.Terminal.slice/vte-spawn-1.scope\n";
        assert!(!runs_in(terminal, unit));
        let service = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/steno.service\n";
        assert!(!runs_in(service, unit), "the NixOS module's user service");
        let other = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-steno\\x2ddesktop@autostart.service.bak\n";
        assert!(!runs_in(other, unit));
        assert!(!runs_in("", unit));
    }

    /// On this process's real cgroup listing: its own innermost cgroup
    /// counts, and the test runner is not the autostart unit.
    #[test]
    fn the_real_cgroup_names_its_own_unit() {
        let Ok(cgroup) = std::fs::read_to_string("/proc/self/cgroup") else {
            return;
        };
        let innermost = cgroup
            .lines()
            .filter_map(|line| line.splitn(3, ':').nth(2))
            .filter_map(|path| path.rsplit('/').next())
            .find(|name| !name.is_empty());
        if let Some(innermost) = innermost {
            assert!(runs_in(&cgroup, innermost), "{cgroup}");
        }
        assert!(!runs_as_autostart_unit(), "{cgroup}");
    }

    /// systemd's manager as far as `Reload` goes: each call arrives on the
    /// channel.
    struct FakeSystemd {
        reloaded: mpsc::Sender<()>,
    }

    #[zbus::interface(name = "org.freedesktop.systemd1.Manager")]
    impl FakeSystemd {
        fn reload(&self) {
            let _ = self.reloaded.send(());
        }
    }

    /// The reload is one `Reload` on systemd's manager.
    #[test]
    fn the_reload_calls_the_managers_reload() {
        let Some(daemon) = crate::session_end::tests::Daemon::start() else {
            return;
        };
        let client = daemon.connect();
        let (reloaded, reloads) = mpsc::channel();
        let _systemd = daemon
            .builder()
            .name(SYSTEMD)
            .unwrap()
            .serve_at(SYSTEMD_PATH, FakeSystemd { reloaded })
            .unwrap()
            .build()
            .unwrap();
        reload(&client).unwrap();
        reloads
            .recv_timeout(Duration::from_secs(10))
            .expect("the manager reloaded");
        assert!(reloads.try_recv().is_err(), "once");
    }
}
