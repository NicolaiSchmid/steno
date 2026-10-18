//! Linux: the systemd drop-ins that raise the stop timeout of the unit
//! Steno runs in from 5 s to 20 s, so the save on SIGTERM
//! (`SHUTDOWN_PATIENCE` plus `EXIT_GRACE`) finishes before `SIGKILL`. One
//! is for the XDG autostart unit, one for GNOME's app scopes; the `.deb`
//! ships both and `sync` writes the user's own copies.

use std::io;
use std::path::{Path, PathBuf};

use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;

/// One drop-in: its unit, its file name in the unit's `.d` directory, and
/// its contents, byte for byte the file the `.deb` installs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DropIn {
    /// The unit's name; for GNOME's scopes, the prefix systemd also reads
    /// drop-ins under (the name cut after its last `-`, plus `.scope`).
    unit: &'static str,
    file_name: &'static str,
    contents: &'static str,
}

impl DropIn {
    /// The unit the generator makes from `steno-desktop.desktop`, with `-`
    /// escaped as `\x2d`. Named `10-` so a user's own drop-in still wins.
    const AUTOSTART: Self = Self {
        unit: "app-steno\\x2ddesktop@autostart.service",
        file_name: "10-steno.conf",
        contents: include_str!("../linux/autostart-service-stop-timeout.conf"),
    };

    /// GNOME's scope for the app id `steno-desktop`. The name sorts after
    /// gnome-session's `override.conf` rather than replacing it, which
    /// would drop its `PartOf=graphical-session.target`.
    const GNOME_SCOPE: Self = Self {
        unit: "app-gnome-steno\\x2ddesktop-.scope",
        file_name: "zz-steno.conf",
        contents: include_str!("../linux/gnome-scope-stop-timeout.conf"),
    };

    /// Both, as the `.deb` ships them.
    #[cfg(test)]
    const ALL: [Self; 2] = [Self::AUTOSTART, Self::GNOME_SCOPE];

    fn relative_path(&self) -> String {
        format!("{}.d/{}", self.unit, self.file_name)
    }

    /// The user's own copy under `$HOME/.config/systemd/user`, whatever
    /// `XDG_CONFIG_HOME` says: the generator sees the entry only in a
    /// manager whose config home is `$HOME/.config`.
    fn user_path(&self, home: &Path) -> PathBuf {
        home.join(".config/systemd/user").join(self.relative_path())
    }
}

/// The mark of a reload the user manager owes: set when `sync` writes a
/// drop-in, cleared by a reload that went through. A file, so the debt
/// outlives a kill.
const RELOAD_OWED: &str = "systemd-reload-owed";

/// Brings the user's copies in line with Launch at login (`None`: the
/// entry could not be read, leave it) and reloads the user manager when
/// a file was written or a reload is owed, as `may_reload` allows. A
/// failure is logged: Launch at login works without the drop-ins.
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

/// `sync` under `home`, with the owed reload's mark at `owed`. A removal
/// alone owes no reload.
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

fn mark_owed(owed: &Path, on: bool) {
    if let Err(error) = crate::autostart::set_mark(owed, on) {
        tracing::debug!(%error, on, "the mark of an owed reload");
    }
}

/// Whether the user manager may reload: as the autostart unit only while
/// its entry stands, since a reload without it unloads the running unit
/// and the session's end then stops the app without a SIGTERM.
fn may_reload(login_item: Option<bool>, as_autostart_unit: bool) -> bool {
    login_item == Some(true) || !as_autostart_unit
}

/// Removes the autostart unit's drop-in without a reload.
pub fn remove_autostart() {
    if let Some(home) = home() {
        change(&DropIn::AUTOSTART, &home, false);
    }
}

fn home() -> Option<PathBuf> {
    std::env::home_dir().filter(|home| home.is_absolute())
}

/// Installs (`on`) or removes `drop_in`'s copy under `home`; true when it
/// wrote the file.
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

/// Writes `contents` at `path` unless it already holds them, through a
/// temporary file renamed over the old one; true when it wrote.
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

/// Beside `path`, hidden and ending in `.tmp`, so systemd never reads it
/// as a drop-in or an autostart entry.
fn temporary(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}.{}.tmp", std::process::id()))
}

/// Removes the drop-in at `path`, and its directory when left empty.
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

/// Whether this process runs in the autostart unit, by its cgroup path.
pub fn runs_as_autostart_unit() -> bool {
    std::fs::read_to_string("/proc/self/cgroup")
        .is_ok_and(|cgroup| runs_in(&cgroup, DropIn::AUTOSTART.unit))
}

/// Whether a `/proc/<pid>/cgroup` listing puts the process in `unit` or
/// below it.
fn runs_in(cgroup: &str, unit: &str) -> bool {
    cgroup
        .lines()
        .filter_map(|line| line.splitn(3, ':').nth(2))
        .any(|path| path.split('/').any(|component| component == unit))
}

/// Has the user manager reload its units on a thread of its own, then
/// settles the owed reload.
fn reload_user_manager(owed: Option<PathBuf>) {
    crate::dbus::spawn_client(
        "steno-reload",
        "a stop timeout drop-in may wait for the next login",
        move || {
            let reloaded = Builder::session()
                .and_then(crate::dbus::patient)
                .and_then(|session| reload(&session));
            settle(owed.as_deref(), &reloaded);
            Ok(())
        },
    );
}

/// Clears the owed mark after a reload that went through; sets it again
/// after a failed one, so the next `sync` asks again.
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

const SYSTEMD: &str = "org.freedesktop.systemd1";
const SYSTEMD_PATH: &str = "/org/freedesktop/systemd1";
const SYSTEMD_MANAGER: &str = "org.freedesktop.systemd1.Manager";

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

    /// The unit names match the generator's and gnome-session's naming.
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

    /// Each file sets a stop timeout longer than the save and the exit.
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

    /// File names sort after gnome-session's and before a user's own.
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

    /// The `.deb` installs exactly these drop-ins, from their source files.
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

    /// A fake `systemctl`: logs calls to `$FAKE/calls`, lists
    /// `$FAKE/managers`, answers `is-active` from `$FAKE/<user>` and fails
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

    const SYSTEMD_RUNNING: &str = "[ -d /run/systemd/system ]";

    /// Runs the `.deb`'s postinst with fake `systemctl` and `getent` and
    /// the given `(user, entry, unit state)` managers. Returns the exit
    /// status and the `systemctl` calls.
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

    /// At `configure` the postinst reloads each user manager whose user
    /// has the entry or whose unit is known stopped, skips the rest, reads
    /// homes from `passwd`, and survives a failed reload.
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

    /// The path ignores `XDG_CONFIG_HOME`.
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
        // A hard link keeps the old bytes: the file was renamed over.
        let old = root.join("old");
        std::fs::hard_link(&file, &old).unwrap();
        assert!(
            install(&file, contents).unwrap(),
            "other contents are replaced"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), contents);
        assert_eq!(
            std::fs::read_to_string(&old).unwrap(),
            "[Service]\nTimeoutStopSec=5s\n"
        );

        assert!(remove(&file).unwrap());
        assert!(!directory.exists(), "the empty directory goes with it");
        assert!(!remove(&file).unwrap(), "removing twice is fine");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A failed rename is an error and leaves no temporary file.
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
        match result {
            Err(_) => assert!(!file.exists()),
            Ok(wrote) => assert!(wrote && file.exists()),
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// `sync` installs GNOME's drop-in always and the autostart one while
    /// Launch at login is on, and reloads per write, never after a removal.
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

    /// A failed or skipped reload stays owed until a later `sync`; with no
    /// place for the mark, every `sync` reloads.
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

    /// As the autostart unit, `sync` reloads only while the entry stands.
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

    /// The test runner's real cgroup is not the autostart unit.
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

    /// A fake systemd manager that reports each `Reload` on a channel.
    struct FakeSystemd {
        reloaded: mpsc::Sender<()>,
    }

    #[zbus::interface(name = "org.freedesktop.systemd1.Manager")]
    impl FakeSystemd {
        fn reload(&self) {
            let _ = self.reloaded.send(());
        }
    }

    #[test]
    fn the_reload_calls_the_managers_reload() {
        let Some(daemon) = crate::dbus::tests::Daemon::start() else {
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
