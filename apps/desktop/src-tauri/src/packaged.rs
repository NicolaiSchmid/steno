//! Launch at login on a packaged install (stable plan X5). The updates
//! half of X5 is `steno_services::updates::updates_are_managed`.
//!
//! - **Managed by the system** ([`login_item_is_managed`]):
//!   `STENO_LOGIN_ITEM=managed`, which the NixOS module sets for its user
//!   service and the session. The login item is then the system's:
//!   `autostart` reports `LoginItemStatus::Managed` and the app never
//!   writes, rewrites or removes its autostart entry. The one exception
//!   is an entry an earlier build wrote ([`remove_earlier_entry`]): one
//!   whose `Exec` starts a program in the Nix store, or names one of the
//!   profile paths below. Left in place, it would start a second copy
//!   beside the service, without the wrapper's environment, and a store
//!   path breaks once it is collected. It goes at launch, unless the app
//!   runs as the unit systemd made from that entry: then it goes at the
//!   exit, after the save ([`remove_earlier_entry_at_exit`]), since a
//!   reload of the user manager without the entry would leave the
//!   recorder in a unit no logout stops.
//! - **The entry's path** (`linux::launcher_path`): the autostart entry
//!   the app writes names a path that stays the same across upgrades,
//!   never `current_exe()` (on Nix the wrapped binary inside the store):
//!   [`EXEC_PATH_VARIABLE`](linux::EXEC_PATH_VARIABLE) when a package's
//!   wrapper names one, else the first of the candidates
//!   (`linux::candidates`) that resolves into the directory the running
//!   binary resolves into. Nix's wrapper (`<out>/bin/steno-desktop`) and
//!   the binary it runs (`<out>/bin/.steno-desktop-wrapped`) share that
//!   directory, so a profile's `bin/steno-desktop` counts while it links
//!   to this build. An `AppImage` keeps the plugin's own entry, which
//!   names `$APPIMAGE`. With no such path the app writes no entry, and
//!   turning launch at login on fails with
//!   [`NO_STABLE_PATH`](linux::NO_STABLE_PATH).
//!
//! Rust only: the Swift app is a bundle `SMAppService` registers.

use std::ffi::OsStr;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::{remove_earlier_entry, remove_earlier_entry_at_exit, write_entry};

/// The variable a package sets to say the system starts the app at login
/// ([`login_item_is_managed`]).
pub const LOGIN_ITEM_VARIABLE: &str = "STENO_LOGIN_ITEM";

/// Whether the system starts the app at login and owns the setting:
/// [`LOGIN_ITEM_VARIABLE`] is `managed`.
pub fn login_item_is_managed() -> bool {
    managed(std::env::var_os(LOGIN_ITEM_VARIABLE).as_deref())
}

fn managed(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new("managed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_managed_leaves_the_login_item_to_the_system() {
        assert!(managed(Some(OsStr::new("managed"))));
        assert!(!managed(None));
        assert!(!managed(Some(OsStr::new(""))));
        assert!(!managed(Some(OsStr::new("Managed"))));
        assert!(!managed(Some(OsStr::new("app"))));
    }
}
