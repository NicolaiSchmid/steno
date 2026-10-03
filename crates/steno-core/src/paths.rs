//! Where the database and support files live. On the Mac this is Swift's
//! `~/Library/Application Support/Steno/`, the file the Swift app writes
//! today; Linux and Windows follow their platform conventions through the
//! `directories` crate. Audio lives in `Settings::audio_folder`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use directories::BaseDirs;

/// The support directory and what hangs off it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StenoPaths {
    pub support_directory: PathBuf,
}

impl StenoPaths {
    #[must_use]
    pub fn new(support_directory: impl Into<PathBuf>) -> Self {
        StenoPaths {
            support_directory: support_directory.into(),
        }
    }

    /// `<support>/steno.sqlite`.
    #[must_use]
    pub fn database_path(&self) -> PathBuf {
        self.support_directory.join("steno.sqlite")
    }

    /// [`StenoPaths::support_directory`] for the process environment.
    #[must_use]
    pub fn default_support_directory() -> PathBuf {
        Self::support_directory(&std::env::vars().collect())
    }

    /// The platform's support directory for `environment`, which tests pass
    /// explicitly:
    ///
    /// - macOS: `$HOME/Library/Application Support/Steno` (Swift's path;
    ///   `HOME` first because not every Foundation honours it otherwise)
    /// - Linux: `$XDG_DATA_HOME/Steno`, else `$HOME/.local/share/Steno`
    /// - Windows: `%APPDATA%\Steno`
    ///
    /// A variable holding a relative path counts as unset, as the XDG base
    /// directory specification requires for `XDG_DATA_HOME`; `HOME` and
    /// `APPDATA` get the same treatment.
    #[must_use]
    pub fn support_directory(environment: &HashMap<String, String>) -> PathBuf {
        let absolute = |key: &str| {
            environment
                .get(key)
                .map(Path::new)
                .filter(|path| path.is_absolute())
                .map(Path::to_path_buf)
        };
        let base_dirs = BaseDirs::new();
        let home = || {
            absolute("HOME")
                .or_else(|| base_dirs.as_ref().map(|dirs| dirs.home_dir().to_path_buf()))
                .unwrap_or_else(|| PathBuf::from("."))
        };
        let data = if cfg!(target_os = "macos") {
            home().join("Library").join("Application Support")
        } else if cfg!(windows) {
            absolute("APPDATA")
                .or_else(|| base_dirs.as_ref().map(|dirs| dirs.data_dir().to_path_buf()))
                .unwrap_or_else(home)
        } else {
            absolute("XDG_DATA_HOME").unwrap_or_else(|| home().join(".local").join("share"))
        };
        data.join("Steno")
    }

    /// The default paths with the support directory created.
    pub fn create_default() -> std::io::Result<StenoPaths> {
        let paths = StenoPaths::new(Self::default_support_directory());
        std::fs::create_dir_all(&paths.support_directory)?;
        Ok(paths)
    }
}

/// `path` as the `file://` URL string Swift's `URL(fileURLWithPath:)`
/// produces: percent-encoding outside the URL path-allowed set, a trailing
/// slash for a directory, forward slashes and a leading slash before a
/// Windows drive letter.
#[must_use]
pub fn file_url(path: &Path, is_directory: bool) -> String {
    let mut text = path.to_string_lossy().into_owned();
    if cfg!(windows) {
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            text = rest.to_owned();
        }
        text = text.replace('\\', "/");
        if !text.starts_with('/') {
            text.insert(0, '/');
        }
    }
    let mut url = String::from("file://");
    for byte in text.bytes() {
        if is_path_allowed(byte) {
            url.push(char::from(byte));
        } else {
            let _ = write!(url, "%{byte:02X}");
        }
    }
    if is_directory && !url.ends_with('/') {
        url.push('/');
    }
    url
}

/// The inverse of [`file_url`]: the path of a `file://` URL as this
/// machine spells it, percent-decoding what `file_url` encoded. `None` for
/// any other scheme or a host other than the local one. On Unix the
/// decoded bytes become the path as they are (a file name need not be
/// UTF-8 there); on Windows they must be UTF-8.
#[must_use]
pub fn path_from_file_url(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let mut bytes = Vec::with_capacity(rest.len());
    let raw = rest.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'%' && index + 2 < raw.len() {
            let hex = std::str::from_utf8(&raw[index + 1..index + 3]).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            bytes.push(raw[index]);
            index += 1;
        }
    }
    if bytes.len() > 1 && bytes.last() == Some(&b'/') {
        bytes.pop();
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
    }
    #[cfg(not(unix))]
    {
        let mut text = String::from_utf8(bytes).ok()?;
        if cfg!(windows) {
            // `/C:/...` becomes `C:\...`.
            if text.len() >= 3 && text.as_bytes()[2] == b':' {
                text.remove(0);
            }
            text = text.replace('/', "\\");
        }
        Some(PathBuf::from(text))
    }
}

/// RFC 3986 unreserved and sub-delims plus `:`, `@` and `/`: the characters
/// Foundation leaves alone in a file URL's path.
fn is_path_allowed(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/".contains(&byte)
}

/// The local path of a `file://` URL as [`file_url`] spells it: the
/// percent-encoding undone, any host part dropped (`file://server/share/x`
/// reads as `/share/x`), a directory's trailing slash dropped except on `/`
/// and a drive root `/X:/` (the audio folder reads back as `.../audio`),
/// and on Windows a drive path restored with backslashes.
/// A `%` that is not followed by two hex digits is kept as it is. The
/// decoded bytes must be UTF-8. `None` for any other scheme or a URL
/// without a path. Swift: `URL.path` of the stored `mixdownURL` or
/// `audioFolder`.
#[must_use]
pub fn file_url_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let path = match rest.find('/') {
        Some(0) => rest,
        Some(slash) => &rest[slash..],
        None => return None,
    };
    let raw = path.as_bytes();
    let mut bytes = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'%'
            && let [high, low, ..] = &raw[index + 1..]
            && high.is_ascii_hexdigit()
            && low.is_ascii_hexdigit()
            && let Ok(byte) = u8::from_str_radix(&path[index + 1..index + 3], 16)
        {
            bytes.push(byte);
            index += 3;
        } else {
            bytes.push(raw[index]);
            index += 1;
        }
    }
    drop_trailing_slash(&mut bytes);
    let decoded = String::from_utf8(bytes).ok()?;
    // `file_url` spelt a Windows drive path with forward slashes behind a
    // leading `/`; both are undone so the path reads back as the OS spells
    // it. A path without a drive (a Mac URL read on Windows) keeps its
    // slashes, which Windows reads as separators all the same.
    #[cfg(windows)]
    let decoded = if decoded.starts_with('/') && decoded.as_bytes().get(2) == Some(&b':') {
        decoded[1..].replace('/', "\\")
    } else {
        decoded
    };
    Some(PathBuf::from(decoded))
}

/// Drops a directory's one trailing `/` from decoded path bytes, as Swift's
/// `URL.path` does, before any UTF-8 step: `/` stays, and so does a drive
/// root `/X:/` (a drive letter `X`), because `C:` alone is drive-relative.
/// Every other `.../` loses the slash, a name ending in `:` included
/// (`/Volumes/a:/` reads as `/Volumes/a:`).
fn drop_trailing_slash(bytes: &mut Vec<u8>) {
    let drive_root =
        matches!(bytes.as_slice(), [b'/', drive, b':', b'/'] if drive.is_ascii_alphabetic());
    if bytes.len() > 1 && bytes.last() == Some(&b'/') && !drive_root {
        bytes.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_round_trip_through_the_path() {
        for path in [
            "/Users/me/Audio Files/a b.caf",
            "/tmp/plain.caf",
            "/ü/ß.wav",
        ] {
            let url = file_url(Path::new(path), false);
            assert_eq!(path_from_file_url(&url), Some(PathBuf::from(path)), "{url}");
        }
        assert_eq!(
            path_from_file_url("file:///tmp/dir/"),
            Some(PathBuf::from("/tmp/dir"))
        );
        assert_eq!(path_from_file_url("https://example.com/a"), None);
    }

    /// A Unix file name is bytes; one that is not UTF-8 still has a path.
    #[cfg(unix)]
    #[test]
    fn non_utf8_bytes_become_a_path_on_unix() {
        use std::os::unix::ffi::OsStringExt;
        let bytes = |url: &str| path_from_file_url(url).unwrap().into_os_string().into_vec();
        assert_eq!(
            bytes("file:///tmp/%FF%FEname.caf"),
            b"/tmp/\xFF\xFEname.caf"
        );
        assert_eq!(bytes("file:///tmp/%FF/"), b"/tmp/\xFF");
    }

    #[test]
    fn file_urls_match_foundation() {
        if cfg!(windows) {
            assert_eq!(
                file_url(Path::new(r"C:\Users\x\AppData\Roaming\Steno\Audio"), true),
                "file:///C:/Users/x/AppData/Roaming/Steno/Audio/"
            );
        } else {
            assert_eq!(
                file_url(
                    Path::new("/Users/x/Library/Application Support/Steno/Audio"),
                    true
                ),
                "file:///Users/x/Library/Application%20Support/Steno/Audio/"
            );
            assert_eq!(
                file_url(Path::new("/tmp/a b/ü.wav"), false),
                "file:///tmp/a%20b/%C3%BC.wav"
            );
        }
    }

    #[test]
    fn file_url_paths_round_trip() {
        let path = if cfg!(windows) {
            Path::new(r"C:\Users\x\a b\ü.wav")
        } else {
            Path::new("/Users/x/a b/ü.wav")
        };
        assert_eq!(file_url_path(&file_url(path, false)).as_deref(), Some(path));
        assert_eq!(
            file_url_path("file://localhost/tmp/x.wav"),
            Some(PathBuf::from("/tmp/x.wav")),
            "a host part is dropped"
        );
        assert_eq!(
            file_url_path("file:///tmp/100%25/x.wav"),
            Some(PathBuf::from("/tmp/100%/x.wav"))
        );
        assert_eq!(
            file_url_path("file:///tmp/%€/%4/%"),
            Some(PathBuf::from("/tmp/%€/%4/%")),
            "a percent sign without two hex digits stays as it is"
        );
        assert_eq!(file_url_path("file:///tmp/%FF.wav"), None, "not UTF-8");
        assert_eq!(file_url_path("https://example.com/x"), None);
        assert_eq!(file_url_path("file://host-only"), None);
    }

    /// The trailing-slash rule, in one place. #166 adopts the same rule;
    /// whichever merges second keeps one spelling and the union of both
    /// test sets.
    #[test]
    fn a_directory_loses_its_trailing_slash_as_url_path_does() {
        let dropped = |path: &[u8]| {
            let mut bytes = path.to_vec();
            drop_trailing_slash(&mut bytes);
            bytes
        };
        assert_eq!(dropped(b"/tmp/a:/"), b"/tmp/a:");
        assert_eq!(dropped(b"/Volumes/a:/"), b"/Volumes/a:");
        assert_eq!(dropped(b"/Users/x/"), b"/Users/x");
        assert_eq!(dropped(b"/C:/x/"), b"/C:/x");
        assert_eq!(dropped(b"/tmp/x//"), b"/tmp/x/", "one slash only");
        assert_eq!(dropped(b"/tmp/x"), b"/tmp/x");
        assert_eq!(dropped(b"/"), b"/", "the root keeps its slash");
        assert_eq!(dropped(b"/C:/"), b"/C:/", "a drive root keeps its slash");
        assert_eq!(dropped(b"/z:/"), b"/z:/");
        assert_eq!(dropped(b"/1:/"), b"/1:", "only a letter names a drive");
        assert_eq!(dropped(b"/ab:/"), b"/ab:");
        assert_eq!(
            dropped(b"/tmp/\xFF/"),
            b"/tmp/\xFF",
            "the rule reads bytes, before any UTF-8 step"
        );

        // Through `file_url_path`, which needs UTF-8 after the rule.
        assert_eq!(file_url_path("file:///tmp/%FF/"), None, "not UTF-8");
        let path = |url: &str| file_url_path(url).unwrap().to_string_lossy().into_owned();
        assert_eq!(path("file:///tmp/a:/"), "/tmp/a:");
        assert_eq!(path("file:///Volumes/a:/"), "/Volumes/a:");
        if cfg!(windows) {
            assert_eq!(path("file:///C:/Users/x/Audio/"), r"C:\Users\x\Audio");
            assert_eq!(path("file:///C:/"), r"C:\");
            assert_eq!(
                path("file:///Users/x/Audio/"),
                "/Users/x/Audio",
                "a path without a drive keeps its slashes"
            );
        } else {
            assert_eq!(path("file:///"), "/");
            assert_eq!(path("file:///C:/"), "/C:/");
            assert_eq!(path("file:///Users/x/"), "/Users/x");
            for directory in ["/tmp/a b", "/tmp/a:"] {
                assert_eq!(path(&file_url(Path::new(directory), true)), directory);
            }
        }
    }

    #[test]
    fn the_support_directory_follows_the_environment() {
        let environment: HashMap<String, String> = [
            ("HOME".to_owned(), "/home/nicolai".to_owned()),
            ("XDG_DATA_HOME".to_owned(), "/data".to_owned()),
            (
                "APPDATA".to_owned(),
                r"C:\Users\nicolai\AppData\Roaming".to_owned(),
            ),
        ]
        .into_iter()
        .collect();
        let directory = StenoPaths::support_directory(&environment);
        if cfg!(target_os = "macos") {
            assert_eq!(
                directory,
                PathBuf::from("/home/nicolai/Library/Application Support/Steno")
            );
        } else if cfg!(windows) {
            assert_eq!(
                directory,
                PathBuf::from(r"C:\Users\nicolai\AppData\Roaming\Steno")
            );
        } else {
            assert_eq!(directory, PathBuf::from("/data/Steno"));
        }
        assert_eq!(
            StenoPaths::new(&directory).database_path(),
            directory.join("steno.sqlite")
        );
    }
}
