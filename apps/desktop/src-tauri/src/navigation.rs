//! Where a window may navigate, as `WebNavigationPolicy.swift` decides for
//! the Swift host: the app's own origin and, in a debug build, the Vite dev
//! server. External links go through `system.openURL`.

use tauri::Url;

/// `tauri://localhost` on Linux and macOS, `http://tauri.localhost` on
/// Windows; `about:blank` is the webview's own empty document.
pub fn allows(url: &Url, dev_server: Option<&Url>) -> bool {
    if url.as_str() == "about:blank" {
        return true;
    }
    if url.scheme() == "tauri" {
        return true;
    }
    if matches!(url.scheme(), "http" | "https") && url.host_str() == Some("tauri.localhost") {
        return true;
    }
    dev_server.is_some_and(|dev| same_origin(url, dev))
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).expect("a test URL")
    }

    #[test]
    fn the_app_origin_is_allowed_on_every_platform() {
        assert!(allows(&url("tauri://localhost/index.html#/settings"), None));
        assert!(allows(&url("http://tauri.localhost/index.html"), None));
        assert!(allows(&url("https://tauri.localhost/"), None));
        assert!(allows(&url("about:blank"), None));
    }

    #[test]
    fn everything_else_is_cancelled() {
        assert!(!allows(
            &url("https://github.com/NicolaiSchmid/steno"),
            None
        ));
        assert!(!allows(&url("http://localhost:5173/"), None));
        assert!(!allows(&url("file:///etc/passwd"), None));
        assert!(!allows(&url("about:config"), None));
    }

    #[test]
    fn the_dev_server_is_allowed_only_when_named() {
        let dev = url("http://localhost:5173");
        assert!(allows(&url("http://localhost:5173/#/main"), Some(&dev)));
        assert!(!allows(&url("http://localhost:4173/"), Some(&dev)));
        assert!(!allows(&url("https://localhost:5173/"), Some(&dev)));
    }
}
