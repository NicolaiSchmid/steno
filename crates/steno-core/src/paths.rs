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

/// RFC 3986 unreserved and sub-delims plus `:`, `@` and `/`: the characters
/// Foundation leaves alone in a file URL's path.
fn is_path_allowed(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/".contains(&byte)
}

/// The local path of a `file://` URL as [`file_url`] spells it: the
/// percent-encoding undone, the host part (empty or `localhost`) dropped,
/// a directory's trailing slash dropped (the audio folder reads back as
/// `.../audio`), and on Windows the drive path restored with backslashes.
/// A `%` that is not followed by two hex digits is kept as it is. `None`
/// for any other scheme. Swift: `URL.path` of the stored `mixdownURL` or
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
    let mut decoded = String::from_utf8(bytes).ok()?;
    // A drive root (`/C:/`) keeps its slash: `C:` alone is drive-relative.
    if decoded.len() > 1 && decoded.ends_with('/') && !decoded.ends_with(":/") {
        decoded.pop();
    }
    // `file_url` spelt a Windows path with forward slashes behind a leading
    // `/`; both are undone so the path reads back as the OS spells it.
    #[cfg(windows)]
    let decoded = decoded
        .strip_prefix('/')
        .filter(|rest| rest.as_bytes().get(1) == Some(&b':'))
        .unwrap_or(&decoded)
        .replace('/', "\\");
    Some(PathBuf::from(decoded))
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
        assert_eq!(file_url_path("file:///tmp/%FF.wav"), None, "not UTF-8");
        // A directory reads back without its slash, as `URL.path` does.
        if cfg!(windows) {
            assert_eq!(
                file_url_path("file:///C:/Users/x/Audio/")
                    .unwrap()
                    .to_string_lossy(),
                r"C:\Users\x\Audio"
            );
            assert_eq!(
                file_url_path("file:///C:/").unwrap().to_string_lossy(),
                r"C:\"
            );
        } else {
            assert_eq!(
                file_url_path(&file_url(Path::new("/tmp/a b"), true))
                    .unwrap()
                    .to_string_lossy(),
                "/tmp/a b"
            );
            assert_eq!(file_url_path("file:///"), Some(PathBuf::from("/")));
        }
        assert_eq!(file_url_path("https://example.com/x"), None);
        assert_eq!(file_url_path("file://host-only"), None);
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
