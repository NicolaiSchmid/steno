//! Launch at login over `tauri-plugin-autostart`: a Launch Agent on macOS,
//! the `autostart` desktop entry on Linux, the Run registry key on Windows.
//! The Swift app registers itself with `SMAppService`, whose
//! `requiresApproval` state has no Launch Agent counterpart and never
//! occurs here. The status is the host's `LoginItemStatus`, and
//! `ShellLoginItem` is the host's `LoginItem` over this module (`WP6b`), so
//! the General section reads and switches the real registration.
//!
//! On Linux outside an `AppImage` the shell writes the `autostart` entry
//! itself, in the plugin's form and file, so that it names a path that
//! outlives an upgrade (`packaged::write_entry`) rather than the plugin's
//! `current_exe()`. When the system starts the app at login
//! (`STENO_LOGIN_ITEM=managed`, `packaged`), the status is `Managed` and
//! nothing here changes the registration; only an entry an earlier build
//! wrote goes, at launch, or at the exit while the app runs as the unit
//! made from it (`at_launch`, `at_exit`).
//!
//! On Linux the entry also gets a systemd drop-in (`stop_timeout`): a
//! session that runs XDG autostart through systemd stops the unit it makes
//! from the entry 5 s after SIGTERM, less than a save can take.
//!
//! Swift: `LoginItemController.swift`, `LoginItemStatus` in `AppProtocols.swift`.

use steno_core::protocols::BoundaryResult;
pub use steno_host::services::LoginItemStatus;
use tauri::AppHandle;
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

use crate::bridge::{BridgeError, failed};
use crate::packaged;

/// The status from the plugin's answer; a registration the plugin could
/// not read is `NotFound`, its reason logged.
pub fn status_from_plugin(result: Result<bool, impl std::fmt::Display>) -> LoginItemStatus {
    match result {
        Ok(true) => LoginItemStatus::Enabled,
        Ok(false) => LoginItemStatus::NotRegistered,
        Err(error) => {
            tracing::debug!(%error, "the login item could not be read");
            LoginItemStatus::NotFound
        }
    }
}

/// The plugin, configured as the Swift app behaves: a Launch Agent (no
/// `AppleScript` prompt), no launch arguments.
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None)
}

/// The registration; `Managed` while the system starts the app at login.
pub fn status(app: &AppHandle) -> LoginItemStatus {
    status_unless_managed(packaged::login_item_is_managed(), || {
        app.autolaunch().is_enabled()
    })
}

/// `Managed` when `managed`, without asking the plugin; else the
/// plugin's answer (`status_from_plugin`).
fn status_unless_managed<E: std::fmt::Display>(
    managed: bool,
    plugin: impl FnOnce() -> Result<bool, E>,
) -> LoginItemStatus {
    if managed {
        return LoginItemStatus::Managed;
    }
    status_from_plugin(plugin())
}

/// Whether the tray's item may switch the login item: not one the system
/// manages, which shows checked and disabled.
pub fn switchable(status: LoginItemStatus) -> bool {
    status != LoginItemStatus::Managed
}

/// Registers or removes the login item; a plugin failure is `failed`,
/// which the page shows as it would any other refused command. Changes
/// nothing while the system manages the login item. On Linux the stop
/// timeout's drop-in follows the entry (`stop_timeout::keep`).
pub fn set_enabled(app: &AppHandle, enabled: bool) -> Result<(), BridgeError> {
    change_unless_managed(packaged::login_item_is_managed(), || {
        let result = if enabled {
            enable(app)
        } else {
            app.autolaunch().disable()
        };
        result?;
        #[cfg(target_os = "linux")]
        stop_timeout::keep(enabled);
        Ok(())
    })
}

/// `change` unless `managed`, its failure as `failed`.
fn change_unless_managed<E: std::fmt::Display>(
    managed: bool,
    change: impl FnOnce() -> Result<(), E>,
) -> Result<(), BridgeError> {
    if managed {
        tracing::debug!("launch at login is the system's; the login item stays as it is");
        return Ok(());
    }
    change().map_err(failed)
}

/// Writes the login item: on Linux outside an `AppImage` the entry that
/// names a stable path (`packaged::write_entry`), elsewhere the plugin's.
fn enable(app: &AppHandle) -> Result<(), tauri_plugin_autostart::Error> {
    #[cfg(target_os = "linux")]
    if writes_own_entry(tauri::Manager::env(app).appimage.as_deref()) {
        return Ok(packaged::write_entry(&app.package_info().name)?);
    }
    app.autolaunch().enable()
}

/// Whether the shell writes the Linux entry itself: outside an `AppImage`
/// (`appimage`, its `$APPIMAGE`), whose plugin entry already names a
/// stable path.
#[cfg(target_os = "linux")]
fn writes_own_entry(appimage: Option<&std::ffi::OsStr>) -> bool {
    appimage.is_none()
}

/// At launch, on Linux: while the system manages the login item, an
/// entry an earlier build wrote goes, now or at the exit
/// (`packaged::remove_earlier_entry`).
#[cfg(target_os = "linux")]
pub fn at_launch(app: &AppHandle) {
    packaged::remove_earlier_entry(&app.package_info().name);
}

/// At launch, on Linux: the stop timeout's drop-in follows the login item
/// as it stands, so an entry written before the drop-in existed, or by an
/// older release, gets it too, and one the user removed loses it. Nothing
/// happens when the entry could not be read, or while the system manages
/// the login item.
#[cfg(target_os = "linux")]
pub fn keep_stop_timeout_at_launch(app: &AppHandle) {
    match status(app) {
        LoginItemStatus::Enabled => stop_timeout::keep(true),
        LoginItemStatus::NotRegistered => stop_timeout::keep(false),
        _ => {}
    }
}

/// An update's relaunch is about to exit (`updater`): the next process
/// runs on in the same unit, so an entry that waits for the exit stays
/// for it.
#[cfg(target_os = "linux")]
static RELAUNCHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Called before an update's relaunch runs the shutdown.
#[cfg(target_os = "linux")]
pub fn relaunching() {
    RELAUNCHING.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// After the shutdown of an exit, on Linux: an earlier build's entry that
/// waited for the exit, because the app ran as the unit made from it,
/// goes now (`packaged::remove_earlier_entry_at_exit`). Not for an
/// update's relaunch; an Xfce query's relaunch never runs as that unit.
#[cfg(target_os = "linux")]
pub fn at_exit(app: &AppHandle) {
    packaged::remove_earlier_entry_at_exit(
        &app.package_info().name,
        RELAUNCHING.load(std::sync::atomic::Ordering::Relaxed),
    );
}

/// Where the user manages login items; `None` where there is no such
/// pane to open (Linux desktops differ).
pub fn system_settings_url() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("x-apple.systempreferences:com.apple.LoginItems-Settings.extension")
    } else if cfg!(target_os = "windows") {
        Some("ms-settings:startupapps")
    } else {
        None
    }
}

/// The host's login item: the plugin's registration, and the pane where
/// the user manages login items. Called with the host's lock held, so it
/// leaves the tray's check mark to its callers (`actions`, and `main.rs`
/// after the launch sequence), which run on the main thread.
pub struct ShellLoginItem {
    pub app: AppHandle,
}

impl steno_host::services::LoginItem for ShellLoginItem {
    fn status(&self) -> LoginItemStatus {
        status(&self.app)
    }

    fn set_enabled(&self, enabled: bool) -> BoundaryResult<()> {
        set_enabled(&self.app, enabled).map_err(|error| error.message.into())
    }

    fn open_system_settings(&self) {
        if let Some(url) = system_settings_url()
            && let Err(error) = crate::dialogs::open_url(&self.app, url)
        {
            tracing::warn!(%error, "opening the login items pane failed");
        }
    }
}

/// The systemd drop-in that gives an autostarted Steno the time its save
/// needs. A systemd session (KDE Plasma, uwsm sessions such as Omarchy's;
/// not GNOME, whose session starts the entries itself) runs the entries in
/// `~/.config/autostart` as units `systemd-xdg-autostart-generator` makes,
/// each with `TimeoutStopSec=5s`: at a logout, or when the compositor
/// ends, systemd sends SIGTERM and SIGKILLs the app 5 s later, while the
/// save it runs on SIGTERM may take up to `SHUTDOWN_PATIENCE` (ten
/// seconds) and the process ends `EXIT_GRACE` (two) after it at the
/// latest. The drop-in raises the timeout to 20 s
/// (`linux/autostart-stop-timeout.conf`). The `.deb` installs the same
/// file under `/usr/lib/systemd/user`; this copy, in the user's own unit
/// directory, covers the `AppImage` and installs from before the drop-in.
/// Rust only: the Swift app is a macOS login item.
#[cfg(target_os = "linux")]
pub mod stop_timeout {
    use std::ffi::OsString;
    use std::io;
    use std::path::{Path, PathBuf};

    /// The unit the generator makes from the plugin's entry,
    /// `steno-desktop.desktop` (the Linux product name in
    /// `tauri.linux.conf.json`): `app-<name>@autostart.service`, the name
    /// escaped as systemd escapes a unit name, `-` as `\x2d`.
    pub const UNIT: &str = "app-steno\\x2ddesktop@autostart.service";

    /// The drop-in's file name in the unit's `.d` directory.
    pub const FILE_NAME: &str = "10-steno.conf";

    /// The drop-in, byte for byte the file the `.deb` installs.
    pub const CONTENTS: &str = include_str!("../linux/autostart-stop-timeout.conf");

    /// Where the user manager reads the user's own drop-ins for the unit:
    /// `$XDG_CONFIG_HOME/systemd/user`, else `$HOME/.config/systemd/user`,
    /// as systemd resolves it (a relative `XDG_CONFIG_HOME` counts as
    /// unset). `None` without either.
    pub fn path(lookup: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
        let absolute = |name: &str| {
            lookup(name)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        };
        let config = absolute("XDG_CONFIG_HOME")
            .or_else(|| absolute("HOME").map(|home| home.join(".config")))?;
        Some(
            config
                .join("systemd")
                .join("user")
                .join(format!("{UNIT}.d"))
                .join(FILE_NAME),
        )
    }

    /// Writes the drop-in at `path` unless it already holds `CONTENTS`;
    /// true when it wrote. Atomic: a temporary file in the same directory
    /// (systemd reads only `*.conf` there), renamed over the old one.
    pub fn install(path: &Path) -> io::Result<bool> {
        if std::fs::read(path).is_ok_and(|current| current == CONTENTS.as_bytes()) {
            return Ok(false);
        }
        let directory = path
            .parent()
            .ok_or_else(|| io::Error::other("the drop-in has no directory"))?;
        std::fs::create_dir_all(directory)?;
        let temporary = directory.join(format!(".{FILE_NAME}.{}.tmp", std::process::id()));
        let written =
            std::fs::write(&temporary, CONTENTS).and_then(|()| std::fs::rename(&temporary, path));
        if written.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        written.map(|()| true)
    }

    /// Removes the drop-in at `path`, and its directory when that is left
    /// empty; true when there was one.
    pub fn remove(path: &Path) -> io::Result<bool> {
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

    /// Installs the drop-in for an entry that is on, removes it for one
    /// that is off, and has the user manager reload when that changed
    /// anything, so a session already running takes the new timeout.
    /// A failure is logged and changes nothing else: the login item works
    /// without the drop-in.
    pub fn keep(on: bool) {
        let Some(path) = path(|name| std::env::var_os(name)) else {
            tracing::warn!("no config directory for the autostart unit's stop timeout");
            return;
        };
        let changed = if on { install(&path) } else { remove(&path) };
        match changed {
            Ok(true) => {
                tracing::info!(path = %path.display(), on, "the autostart unit's stop timeout changed");
                reload_user_manager();
            }
            Ok(false) => {}
            Err(error) => tracing::warn!(
                %error,
                path = %path.display(),
                on,
                "the autostart unit's stop timeout could not be changed"
            ),
        }
    }

    /// Asks the systemd user manager on the session bus to reload its
    /// units (`systemctl --user daemon-reload`), on a thread of its own so
    /// a slow bus holds nothing. Without a user manager on the bus the
    /// drop-in takes effect when the next one starts, at the next login.
    fn reload_user_manager() {
        use crate::session_end::{patient, spawn_client};
        spawn_client(
            "steno-reload",
            "the autostart unit's stop timeout waits for the next login",
            || {
                patient(zbus::blocking::connection::Builder::session()?)?.call_method(
                    Some("org.freedesktop.systemd1"),
                    "/org/freedesktop/systemd1",
                    Some("org.freedesktop.systemd1.Manager"),
                    "Reload",
                    &(),
                )?;
                tracing::debug!("the systemd user manager reloaded its units");
                Ok(())
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_reads_the_plugins_answer() {
        assert_eq!(
            status_from_plugin(Ok::<bool, String>(true)),
            LoginItemStatus::Enabled
        );
        assert_eq!(
            status_from_plugin(Ok::<bool, String>(false)),
            LoginItemStatus::NotRegistered
        );
        assert_eq!(
            status_from_plugin(Err::<bool, _>("no desktop entry")),
            LoginItemStatus::NotFound
        );
        assert!(status_from_plugin(Ok::<bool, String>(true)).is_on());
        assert!(!status_from_plugin(Ok::<bool, String>(false)).is_on());
    }

    /// While the system manages the login item, the plugin is neither
    /// asked nor changed, and the tray's item cannot switch it.
    #[test]
    fn a_managed_login_item_asks_and_changes_nothing() {
        let asked = || -> Result<bool, String> { panic!("the plugin was asked") };
        assert_eq!(status_unless_managed(true, asked), LoginItemStatus::Managed);
        assert_eq!(
            status_unless_managed(false, || Ok::<bool, String>(true)),
            LoginItemStatus::Enabled
        );
        let changed = || -> Result<(), String> { panic!("the plugin changed it") };
        assert!(change_unless_managed(true, changed).is_ok());
        let refused = change_unless_managed(false, || Err("no stable path")).unwrap_err();
        assert_eq!(refused.message, "no stable path");
        assert!(!switchable(LoginItemStatus::Managed));
        assert!(switchable(LoginItemStatus::Enabled));
        assert!(switchable(LoginItemStatus::NotRegistered));
    }

    /// On Linux the shell writes the entry itself, except in an
    /// `AppImage`, where the plugin's names `$APPIMAGE`.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_appimage_keeps_the_plugins_entry() {
        assert!(writes_own_entry(None));
        assert!(!writes_own_entry(Some(std::ffi::OsStr::new(
            "/home/ada/Steno.AppImage"
        ))));
    }

    #[test]
    fn only_macos_and_windows_have_a_pane() {
        let url = system_settings_url();
        if cfg!(target_os = "linux") {
            assert_eq!(url, None);
        } else {
            assert!(url.is_some_and(|url| url.contains(':')));
        }
    }

    #[cfg(target_os = "linux")]
    mod stop_timeout {
        use std::ffi::OsString;
        use std::path::{Path, PathBuf};

        use super::super::stop_timeout::*;

        fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
            let pairs: Vec<(String, OsString)> = pairs
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).into()))
                .collect();
            move |name| {
                pairs
                    .iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value.clone())
            }
        }

        fn scratch(name: &str) -> PathBuf {
            let root = std::env::temp_dir()
                .join(format!("steno-stop-timeout-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            root
        }

        fn drop_in(root: &Path) -> PathBuf {
            path(lookup(&[("HOME", root.to_str().unwrap())])).unwrap()
        }

        /// The unit name is the generator's for the entry the plugin
        /// writes: `app-`, the desktop file's name without `.desktop`,
        /// escaped (`-` as `\x2d`), `@autostart.service`. The name is the
        /// Linux product name.
        #[test]
        fn the_unit_is_the_one_the_generator_makes_from_the_entry() {
            let config: serde_json::Value =
                serde_json::from_str(include_str!("../tauri.linux.conf.json")).unwrap();
            let entry = config["productName"].as_str().unwrap();
            assert_eq!(entry, "steno-desktop");
            let escaped = entry.replace('-', "\\x2d");
            assert_eq!(UNIT, format!("app-{escaped}@autostart.service"));
        }

        #[test]
        fn the_drop_in_raises_the_stop_timeout_above_the_save() {
            let line = CONTENTS
                .lines()
                .skip_while(|line| *line != "[Service]")
                .find_map(|line| line.strip_prefix("TimeoutStopSec="))
                .unwrap();
            let seconds: u64 = line.strip_suffix('s').unwrap().parse().unwrap();
            let save = steno_services::app::SHUTDOWN_PATIENCE + crate::EXIT_GRACE;
            assert!(std::time::Duration::from_secs(seconds) >= save * 3 / 2);
        }

        #[test]
        fn the_path_follows_the_config_home_as_systemd_does() {
            let suffix = format!("systemd/user/{UNIT}.d/10-steno.conf");
            assert_eq!(
                path(lookup(&[("HOME", "/home/u"), ("XDG_CONFIG_HOME", "/cfg")])),
                Some(PathBuf::from("/cfg").join(&suffix))
            );
            assert_eq!(
                path(lookup(&[("HOME", "/home/u"), ("XDG_CONFIG_HOME", "cfg")])),
                Some(PathBuf::from("/home/u/.config").join(&suffix))
            );
            assert_eq!(path(lookup(&[("HOME", "home")])), None);
            assert_eq!(path(lookup(&[])), None);
        }

        #[test]
        fn install_writes_once_and_remove_cleans_up() {
            let root = scratch("cycle");
            let file = drop_in(&root);
            assert!(install(&file).unwrap());
            assert_eq!(std::fs::read_to_string(&file).unwrap(), CONTENTS);
            assert!(!install(&file).unwrap(), "the same contents are left alone");
            let directory = file.parent().unwrap();
            assert_eq!(
                std::fs::read_dir(directory).unwrap().count(),
                1,
                "no temporary file stays"
            );

            std::fs::write(&file, "[Service]\nTimeoutStopSec=5s\n").unwrap();
            assert!(install(&file).unwrap(), "other contents are replaced");
            assert_eq!(std::fs::read_to_string(&file).unwrap(), CONTENTS);

            assert!(remove(&file).unwrap());
            assert!(!directory.exists(), "the empty directory goes with it");
            assert!(!remove(&file).unwrap(), "removing twice is fine");
            std::fs::remove_dir_all(&root).unwrap();
        }

        #[test]
        fn remove_leaves_the_users_own_drop_ins() {
            let root = scratch("own");
            let file = drop_in(&root);
            install(&file).unwrap();
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
            let file = drop_in(&root);
            let result = install(&file);
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            // Root ignores the mode, and there the write goes through.
            match result {
                Err(_) => assert!(!file.exists()),
                Ok(wrote) => assert!(wrote && file.exists()),
            }
            std::fs::remove_dir_all(&root).unwrap();
        }
    }
}
