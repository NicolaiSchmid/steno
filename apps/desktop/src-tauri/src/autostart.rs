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
//! made from it (`remove_earlier_entry`, `remove_earlier_entry_at_exit`).
//!
//! On Linux the entry comes with the systemd drop-ins that give the app
//! the time its save needs when the session stops it (`stop_timeout`).
//! Launch at login turned off while the app runs as the autostart unit
//! (`stop_timeout`) goes off when the app exits (`OFF_AT_EXIT`):
//! removing the entry then would let any reload of the user manager
//! unload the running unit, and the session's end would stop the app
//! without the SIGTERM that saves its recording.
//!
//! Swift: `LoginItemController.swift`, `LoginItemStatus` in `AppProtocols.swift`.

use steno_core::protocols::BoundaryResult;
pub use steno_host::services::LoginItemStatus;
use tauri::AppHandle;
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

use crate::bridge::{BridgeError, failed};
use crate::packaged;
#[cfg(target_os = "linux")]
use crate::stop_timeout;

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

/// The login item as the user set it: `Managed` while the system starts
/// the app at login; on Linux, an entry that goes at the exit
/// (`OFF_AT_EXIT`) is already off.
pub fn status(app: &AppHandle) -> LoginItemStatus {
    let mark = off_at_exit(app).filter(|_| cfg!(target_os = "linux"));
    with_off_at_exit(
        status_unless_managed(packaged::login_item_is_managed(), || {
            app.autolaunch().is_enabled()
        }),
        mark.as_deref(),
    )
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
/// nothing while the system manages the login item. On Linux the drop-ins
/// follow (`stop_timeout::sync`), and turning it off while the app runs as
/// the autostart unit only marks it to go at the exit (`OFF_AT_EXIT`);
/// turning it on again clears the mark.
pub fn set_enabled(app: &AppHandle, enabled: bool) -> Result<(), BridgeError> {
    change_unless_managed(packaged::login_item_is_managed(), || {
        #[cfg(target_os = "linux")]
        {
            let deferred = defers_off(enabled, stop_timeout::runs_as_autostart_unit());
            mark_off_at_exit(app, deferred)?;
            if deferred {
                tracing::info!("Launch at login goes off when the app exits");
                return Ok(());
            }
        }
        let result = if enabled {
            enable(app)
        } else {
            app.autolaunch().disable()
        };
        result.map_err(failed)?;
        #[cfg(target_os = "linux")]
        stop_timeout::sync(Some(enabled));
        Ok(())
    })
}

/// `change` unless `managed`.
fn change_unless_managed(
    managed: bool,
    change: impl FnOnce() -> Result<(), BridgeError>,
) -> Result<(), BridgeError> {
    if managed {
        tracing::debug!("launch at login is the system's; the login item stays as it is");
        return Ok(());
    }
    change()
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
pub fn remove_earlier_entry(app: &AppHandle) {
    packaged::remove_earlier_entry(&app.package_info().name);
}

/// Whether switching Launch at login to `enabled` waits for the exit: only
/// turning it off while the app runs as the autostart unit
/// (`as_autostart_unit`), whose entry the unit needs until it has stopped.
#[cfg(target_os = "linux")]
fn defers_off(enabled: bool, as_autostart_unit: bool) -> bool {
    !enabled && as_autostart_unit
}

/// The file that marks Launch at login to go off at the exit, in the
/// app's config directory. A file rather than a flag, so the choice
/// outlives a kill or a crash before the exit (`at_launch`). Read only on
/// Linux.
const OFF_AT_EXIT: &str = "launch-at-login-off-at-exit";

fn off_at_exit(app: &AppHandle) -> Option<std::path::PathBuf> {
    use tauri::Manager as _;
    app.path()
        .app_config_dir()
        .ok()
        .map(|directory| directory.join(OFF_AT_EXIT))
}

/// Sets (`on`) or clears the mark; a failure is `failed`, so the switch
/// stays where it was.
#[cfg(target_os = "linux")]
fn mark_off_at_exit(app: &AppHandle, on: bool) -> Result<(), BridgeError> {
    let mark = off_at_exit(app).ok_or_else(|| failed("the app has no config directory"))?;
    set_mark(&mark, on).map_err(|error| failed(error.kind()))
}

/// Creates (`on`) or removes the file at `mark`; removing none is fine.
#[cfg(target_os = "linux")]
fn set_mark(mark: &std::path::Path, on: bool) -> std::io::Result<()> {
    if on {
        if let Some(directory) = mark.parent() {
            std::fs::create_dir_all(directory)?;
        }
        std::fs::write(mark, b"")
    } else {
        match std::fs::remove_file(mark) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

/// The plugin's status with the mark at `mark`, if any: an entry marked
/// to go is off.
fn with_off_at_exit(status: LoginItemStatus, mark: Option<&std::path::Path>) -> LoginItemStatus {
    match status {
        LoginItemStatus::Enabled if mark.is_some_and(std::path::Path::exists) => {
            LoginItemStatus::NotRegistered
        }
        status => status,
    }
}

/// An update's relaunch is about to exit (`updater`): the next process
/// runs on in the same unit, so the mark, and an earlier build's entry
/// that waits for the exit, stay for it.
#[cfg(target_os = "linux")]
static RELAUNCHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Called before an update's relaunch runs the shutdown.
#[cfg(target_os = "linux")]
pub fn relaunching() {
    RELAUNCHING.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// After the shutdown of an exit, on Linux: an entry marked to go goes
/// now, with the autostart unit's drop-in, and no reload, so the unit
/// stays as it is until it has stopped. Not for an update's relaunch, nor
/// while the system manages the login item.
#[cfg(target_os = "linux")]
pub fn turn_off_at_exit(app: &AppHandle) {
    if packaged::login_item_is_managed() {
        return;
    }
    let Some(mark) = off_at_exit(app) else {
        return;
    };
    if !turns_off_at_exit(
        mark.exists(),
        RELAUNCHING.load(std::sync::atomic::Ordering::Relaxed),
    ) {
        return;
    }
    if let Err(error) = app.autolaunch().disable() {
        tracing::warn!(
            "Launch at login could not be turned off at the exit; it goes at the next launch"
        );
        tracing::debug!(%error, "turning Launch at login off at the exit");
        return;
    }
    stop_timeout::remove_autostart();
    clear_mark(&mark);
}

/// Whether the exit removes the entry: when it is `marked` to go, unless
/// the exit is an update's relaunch (`relaunching`), whose next process
/// runs on in the same unit.
#[cfg(target_os = "linux")]
fn turns_off_at_exit(marked: bool, relaunching: bool) -> bool {
    marked && !relaunching
}

/// Removes the mark at `mark`; a failure is logged.
#[cfg(target_os = "linux")]
fn clear_mark(mark: &std::path::Path) {
    if let Err(error) = set_mark(mark, false) {
        tracing::debug!(%error, "clearing the mark of Launch at login");
    }
}

/// After the shutdown of an exit, on Linux: an earlier build's entry that
/// waited for the exit, because the app ran as the unit made from it,
/// goes now (`packaged::remove_earlier_entry_at_exit`). Not for an
/// update's relaunch; an Xfce query's relaunch never runs as that unit.
#[cfg(target_os = "linux")]
pub fn remove_earlier_entry_at_exit(app: &AppHandle) {
    packaged::remove_earlier_entry_at_exit(
        &app.package_info().name,
        RELAUNCHING.load(std::sync::atomic::Ordering::Relaxed),
    );
}

/// What the launch does with the login item and its drop-ins on Linux.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AtLaunch {
    /// The entry stands, marked or not: its drop-in is installed.
    Keep,
    /// An entry marked to go at an exit that never came (a kill, a crash),
    /// and the app no longer runs as its unit: it goes now.
    TurnOff,
    /// No entry while the app runs as the autostart unit: an older release
    /// turned Launch at login off at once, or the user removed the entry,
    /// and an update's relaunch stayed in the unit. The entry comes back,
    /// marked to go at the exit, so the unit gets its drop-in and the
    /// reload that applies it, and outlives any other reload until it has
    /// stopped.
    Restore,
    /// No entry: its drop-in goes, and a mark left behind.
    Gone,
    /// The entry could not be read: only GNOME's drop-in is installed.
    Unread,
}

/// The step for the plugin's `status`, whether the entry is `marked` to go
/// at the exit, and whether the app runs as the autostart unit.
#[cfg(target_os = "linux")]
fn at_launch(status: LoginItemStatus, marked: bool, as_autostart_unit: bool) -> AtLaunch {
    match status {
        LoginItemStatus::Enabled if marked && !as_autostart_unit => AtLaunch::TurnOff,
        LoginItemStatus::Enabled => AtLaunch::Keep,
        LoginItemStatus::NotRegistered if as_autostart_unit => AtLaunch::Restore,
        LoginItemStatus::NotRegistered => AtLaunch::Gone,
        _ => AtLaunch::Unread,
    }
}

/// The login item the drop-ins follow after the launch's `step`
/// (`stop_timeout::sync`). `switch` removes the entry for `TurnOff`
/// (`false`) and restores it for `Restore` (`true`), and says whether that
/// worked; an entry it could not change stays as it was.
#[cfg(target_os = "linux")]
fn login_item_after(step: AtLaunch, switch: impl FnOnce(bool) -> bool) -> Option<bool> {
    match step {
        AtLaunch::Keep => Some(true),
        AtLaunch::TurnOff => Some(!switch(false)),
        AtLaunch::Restore => Some(switch(true)),
        AtLaunch::Gone => Some(false),
        AtLaunch::Unread => None,
    }
}

/// At launch, on Linux: the drop-ins follow the login item as it stands
/// (`at_launch`), so an entry written before the drop-ins existed, or by
/// an older release, gets them too, and one the user removed loses its
/// own, unless the app runs as its unit (`AtLaunch::Restore`). While the
/// system manages the login item, only GNOME's drop-in.
#[cfg(target_os = "linux")]
pub fn sync_at_launch(app: &AppHandle) {
    if packaged::login_item_is_managed() {
        stop_timeout::sync(None);
        return;
    }
    let mark = off_at_exit(app);
    let marked = mark.as_ref().is_some_and(|mark| mark.exists());
    let manager = app.autolaunch();
    let step = at_launch(
        status_from_plugin(manager.is_enabled()),
        marked,
        stop_timeout::runs_as_autostart_unit(),
    );
    let login_item = login_item_after(step, |on| {
        let switched = if on {
            // Marked first: a restored entry without its mark would stay.
            mark.as_deref()
                .ok_or_else(|| "the app has no config directory".to_owned())
                .and_then(|mark| set_mark(mark, true).map_err(|error| error.to_string()))
                .and_then(|()| enable(app).map_err(|error| error.to_string()))
        } else {
            manager.disable().map_err(|error| error.to_string())
        };
        if let Err(error) = &switched {
            if on {
                tracing::warn!(
                    "the autostart entry could not be kept until the exit; the unit keeps a 5 s stop timeout"
                );
            } else {
                tracing::warn!("Launch at login could not be turned off; it stays on");
            }
            tracing::debug!(%error, on, "the login item at launch");
        }
        switched.is_ok()
    });
    // No entry is left: a mark has nothing more to turn off.
    if login_item == Some(false)
        && let Some(mark) = &mark
    {
        clear_mark(mark);
    }
    stop_timeout::sync(login_item);
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
        let changed = || -> Result<(), BridgeError> { panic!("the plugin changed it") };
        assert!(change_unless_managed(true, changed).is_ok());
        let refused = change_unless_managed(false, || Err(failed("no stable path"))).unwrap_err();
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

    /// An entry marked to go at the exit reads as off; nothing else
    /// changes, and a mark path with no file is no mark.
    #[test]
    fn a_marked_entry_reads_as_off() {
        use LoginItemStatus::{Enabled, NotFound, NotRegistered};
        let root = std::env::temp_dir().join(format!("steno-marked-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let (marked, unmarked) = (root.join("marked"), root.join("unmarked"));
        std::fs::write(&marked, b"").unwrap();
        let marks = [Some(marked.as_path()), Some(unmarked.as_path()), None];
        assert_eq!(with_off_at_exit(Enabled, marks[0]), NotRegistered);
        assert_eq!(with_off_at_exit(Enabled, marks[1]), Enabled);
        assert_eq!(with_off_at_exit(Enabled, marks[2]), Enabled);
        for status in [NotRegistered, NotFound] {
            for mark in marks {
                assert_eq!(with_off_at_exit(status, mark), status);
            }
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Only turning Launch at login off as the autostart unit waits for
    /// the exit; turning it on, or off outside the unit, applies at once.
    #[cfg(target_os = "linux")]
    #[test]
    fn only_off_as_the_autostart_unit_waits_for_the_exit() {
        assert!(defers_off(false, true));
        assert!(!defers_off(false, false));
        assert!(!defers_off(true, true));
        assert!(!defers_off(true, false));
    }

    /// A marked entry goes at the exit, except at an update's relaunch.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_exit_removes_a_marked_entry_unless_it_relaunches() {
        assert!(turns_off_at_exit(true, false));
        assert!(!turns_off_at_exit(true, true));
        assert!(!turns_off_at_exit(false, false));
        assert!(!turns_off_at_exit(false, true));
    }

    /// The launch keeps a standing entry's drop-in, also for a marked
    /// entry while the app runs as its unit; turns a marked entry off once
    /// it does not; restores a missing entry while it does, and drops the
    /// drop-in without an entry otherwise; and changes nothing for an
    /// entry it could not read.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_launch_follows_the_entry_and_its_mark() {
        use LoginItemStatus::{Enabled, NotFound, NotRegistered};
        for as_unit in [false, true] {
            assert_eq!(at_launch(Enabled, false, as_unit), AtLaunch::Keep);
            for marked in [false, true] {
                assert_eq!(at_launch(NotFound, marked, as_unit), AtLaunch::Unread);
            }
        }
        for marked in [false, true] {
            assert_eq!(at_launch(NotRegistered, marked, false), AtLaunch::Gone);
            assert_eq!(at_launch(NotRegistered, marked, true), AtLaunch::Restore);
        }
        assert_eq!(at_launch(Enabled, true, true), AtLaunch::Keep);
        assert_eq!(at_launch(Enabled, true, false), AtLaunch::TurnOff);
    }

    /// Only `TurnOff` removes the entry and only `Restore` brings it back;
    /// an entry the launch could not change keeps the drop-in it had.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_launch_changes_the_entry_only_to_turn_off_or_restore() {
        let untouched = |step| login_item_after(step, |on| panic!("{step:?} switched to {on}"));
        assert_eq!(untouched(AtLaunch::Keep), Some(true));
        assert_eq!(untouched(AtLaunch::Gone), Some(false));
        assert_eq!(untouched(AtLaunch::Unread), None);
        for worked in [true, false] {
            let off = login_item_after(AtLaunch::TurnOff, |on| !on && worked);
            assert_eq!(off, Some(!worked));
            let restored = login_item_after(AtLaunch::Restore, |on| on && worked);
            assert_eq!(restored, Some(worked));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_mark_is_set_and_cleared() {
        let root = std::env::temp_dir().join(format!("steno-off-at-exit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mark = root.join("config").join(OFF_AT_EXIT);
        set_mark(&mark, false).unwrap();
        set_mark(&mark, true).unwrap();
        assert!(mark.exists());
        set_mark(&mark, true).unwrap();
        set_mark(&mark, false).unwrap();
        assert!(!mark.exists());
        set_mark(&mark, false).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }
}
