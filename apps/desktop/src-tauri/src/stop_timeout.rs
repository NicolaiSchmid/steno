//! Linux: the systemd drop-ins that give Steno the time its save needs
//! when the session stops the unit Steno runs in. systemd sends SIGTERM
//! and kills the app 5 s later in both units below, while the save the app
//! runs on SIGTERM may take up to `SHUTDOWN_PATIENCE` (ten seconds) and the
//! process ends `EXIT_GRACE` (two) after it at the latest. Each drop-in
//! raises the timeout to 20 s (`DropIn`):
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
//! The `.deb` installs both under `/usr/lib/systemd/user` (`files` in
//! `tauri.conf.json`); the app writes the same files into the user's own
//! unit directory (`sync`) for the `AppImage` and installs from before the
//! drop-ins, and has the user manager reload its units when it wrote one.
//! It never reloads after a removal: once the autostart entry is gone, a
//! reload unloads the unit a running autostarted Steno is in, and the
//! session's end then stops the app without a SIGTERM. So Launch at login,
//! turned off while the app runs as that unit (`runs_as_autostart_unit`),
//! goes off at the exit (`autostart::set_enabled`).
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
    /// The unit, or for a drop-in every unit of a prefix applies to, the
    /// unit name cut after that prefix's last `-`, as systemd looks it up.
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

/// Brings the user's copies in line with Launch at login: installs GNOME's
/// scope drop-in, and the autostart unit's for `Some(true)`; removes the
/// autostart unit's for `Some(false)`; leaves it for `None` (the entry
/// could not be read). Has the user manager reload when it wrote a file.
/// A failure is logged and changes nothing else: Launch at login works
/// without the drop-ins.
pub fn sync(login_item: Option<bool>) {
    let Some(home) = home() else {
        tracing::warn!("no home directory for the stop timeout drop-ins");
        return;
    };
    sync_in(&home, login_item, reload_user_manager);
}

/// `sync` under `home`, calling `reload` once when it wrote a file and
/// never after a removal alone.
fn sync_in(home: &Path, login_item: Option<bool>, reload: impl FnOnce()) {
    let mut wrote = change(&DropIn::GNOME_SCOPE, home, true);
    if let Some(on) = login_item {
        wrote |= change(&DropIn::AUTOSTART, home, on);
    }
    if wrote {
        reload();
    }
}

/// Removes the autostart unit's drop-in without a reload, at the exit
/// that turns Launch at login off (`autostart::turn_off_at_exit`).
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
/// renamed over the old one.
fn install(path: &Path, contents: &str) -> io::Result<bool> {
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
/// not ending in `.conf`, so systemd never reads it as a drop-in.
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
/// bus holds nothing. A failure is logged: a drop-in the manager has not
/// read may wait for the next login.
fn reload_user_manager() {
    crate::session_end::spawn_client(
        "steno-reload",
        "a stop timeout drop-in may wait for the next login",
        || {
            match Builder::session()
                .and_then(crate::session_end::patient)
                .and_then(|session| reload(&session))
            {
                Ok(()) => tracing::debug!("the systemd user manager reloaded its units"),
                Err(error) => {
                    tracing::warn!(
                        "the systemd user manager did not reload its units; a stop timeout drop-in may wait for the next login"
                    );
                    tracing::debug!(%error, "the systemd user manager's reload");
                }
            }
            Ok(())
        },
    );
}

/// systemd's manager on `connection`: its name, object and interface.
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

    /// Both, as the `.deb` ships them.
    const ALL: [DropIn; 2] = [DropIn::AUTOSTART, DropIn::GNOME_SCOPE];

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
        for drop_in in ALL {
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
        assert_eq!(units, ALL.len());
        for drop_in in ALL {
            let target = format!("/usr/lib/systemd/user/{}", drop_in.relative_path());
            let source = files[&target].as_str().unwrap();
            let shipped =
                std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(source))
                    .unwrap();
            assert_eq!(shipped, drop_in.contents, "{target}");
        }
    }

    #[test]
    fn the_user_copy_is_under_home_dot_config() {
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
    /// autostarted Steno's unit.
    #[test]
    fn sync_follows_the_login_item_and_reloads_only_after_a_write() {
        let root = scratch("sync");
        let (scope, service) = (
            DropIn::GNOME_SCOPE.user_path(&root),
            DropIn::AUTOSTART.user_path(&root),
        );
        let reloads = Cell::new(0);
        let sync = |login_item| sync_in(&root, login_item, || reloads.set(reloads.get() + 1));

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
        std::fs::remove_dir_all(&root).unwrap();
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
        let other = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-steno\\x2ddesktop@autostart.service.bak\n";
        assert!(!runs_in(other, unit));
        assert!(!runs_in("", unit));
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
