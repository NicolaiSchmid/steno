//! The platform the pages word themselves for: a plugin whose one job is
//! an initialization script setting `window.__STENO_PLATFORM__` in every
//! webview before the page's scripts, so the first render already says
//! "File Explorer" or "Ctrl+F". A page with nothing set is the Mac's.

use steno_bridge::Platform;
use tauri::{Runtime, plugin::TauriPlugin};

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

    /// The script assigns the raw value as a JavaScript string.
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
