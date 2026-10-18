//! Where the database and support files live, by platform convention.
//! Audio lives in `Settings::audio_folder`.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use directories::BaseDirs;

/// The support directory and what hangs off it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StenoPaths {
    pub support_directory: PathBuf,
}

impl StenoPaths {
    /// Paths under `support_directory`; nothing is created.
    #[must_use]
    pub fn new(support_directory: impl Into<PathBuf>) -> Self {
        StenoPaths {
            support_directory: support_directory.into(),
        }
    }

    #[must_use]
    pub fn database_path(&self) -> PathBuf {
        self.support_directory.join("steno.sqlite")
    }

    /// [`StenoPaths::support_directory`] for the process environment.
    #[must_use]
    pub fn default_support_directory() -> PathBuf {
        Self::support_directory(|name| std::env::var_os(name))
    }

    /// The platform's support directory for the environment `lookup` answers:
    ///
    /// - macOS: `$HOME/Library/Application Support/Steno`
    /// - Linux: `$XDG_DATA_HOME/Steno`, else `$HOME/.local/share/Steno`
    /// - Windows: `%APPDATA%\Steno`
    ///
    /// A relative path counts as unset. A path that is not Unicode is used as it
    /// is: skipping it would open a second database elsewhere.
    ///
    /// ```
    /// let directory = steno_core::StenoPaths::support_directory(|name| {
    ///     (name == "HOME" || name == "XDG_DATA_HOME" || name == "APPDATA")
    ///         .then(|| std::env::temp_dir().into_os_string())
    /// });
    /// assert!(directory.ends_with("Steno"));
    /// ```
    #[must_use]
    pub fn support_directory(lookup: impl Fn(&str) -> Option<OsString>) -> PathBuf {
        let absolute = |key: &str| {
            lookup(key)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
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

/// `path` as the `file://` URL string Foundation's `URL(fileURLWithPath:)`
/// produces. On Unix the path's bytes are encoded as they are, so a non-UTF-8
/// name round-trips through [`file_url_path`].
#[must_use]
pub fn file_url(path: &Path, is_directory: bool) -> String {
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes()
    };
    #[cfg(not(unix))]
    let text = {
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
        text
    };
    #[cfg(not(unix))]
    let bytes = text.as_bytes();
    let mut url = String::from("file://");
    for &byte in bytes {
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

/// The characters Foundation leaves alone in a file URL's path.
fn is_path_allowed(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/".contains(&byte)
}

/// The local path of a `file://` URL, read as Foundation's `URL.path` reads it:
/// host dropped, percent-encoding undone, one trailing slash dropped (`/` and a
/// drive root keep theirs). On Windows the bytes must be UTF-8 and a drive path
/// loses its leading slash. `None` for any other scheme or no path.
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
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
    }
    #[cfg(not(unix))]
    {
        let decoded = String::from_utf8(bytes).ok()?;
        // Undo `file_url`'s `/C:/` spelling of a drive path; a path without a drive
        // keeps its forward slashes, which Windows reads as separators.
        #[cfg(windows)]
        let decoded = if starts_with_drive(decoded.as_bytes()) {
            decoded[1..].replace('/', "\\")
        } else {
            decoded
        };
        Some(PathBuf::from(decoded))
    }
}

/// Whether decoded path bytes begin with `/X:`, `X` an ASCII letter.
fn starts_with_drive(bytes: &[u8]) -> bool {
    matches!(bytes, [b'/', drive, b':', ..] if drive.is_ascii_alphabetic())
}

/// Drops a directory's one trailing `/` from decoded path bytes; `/` and a
/// drive root `/X:/` keep it, because `C:` alone is drive-relative.
fn drop_trailing_slash(bytes: &mut Vec<u8>) {
    let drive_root = bytes.len() == 4 && starts_with_drive(bytes) && bytes[3] == b'/';
    if bytes.len() > 1 && bytes.last() == Some(&b'/') && !drive_root {
        bytes.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            file_url_path("file:///tmp/%+1/x.wav"),
            Some(PathBuf::from("/tmp/%+1/x.wav")),
            "a sign is not a hex digit"
        );
        if cfg!(windows) {
            assert_eq!(file_url_path("file:///tmp/%FF.wav"), None, "not UTF-8");
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            assert_eq!(
                file_url_path("file:///tmp/%FF.wav").as_deref(),
                Some(Path::new(std::ffi::OsStr::from_bytes(b"/tmp/\xFF.wav"))),
                "a Unix path need not be UTF-8"
            );
        }
        assert_eq!(file_url_path("https://example.com/x"), None);
        assert_eq!(file_url_path("file://host-only"), None);
    }

    /// The trailing-slash rule.
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

        let path = |url: &str| file_url_path(url).unwrap().to_string_lossy().into_owned();
        assert_eq!(path("file:///tmp/a:/"), "/tmp/a:");
        assert_eq!(path("file:///Volumes/a:/"), "/Volumes/a:");
        if cfg!(windows) {
            assert_eq!(path("file:///C:/Users/x/Audio/"), r"C:\Users\x\Audio");
            assert_eq!(path("file:///C:/"), r"C:\");
            assert_eq!(path("file:///1:/a"), "/1:/a", "only a letter names a drive");
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

    /// A non-UTF-8 Unix file name round-trips.
    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_round_trip_on_unix() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let path = Path::new(std::ffi::OsStr::from_bytes(
            b"/tmp/caf\xE9/\xFF\xFEname.caf",
        ));
        let url = file_url(path, false);
        assert_eq!(url, "file:///tmp/caf%E9/%FF%FEname.caf");
        assert_eq!(file_url_path(&url).as_deref(), Some(path));
        let bytes = |url: &str| file_url_path(url).unwrap().into_os_string().into_vec();
        assert_eq!(bytes("file:///tmp/%FF/"), b"/tmp/\xFF");
    }

    fn lookup(pairs: Vec<(&str, OsString)>) -> impl Fn(&str) -> Option<OsString> {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn the_support_directory_follows_the_environment() {
        let directory = StenoPaths::support_directory(lookup(vec![
            ("HOME", "/home/nicolai".into()),
            ("XDG_DATA_HOME", "/data".into()),
            ("APPDATA", r"C:\Users\nicolai\AppData\Roaming".into()),
        ]));
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

    /// A relative `XDG_DATA_HOME` is no `XDG_DATA_HOME`.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_relative_path_counts_as_unset() {
        let directory = StenoPaths::support_directory(lookup(vec![
            ("HOME", "/home/nicolai".into()),
            ("XDG_DATA_HOME", "data".into()),
        ]));
        assert_eq!(directory, PathBuf::from("/home/nicolai/.local/share/Steno"));
    }

    /// A support directory path that is not Unicode is used as it is.
    #[test]
    fn a_path_that_is_not_unicode_is_used_as_it_is() {
        #[cfg(unix)]
        let (name, base) = {
            use std::os::unix::ffi::OsStringExt;
            let name = if cfg!(target_os = "macos") {
                "HOME"
            } else {
                "XDG_DATA_HOME"
            };
            (name, OsString::from_vec(b"/home/caf\xE9".to_vec()))
        };
        #[cfg(windows)]
        let (name, base) = {
            use std::os::windows::ffi::OsStringExt;
            let mut wide: Vec<u16> = r"C:\Users\caf".encode_utf16().collect();
            wide.push(0xD800);
            ("APPDATA", OsString::from_wide(&wide))
        };
        assert!(base.to_str().is_none());
        let directory = StenoPaths::support_directory(lookup(vec![(name, base.clone())]));
        let base = PathBuf::from(base);
        let expected = if cfg!(target_os = "macos") {
            base.join("Library").join("Application Support")
        } else {
            base
        };
        assert_eq!(directory, expected.join("Steno"));
    }
}
