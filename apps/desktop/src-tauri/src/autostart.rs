//! Launch at login: on macOS `SMAppService.mainApp`, as the Swift app
//! registers (`main_app`, which also removes the Launch Agent a build under
//! the earlier identifier left behind); on Linux and Windows over
//! `tauri-plugin-autostart`, the `autostart` desktop entry and the Run
//! registry key. The status is the host's `LoginItemStatus`, and
//! `ShellLoginItem` is the host's `LoginItem` over this module (`WP6b`), so
//! the General section reads and switches the real registration.
//!
//! On Linux, with the systemd side in `stop_timeout`:
//!
//! - **The entry** (`enable`): outside an `AppImage` the shell writes it
//!   itself, in the plugin's form and file, naming a path that outlives an
//!   upgrade (`packaged::write_entry`).
//! - **Managed** (`STENO_LOGIN_ITEM=managed`, `packaged`): the status is
//!   `Managed` and the switch changes nothing; an entry an earlier build
//!   wrote goes at launch, or after the exit's save while the app runs as
//!   the unit made from it (`remove_earlier_entry`, `at_exit`). That unit
//!   gets its drop-in while it runs (`managed_login_item`), and loses it
//!   with the entry.
//! - **The drop-ins** follow the entry (`set_enabled`, `sync_at_launch`,
//!   both through `stop_timeout`).
//! - **The deferral**: Launch at login turned off while the app runs as
//!   the autostart unit only sets the mark (`OFF_AT_EXIT`, `defers_off`);
//!   removing the entry then would let any reload of the user manager
//!   unload the running unit, and the session's end would stop the app
//!   without the SIGTERM that saves its recording. Turned on again, it
//!   clears the mark and leaves the entry unwritten (`switch_entry`).
//! - **The exit** removes a marked entry after the save (`at_exit`),
//!   except at an update's relaunch (`relaunching`).
//! - **The launch** (`sync_at_launch`, `LaunchStep`) applies a mark a kill
//!   left, puts back an entry missing while the app runs as the unit, and
//!   syncs the drop-ins.
//!
//! Swift: `LoginItemController.swift`, `LoginItemStatus` in `AppProtocols.swift`.

mod main_app;

use steno_core::protocols::BoundaryResult;
pub use steno_host::services::LoginItemStatus;
use tauri::AppHandle;
#[cfg(not(target_os = "macos"))]
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

use crate::bridge::{BridgeError, failed};
use crate::packaged;
#[cfg(target_os = "linux")]
use crate::stop_timeout;

/// The status from the plugin's answer; a registration the plugin could
/// not read is `NotFound`, its reason logged.
#[cfg(not(target_os = "macos"))]
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

/// The plugin, with no launch arguments. It runs on Linux and Windows
/// only, so its macOS launcher is never used.
#[cfg(not(target_os = "macos"))]
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None)
}

/// The login item as the user set it: `Managed` while the system starts
/// the app at login; on Linux, an entry that goes at the exit
/// (`OFF_AT_EXIT`) is already off.
pub fn status(app: &AppHandle) -> LoginItemStatus {
    let mark = off_at_exit().filter(|_| cfg!(target_os = "linux"));
    with_off_at_exit(
        status_unless_managed(packaged::login_item_is_managed(), || {
            #[cfg(target_os = "macos")]
            {
                let _ = app;
                main_app::MainApp::status(&main_app::SystemMainApp)
            }
            #[cfg(not(target_os = "macos"))]
            {
                status_from_plugin(app.autolaunch().is_enabled())
            }
        }),
        mark.as_deref(),
    )
}

/// `Managed` when `managed`, without asking the system; else `read`'s
/// answer.
fn status_unless_managed(managed: bool, read: impl FnOnce() -> LoginItemStatus) -> LoginItemStatus {
    if managed {
        return LoginItemStatus::Managed;
    }
    read()
}

/// Whether the tray's item may switch the login item: not one the system
/// manages, which shows checked and disabled.
pub fn switchable(status: LoginItemStatus) -> bool {
    status != LoginItemStatus::Managed
}

/// Registers or removes the login item; a plugin failure is `failed`,
/// which the page shows as it would any other refused command. Changes
/// nothing while the system manages the login item. On Linux the switch
/// goes through `switch_entry`, and the drop-ins follow the entry as it
/// then stands (`stop_timeout::sync`) unless turning it off waits for the
/// exit.
pub fn set_enabled(app: &AppHandle, enabled: bool) -> Result<(), BridgeError> {
    change_unless_managed(packaged::login_item_is_managed(), || {
        #[cfg(target_os = "linux")]
        {
            switch_then_sync(
                app,
                off_at_exit().as_deref(),
                enabled,
                stop_timeout::runs_as_autostart_unit(),
                |login_item| stop_timeout::sync(login_item, marks_directory().as_deref()),
            )
        }
        #[cfg(target_os = "macos")]
        {
            let _ = app;
            main_app::set_enabled(&main_app::SystemMainApp, enabled).map_err(failed)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let result = if enabled {
                enable(app)
            } else {
                app.autolaunch().disable()
            };
            result.map_err(failed)
        }
    })
}

/// `change` unless `managed`.
fn change_unless_managed(
    managed: bool,
    change: impl FnOnce() -> Result<(), BridgeError>,
) -> Result<(), BridgeError> {
    if managed {
        tracing::debug!("Launch at login is the system's; the login item stays as it is");
        return Ok(());
    }
    change()
}

/// Writes the login item: on Linux outside an `AppImage` the entry that
/// names a stable path (`packaged::write_entry`), elsewhere the plugin's.
#[cfg(not(target_os = "macos"))]
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

/// The result of an `Entry` call.
#[cfg(target_os = "linux")]
type EntryResult<T> = Result<T, Box<dyn std::error::Error>>;

/// The autostart entry as the switch and the launch change and read it
/// (`enable`, and the plugin's `disable` and `is_enabled`): a trait, so
/// the tests drive both over a fake.
#[cfg(target_os = "linux")]
trait Entry {
    fn enable(&self) -> EntryResult<()>;
    fn disable(&self) -> EntryResult<()>;
    fn is_enabled(&self) -> EntryResult<bool>;
}

#[cfg(target_os = "linux")]
impl Entry for AppHandle {
    fn enable(&self) -> EntryResult<()> {
        Ok(enable(self)?)
    }

    fn disable(&self) -> EntryResult<()> {
        Ok(self.autolaunch().disable()?)
    }

    fn is_enabled(&self) -> EntryResult<bool> {
        Ok(self.autolaunch().is_enabled()?)
    }
}

/// Switches `entry` to `enabled` with the mark at `mark`, for an app that
/// runs as the autostart unit or not (`as_unit`). False when turning it
/// off only set the mark and waits for the exit (`defers_off`), true when
/// the entry is now as asked. Turning it on clears the mark, which calls
/// off a removal still waiting for the exit; as the unit, an entry that is
/// still there is left as it is, since the plugin rewrites an entry by
/// emptying it first, and a reload that read it empty would unload the
/// running unit. A mark or entry that could not be changed is `failed`,
/// and the switch stays where it was.
#[cfg(target_os = "linux")]
fn switch_entry(
    entry: &impl Entry,
    mark: Option<&std::path::Path>,
    enabled: bool,
    as_unit: bool,
) -> Result<bool, BridgeError> {
    let deferred = defers_off(enabled, as_unit);
    let mark = mark.ok_or_else(|| failed("the app has no support directory"))?;
    set_mark(mark, deferred).map_err(|error| failed(error.kind()))?;
    if deferred {
        return Ok(false);
    }
    if enabled && as_unit && entry.is_enabled().unwrap_or(false) {
        return Ok(true);
    }
    let result = if enabled {
        entry.enable()
    } else {
        entry.disable()
    };
    result.map_err(failed)?;
    Ok(true)
}

/// `switch_entry`, then `sync` with the entry as it now stands (`Entry`'s
/// `is_enabled`, `None` when it cannot be read), which only `Some(true)`
/// lets reload as the autostart unit. Nothing to sync when turning it off
/// waits for the exit.
#[cfg(target_os = "linux")]
fn switch_then_sync(
    entry: &impl Entry,
    mark: Option<&std::path::Path>,
    enabled: bool,
    as_unit: bool,
    sync: impl FnOnce(Option<bool>),
) -> Result<(), BridgeError> {
    if switch_entry(entry, mark, enabled, as_unit)? {
        sync(entry.is_enabled().ok());
    } else {
        tracing::info!("Launch at login goes off when the app exits");
    }
    Ok(())
}

/// Whether switching Launch at login to `enabled` waits for the exit: only
/// turning it off while the app runs as the autostart unit
/// (`as_autostart_unit`), whose entry the unit needs until it has stopped.
#[cfg(target_os = "linux")]
fn defers_off(enabled: bool, as_autostart_unit: bool) -> bool {
    !enabled && as_autostart_unit
}

/// The file that marks Launch at login to go off at the exit, in the
/// support directory (`marks_directory`). A file rather than a flag, so
/// the choice outlives a kill or a crash before the exit (`launch_step`).
/// Read only on Linux.
const OFF_AT_EXIT: &str = "launch-at-login-off-at-exit";

/// The support directory (`StenoPaths`), which holds the marks
/// (`OFF_AT_EXIT`, and `stop_timeout`'s owed reload); no identifier names
/// it, so a new one keeps them. `None` without a home, where the support
/// directory would be relative.
fn marks_directory() -> Option<std::path::PathBuf> {
    Some(steno_core::StenoPaths::default_support_directory())
        .filter(|directory| directory.is_absolute())
}

fn off_at_exit() -> Option<std::path::PathBuf> {
    marks_directory().map(|directory| directory.join(OFF_AT_EXIT))
}

/// Creates (`on`) or removes the file at `mark`; removing none is fine.
#[cfg(target_os = "linux")]
pub fn set_mark(mark: &std::path::Path, on: bool) -> std::io::Result<()> {
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

/// At launch, on macOS, once the database is open and before the host's
/// first-launch registration: the Launch Agent a build under the earlier
/// identifier left behind goes (`main_app::remove_earlier_agent`), also
/// while the system manages the login item. Nothing goes in a smoke run, a
/// `fixture-host` build or a run from outside an app bundle
/// (`main_app::removes_earlier_agent`), which would remove the agent an
/// installed Steno still starts. Linux's `remove_earlier_entry` does the
/// same for its autostart entry, but only while the system manages the
/// login item.
#[cfg(target_os = "macos")]
pub fn remove_earlier_agent() {
    let exe = std::env::current_exe().ok();
    if !main_app::removes_earlier_agent(main_app::smoke_run(), exe.as_deref()) {
        return;
    }
    main_app::log(&main_app::remove_earlier_agent(
        &main_app::UserLaunchAgents::of_home(),
        std::process::id(),
    ));
}

/// An update's relaunch is about to exit (`updater`): the next process
/// runs on in the same unit, so the mark, and an earlier build's entry
/// that waits for the exit, stay for it.
#[cfg(target_os = "linux")]
static RELAUNCHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Called before an update's relaunch runs the shutdown
/// (`main::shut_down_for_relaunch`).
#[cfg(target_os = "linux")]
pub fn relaunching() {
    RELAUNCHING.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether this exit is an update's relaunch (`relaunching`), for
/// `at_exit`.
#[cfg(target_os = "linux")]
pub fn relaunching_now() -> bool {
    RELAUNCHING.load(std::sync::atomic::Ordering::Relaxed)
}

/// After the shutdown of an exit, on Linux, unless it is an update's
/// relaunch (`relaunching`), whose next process runs on in the same unit:
/// while the system manages the login item, an earlier build's entry that
/// waited for the exit goes (`packaged::remove_earlier_entry_at_exit`);
/// otherwise an entry marked to go goes. Either way the autostart unit's
/// drop-in goes with the entry, and there is no reload, so the unit stays
/// as it is until it has stopped. The one save that may not end the
/// process is the one at an Xfce query on Wayland, which relaunches the
/// app when the session goes on (`session_end::SaveAndQuit::of`);
/// xfce4-session starts autostart entries itself, so that app is not the
/// autostart unit, and a relaunch that did run as the unit would put the
/// entry back (`LaunchStep::Restore`).
#[cfg(target_os = "linux")]
pub fn at_exit(app: &AppHandle, relaunching: bool) {
    if packaged::login_item_is_managed() {
        if packaged::remove_earlier_entry_at_exit(&app.package_info().name, relaunching) {
            stop_timeout::remove_autostart();
        }
        return;
    }
    let Some(mark) = off_at_exit() else {
        return;
    };
    if !exit_turns_off(mark.exists(), relaunching) {
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
fn exit_turns_off(marked: bool, relaunching: bool) -> bool {
    marked && !relaunching
}

/// Removes the mark at `mark`; a failure is logged.
#[cfg(target_os = "linux")]
fn clear_mark(mark: &std::path::Path) {
    if let Err(error) = set_mark(mark, false) {
        tracing::debug!(%error, "clearing the mark of Launch at login");
    }
}

/// What the launch does with the login item and its drop-ins on Linux.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchStep {
    /// The entry stands, marked or not: its drop-in is installed.
    Keep,
    /// An entry marked to go at an exit that never came (a kill, a crash),
    /// and the app no longer runs as its unit: it goes now.
    TurnOff,
    /// No entry while the app runs as the autostart unit (an older release
    /// removed it at once, or the user did, and then an update relaunched
    /// in the unit). The entry comes back, marked to go at the exit, so the
    /// unit gets its drop-in and the reload that applies it, and outlives
    /// any other reload until it has stopped.
    Restore,
    /// No entry: its drop-in goes, and so does a mark left behind.
    Gone,
    /// The entry could not be read: only GNOME's drop-in is installed.
    Unread,
}

/// The step for the plugin's `status`, whether the entry is `marked` to go
/// at the exit, and whether the app runs as the autostart unit.
#[cfg(target_os = "linux")]
fn launch_step(status: LoginItemStatus, marked: bool, as_autostart_unit: bool) -> LaunchStep {
    match status {
        LoginItemStatus::Enabled if marked && !as_autostart_unit => LaunchStep::TurnOff,
        LoginItemStatus::Enabled => LaunchStep::Keep,
        LoginItemStatus::NotRegistered if as_autostart_unit => LaunchStep::Restore,
        LoginItemStatus::NotRegistered => LaunchStep::Gone,
        _ => LaunchStep::Unread,
    }
}

/// The login item the drop-ins follow after the launch's `step`
/// (`stop_timeout::sync`). `switch` removes the entry for `TurnOff`
/// (`false`) and restores it for `Restore` (`true`), and says whether that
/// worked; an entry it could not change stays as it was.
#[cfg(target_os = "linux")]
fn login_item_after(step: LaunchStep, switch: impl FnOnce(bool) -> bool) -> Option<bool> {
    match step {
        LaunchStep::Keep => Some(true),
        LaunchStep::TurnOff => Some(!switch(false)),
        LaunchStep::Restore => Some(switch(true)),
        LaunchStep::Gone => Some(false),
        LaunchStep::Unread => None,
    }
}

/// At launch, on Linux: the drop-ins follow the login item as it stands
/// (`launch_step`), so an entry written before the drop-ins existed, or by
/// an older release, gets them too, and one the user removed loses its
/// own, unless the app runs as its unit (`LaunchStep::Restore`). While the
/// system manages the login item, the launch changes no entry, and the
/// drop-ins follow `managed_login_item`.
#[cfg(target_os = "linux")]
pub fn sync_at_launch(app: &AppHandle) {
    let as_unit = stop_timeout::runs_as_autostart_unit();
    let login_item = if packaged::login_item_is_managed() {
        managed_login_item(as_unit, app.autolaunch().is_enabled().ok())
    } else {
        launch(app, off_at_exit().as_deref(), as_unit)
    };
    stop_timeout::sync(login_item, marks_directory().as_deref());
}

/// The login item the drop-ins follow while the system manages it:
/// `Some(true)` for an app that runs as the autostart unit (`as_unit`)
/// while that unit's `entry` stands (one an earlier build wrote, which
/// goes at the exit, or the user's own), so the unit gets its 20 s and
/// the reload that applies them, which is safe while the entry stands
/// (`stop_timeout::may_reload`); `None` otherwise, an `entry` that could
/// not be read (`None`) included: GNOME's drop-in alone.
#[cfg(target_os = "linux")]
fn managed_login_item(as_unit: bool, entry: Option<bool>) -> Option<bool> {
    (as_unit && entry == Some(true)).then_some(true)
}

/// The launch's step (`launch_step`) on `entry`, with the mark at `mark`,
/// for an app that runs as the autostart unit or not (`as_unit`): the
/// login item the drop-ins then follow (`login_item_after`). A mark left
/// where no entry is is cleared.
#[cfg(target_os = "linux")]
fn launch(entry: &impl Entry, mark: Option<&std::path::Path>, as_unit: bool) -> Option<bool> {
    let step = launch_step(
        status_from_plugin(entry.is_enabled()),
        mark.is_some_and(std::path::Path::exists),
        as_unit,
    );
    let login_item = login_item_after(step, |on| {
        let switched = if on {
            restore(entry, mark)
        } else {
            entry.disable()
        };
        let Err(error) = switched else {
            return true;
        };
        let failure = if on {
            "the autostart entry could not be kept until the exit; the unit keeps a 5 s stop timeout"
        } else {
            "Launch at login could not be turned off; it stays on"
        };
        tracing::warn!("{failure}");
        tracing::debug!(%error, on, "the login item at launch");
        false
    });
    // No entry is left: a mark has nothing more to turn off.
    if login_item == Some(false)
        && let Some(mark) = mark
    {
        clear_mark(mark);
    }
    login_item
}

/// Puts the entry back for `LaunchStep::Restore`, with the mark at `mark`
/// set first: a restored entry without its mark would stay. An `enable`
/// that reports success while `is_enabled` finds no entry is a failure,
/// so the drop-ins and the reload never follow an entry that is not
/// there.
#[cfg(target_os = "linux")]
fn restore(entry: &impl Entry, mark: Option<&std::path::Path>) -> EntryResult<()> {
    let mark = mark.ok_or("the app has no support directory")?;
    set_mark(mark, true)?;
    entry.enable()?;
    if !entry.is_enabled()? {
        return Err("no autostart entry was written".into());
    }
    tracing::info!("the autostart entry is back, marked to go at the exit");
    Ok(())
}

/// Whether Launch at login is marked to go at the exit (`OFF_AT_EXIT`),
/// for the smoke run as the autostart unit (`smoke`).
#[cfg(target_os = "linux")]
pub fn marked_off_at_exit() -> bool {
    off_at_exit().is_some_and(|mark| mark.exists())
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

    /// macOS registers with `SMAppService` only: `tauri-plugin-autostart`
    /// is a dependency only off macOS, so its Launch Agent cannot come
    /// back there, and `main.rs` registers its plugin only off macOS.
    #[test]
    fn the_autostart_plugin_is_neither_built_nor_registered_on_macos() {
        let manifest = include_str!("../Cargo.toml");
        let mut table = "";
        let mut tables = Vec::new();
        for line in manifest.lines() {
            if line.starts_with('[') {
                table = line;
            } else if line.starts_with("tauri-plugin-autostart") {
                tables.push(table);
            }
        }
        assert_eq!(
            tables,
            ["[target.'cfg(not(target_os = \"macos\"))'.dependencies]"]
        );

        // Without the carriage returns a Windows checkout may add.
        let main = include_str!("main.rs").replace("\r\n", "\n");
        assert_eq!(main.matches("autostart::plugin()").count(), 1);
        assert!(main.contains(
            "    #[cfg(not(target_os = \"macos\"))]\n    {\n        builder = builder.plugin(autostart::plugin());\n    }\n"
        ));
    }

    #[cfg(not(target_os = "macos"))]
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
        let asked = || -> LoginItemStatus { panic!("the system was asked") };
        assert_eq!(status_unless_managed(true, asked), LoginItemStatus::Managed);
        assert_eq!(
            status_unless_managed(false, || LoginItemStatus::RequiresApproval),
            LoginItemStatus::RequiresApproval
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
        assert!(exit_turns_off(true, false));
        assert!(!exit_turns_off(true, true));
        assert!(!exit_turns_off(false, false));
        assert!(!exit_turns_off(false, true));
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
            assert_eq!(launch_step(Enabled, false, as_unit), LaunchStep::Keep);
            for marked in [false, true] {
                assert_eq!(launch_step(NotFound, marked, as_unit), LaunchStep::Unread);
            }
        }
        for marked in [false, true] {
            assert_eq!(launch_step(NotRegistered, marked, false), LaunchStep::Gone);
            assert_eq!(
                launch_step(NotRegistered, marked, true),
                LaunchStep::Restore
            );
        }
        assert_eq!(launch_step(Enabled, true, true), LaunchStep::Keep);
        assert_eq!(launch_step(Enabled, true, false), LaunchStep::TurnOff);
    }

    /// Only `TurnOff` removes the entry and only `Restore` brings it back;
    /// an entry the launch could not change keeps the drop-in it had.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_launch_changes_the_entry_only_to_turn_off_or_restore() {
        let untouched = |step| login_item_after(step, |on| panic!("{step:?} switched to {on}"));
        assert_eq!(untouched(LaunchStep::Keep), Some(true));
        assert_eq!(untouched(LaunchStep::Gone), Some(false));
        assert_eq!(untouched(LaunchStep::Unread), None);
        for worked in [true, false] {
            let off = login_item_after(LaunchStep::TurnOff, |on| !on && worked);
            assert_eq!(off, Some(!worked));
            let restored = login_item_after(LaunchStep::Restore, |on| on && worked);
            assert_eq!(restored, Some(worked));
        }
    }

    /// An entry in memory: whether it stands, whether `enable` writes it
    /// (`writes`; otherwise it reports success and writes nothing, as an
    /// install the system manages may), whether `enable` and `disable`
    /// fail (`refuses`) or `is_enabled` does (`unreadable`), and the
    /// changes asked for, each with whether the mark at `mark` was there.
    #[cfg(target_os = "linux")]
    struct FakeEntry {
        stands: std::cell::Cell<bool>,
        writes: bool,
        refuses: bool,
        unreadable: bool,
        mark: std::path::PathBuf,
        calls: std::cell::RefCell<Vec<(&'static str, bool)>>,
    }

    #[cfg(target_os = "linux")]
    impl FakeEntry {
        fn new(stands: bool, mark: &std::path::Path) -> Self {
            Self {
                stands: stands.into(),
                writes: true,
                refuses: false,
                unreadable: false,
                mark: mark.to_owned(),
                calls: Vec::new().into(),
            }
        }

        fn call(&self, name: &'static str) -> EntryResult<()> {
            self.calls.borrow_mut().push((name, self.mark.exists()));
            if self.refuses {
                return Err("refused".into());
            }
            Ok(())
        }

        fn calls(&self) -> Vec<(&'static str, bool)> {
            self.calls.borrow().clone()
        }
    }

    /// No change asked of a `FakeEntry`.
    #[cfg(target_os = "linux")]
    const NO_CALLS: [(&str, bool); 0] = [];

    #[cfg(target_os = "linux")]
    impl Entry for FakeEntry {
        fn enable(&self) -> EntryResult<()> {
            self.call("enable")?;
            if self.writes {
                self.stands.set(true);
            }
            Ok(())
        }

        fn disable(&self) -> EntryResult<()> {
            self.call("disable")?;
            self.stands.set(false);
            Ok(())
        }

        fn is_enabled(&self) -> EntryResult<bool> {
            if self.unreadable {
                return Err("unread".into());
            }
            Ok(self.stands.get())
        }
    }

    /// As the autostart unit, turning Launch at login off only sets the
    /// mark, and turning it on again clears it without rewriting the entry
    /// (an emptied entry, read by a reload, would unload the unit). Without
    /// an entry, or outside the unit, the switch changes the entry at once.
    #[cfg(target_os = "linux")]
    #[test]
    fn on_again_as_the_unit_calls_off_the_removal_and_keeps_the_entry() {
        let root = std::env::temp_dir().join(format!("steno-switch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mark = root.join(OFF_AT_EXIT);
        let entry = FakeEntry::new(true, &mark);
        assert!(!switch_entry(&entry, Some(&mark), false, true).unwrap());
        assert!(mark.exists() && entry.stands.get());
        assert!(switch_entry(&entry, Some(&mark), true, true).unwrap());
        assert!(!mark.exists() && entry.stands.get());
        assert_eq!(entry.calls(), NO_CALLS, "the entry was rewritten");

        let gone = FakeEntry::new(false, &mark);
        assert!(switch_entry(&gone, Some(&mark), true, true).unwrap());
        assert_eq!(gone.calls(), [("enable", false)]);
        let outside = FakeEntry::new(true, &mark);
        assert!(switch_entry(&outside, Some(&mark), true, false).unwrap());
        assert!(switch_entry(&outside, Some(&mark), false, false).unwrap());
        assert!(switch_entry(&outside, Some(&mark), true, false).unwrap());
        assert_eq!(
            outside.calls(),
            [("enable", false), ("disable", false), ("enable", false)],
            "outside the unit an entry is written again, as the plugin keeps it current"
        );
        assert!(!mark.exists());

        let refused = FakeEntry {
            refuses: true,
            ..FakeEntry::new(false, &mark)
        };
        assert!(switch_entry(&refused, Some(&mark), true, true).is_err());
        assert!(switch_entry(&refused, None, true, false).is_err());
        assert_eq!(refused.calls(), [("enable", false)], "no mark, no call");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// After a switch the drop-ins follow the entry as it then reads, not
    /// the value asked for: on is `Some(true)` only once the entry stands,
    /// off is `Some(false)`, an entry that cannot be read is `None`, and a
    /// deferred off syncs nothing. As the unit, an entry that cannot be
    /// read is written, not taken as standing.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_switch_syncs_the_entry_it_reads_back() {
        let root = std::env::temp_dir().join(format!("steno-sync-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mark = root.join(OFF_AT_EXIT);
        let synced = |entry: &FakeEntry, enabled, as_unit| {
            let mut login_item = None;
            switch_then_sync(entry, Some(&mark), enabled, as_unit, |read| {
                login_item = Some(read);
            })
            .unwrap();
            login_item
        };
        for as_unit in [false, true] {
            assert_eq!(
                synced(&FakeEntry::new(false, &mark), true, as_unit),
                Some(Some(true))
            );
            let unwritten = FakeEntry {
                writes: false,
                ..FakeEntry::new(false, &mark)
            };
            assert_eq!(synced(&unwritten, true, as_unit), Some(Some(false)));
            let unread = FakeEntry {
                unreadable: true,
                ..FakeEntry::new(false, &mark)
            };
            assert_eq!(synced(&unread, true, as_unit), Some(None));
            assert_eq!(
                unread.calls(),
                [("enable", false)],
                "as the unit: {as_unit}"
            );
        }
        assert_eq!(
            synced(&FakeEntry::new(true, &mark), false, false),
            Some(Some(false))
        );
        let deferred = FakeEntry::new(true, &mark);
        assert_eq!(
            synced(&deferred, false, true),
            None,
            "no sync until the exit"
        );
        assert!(mark.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// While managed, only the app running as the autostart unit with its
    /// entry standing gets the unit's drop-in; anything else leaves it
    /// (GNOME's alone).
    #[cfg(target_os = "linux")]
    #[test]
    fn managed_only_the_unit_with_its_entry_gets_the_drop_in() {
        assert_eq!(managed_login_item(true, Some(true)), Some(true));
        for entry in [Some(false), None] {
            assert_eq!(managed_login_item(true, entry), None);
        }
        for entry in [Some(true), Some(false), None] {
            assert_eq!(managed_login_item(false, entry), None);
        }
    }

    /// An update's relaunch marks the exit, which `at_exit` reads.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_relaunch_marks_the_exit() {
        relaunching();
        assert!(relaunching_now());
    }

    /// The launch on the plugin's calls: a restore marks the entry before
    /// it enables it, and counts only an entry it reads back, so an
    /// `enable` that wrote nothing leaves no entry, no mark, and no reload
    /// for the drop-ins; a marked entry outside the unit goes; an entry it
    /// cannot read or change stays as it was.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_launch_restores_only_an_entry_it_reads_back() {
        let root = std::env::temp_dir().join(format!("steno-launch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mark = root.join(OFF_AT_EXIT);
        let restored = FakeEntry::new(false, &mark);
        assert_eq!(launch(&restored, Some(&mark), true), Some(true));
        assert_eq!(
            restored.calls(),
            [("enable", true)],
            "enabled before the mark"
        );
        assert!(mark.exists() && restored.stands.get());

        std::fs::remove_file(&mark).unwrap();
        let unwritten = FakeEntry {
            writes: false,
            ..FakeEntry::new(false, &mark)
        };
        assert_eq!(launch(&unwritten, Some(&mark), true), Some(false));
        assert!(!mark.exists(), "a mark without an entry stays");
        let refused = FakeEntry {
            refuses: true,
            ..FakeEntry::new(false, &mark)
        };
        assert_eq!(launch(&refused, Some(&mark), true), Some(false));
        let no_directory = FakeEntry::new(false, &mark);
        assert_eq!(launch(&no_directory, None, true), Some(false));
        assert_eq!(no_directory.calls(), NO_CALLS, "enabled without a mark");

        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&mark, b"").unwrap();
        let kept = FakeEntry {
            refuses: true,
            ..FakeEntry::new(true, &mark)
        };
        assert_eq!(launch(&kept, Some(&mark), false), Some(true));
        assert!(mark.exists(), "the mark stays for the next launch");
        let marked = FakeEntry::new(true, &mark);
        assert_eq!(launch(&marked, Some(&mark), false), Some(false));
        assert_eq!(marked.calls(), [("disable", true)]);
        assert!(!mark.exists() && !marked.stands.get());

        let unread = FakeEntry {
            unreadable: true,
            ..FakeEntry::new(true, &mark)
        };
        assert_eq!(launch(&unread, Some(&mark), true), None);
        assert_eq!(unread.calls(), NO_CALLS);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The marks are in the support directory the rest of the app uses,
    /// which no identifier names, so a new identifier keeps them.
    #[test]
    fn the_marks_are_in_the_support_directory() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let identifier = config["identifier"].as_str().unwrap();
        let directory = marks_directory().unwrap();
        assert_eq!(
            directory,
            steno_core::StenoPaths::default_support_directory()
        );
        assert!(!directory.to_string_lossy().contains(identifier));
        assert_eq!(off_at_exit(), Some(directory.join(OFF_AT_EXIT)));
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
