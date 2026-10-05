//! The platform the pages word themselves for. A plugin with no commands
//! whose one job is an initialization script: every webview the shell
//! opens (the three windows and both panels) runs
//! `window.__STENO_PLATFORM__ = "linux";` (or `"macos"`, `"windows"`)
//! before the page's own scripts, so the first render already says "Show
//! in File Explorer" and "Ctrl+F" instead of Finder and ⌘F. The page reads
//! it in `apps/macos/web/src/lib/platform.tsx`; the Swift app sets
//! nothing, and a page with nothing set is the Mac's. The values are
//! `steno_bridge::Platform`'s, which the contract's `platform` enum
//! spells the same.
//!
//! Swift: none; the Swift app is the Mac.

use steno_bridge::Platform;
use tauri::{Runtime, plugin::TauriPlugin};

/// The script, for `platform`.
pub fn init_script(platform: Platform) -> String {
    format!("window.__STENO_PLATFORM__ = \"{}\";", platform.as_str())
}

/// The plugin that sets this build's platform in every webview.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    tauri::plugin::Builder::new("steno-platform")
        .js_init_script(init_script(Platform::CURRENT))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The script assigns the raw value the page's `platform` enum
    /// accepts, as a JavaScript string.
    #[test]
    fn the_script_sets_the_raw_value() {
        assert_eq!(
            init_script(Platform::Linux),
            r#"window.__STENO_PLATFORM__ = "linux";"#
        );
        for platform in Platform::ALL {
            assert!(init_script(*platform).contains(&format!("\"{platform}\"")));
        }
    }
}
