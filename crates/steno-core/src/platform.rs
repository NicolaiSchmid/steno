//! The OS a Steno app runs on.

use crate::string_enum;

string_enum! {
    /// The OS the app runs on, which decides the words a user reads ("this Mac"
    /// or "this computer"), the shortcut keys and the permissions onboarding asks
    /// for. It is also the platform a call was recorded on.
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
