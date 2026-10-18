//! Launch at login on a packaged install. With `STENO_LOGIN_ITEM=managed`
//! (the NixOS module) the system owns the login item and the app writes
//! no autostart entry, only removing one an earlier build wrote
//! (`remove_earlier_entry`). Otherwise the entry names a path that
//! survives upgrades (`linux::launcher_path`), never `current_exe()`.

use std::ffi::OsStr;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::{remove_earlier_entry, write_entry};

/// The variable a package sets to say the system starts the app at login.
pub const LOGIN_ITEM_VARIABLE: &str = "STENO_LOGIN_ITEM";

/// Whether the system starts the app at login and owns the setting.
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
