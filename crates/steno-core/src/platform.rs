//! The OS a Steno app runs on. Swift: none; the Swift app is the Mac.

use crate::string_enum;

string_enum! {
    /// The OS the app runs on, which decides the words a user reads ("this
    /// Mac" or "this computer", Finder or File Explorer, "Mac call" or
    /// "Linux call"), the shortcut keys (⌘ or Ctrl) and the permissions
    /// onboarding asks for. Every recording is made on the machine that
    /// runs the app (a phone's arrives as [`MeetingSource::Phone`]), so
    /// this is also the platform a call was recorded on.
    ///
    /// The bridge re-exports it as `steno_bridge::Platform`: the Tauri shell
    /// sets the raw value as the page global `window.__STENO_PLATFORM__`,
    /// and a page with nothing set is the Swift app's, always the Mac.
    ///
    /// [`MeetingSource::Phone`]: crate::MeetingSource::Phone
    pub enum Platform {
        Macos = "macos",
        Windows = "windows",
        Linux = "linux",
    }
}

impl Platform {
    /// The OS this binary was built for; any other Unix counts as Linux.
    pub const CURRENT: Platform = if cfg!(target_os = "macos") {
        Platform::Macos
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else {
        Platform::Linux
    };

    /// `mac` on the Mac, `elsewhere` on Windows and Linux: a label or
    /// sentence that differs, kept whole so each reads as one string.
    ///
    /// ```
    /// use steno_core::Platform;
    ///
    /// let machine = |platform: Platform| platform.mac_or("this Mac", "this computer");
    /// assert_eq!(machine(Platform::Macos), "this Mac");
    /// assert_eq!(machine(Platform::Windows), "this computer");
    /// assert_eq!(machine(Platform::Linux), "this computer");
    /// ```
    #[must_use]
    pub const fn mac_or(self, mac: &'static str, elsewhere: &'static str) -> &'static str {
        match self {
            Platform::Macos => mac,
            Platform::Windows | Platform::Linux => elsewhere,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_platform_is_the_build_target() {
        let expected = match std::env::consts::OS {
            "macos" => Platform::Macos,
            "windows" => Platform::Windows,
            _ => Platform::Linux,
        };
        assert_eq!(Platform::CURRENT, expected);
    }
}
