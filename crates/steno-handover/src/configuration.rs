//! How the listener binds, where partial uploads live, and the clock the
//! service runs on.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use steno_core::StenoPaths;

/// The one time source: it stamps receipts and devices and decides when the
/// pairing window has closed. Tests advance it.
pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// How the handover listens and where it keeps partial uploads. Tests use
/// `advertise: false`, so nothing leaves 127.0.0.1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoverConfiguration {
    /// Bonjour instance name; the phone shows it. Defaults to
    /// [`HandoverConfiguration::default_service_name`].
    pub service_name: String,
    /// Publish `_steno._tcp` and serve on the LAN addresses and loopback;
    /// `false` binds loopback only.
    pub advertise: bool,
    /// The largest chunk the computer accepts; a phone's declared chunk size
    /// must not exceed it. 16 MiB in the product.
    pub chunk_size: i64,
    /// Where partial uploads and their metadata live until the intake takes
    /// the finished file.
    pub inbox_directory: PathBuf,
    /// How long a pairing QR code stays valid on the injected clock.
    pub pairing_window: Duration,
    /// The port the listener binds; `0` lets the system choose. A fixed port
    /// that is taken gives way to a system-chosen one with a warning; the port
    /// bound is what Bonjour publishes.
    pub port: u16,
    /// How long a connection may stay silent while the computer waits for the
    /// client before it is closed; not counted while a request is handled.
    pub read_timeout: Duration,
}

impl HandoverConfiguration {
    pub const DEFAULT_CHUNK_SIZE: i64 = 16 * 1024 * 1024;
    /// Headroom over the chunk size for the request body limit.
    pub const BODY_HEADROOM: i64 = 64 * 1024;
    /// Upper bound for the JSON bodies of the small routes.
    pub const JSON_BODY_LIMIT: i64 = 64 * 1024;
    /// The inbox's directory under the support directory. Every build uses this
    /// name, so a phone resumes its partial uploads after an upgrade.
    pub const INBOX_DIRECTORY_NAME: &str = "handover-inbox";
    /// The port the app listens on under Linux, where the desktop firewalls
    /// block every port not opened by name; the AUR `ufw` profile and the NixOS
    /// module open this one. Unassigned by IANA and below Linux's ephemeral
    /// range.
    pub const LINUX_PORT: u16 = 23820;

    /// The port the app listens on unless the user chose one:
    /// [`HandoverConfiguration::LINUX_PORT`] on Linux, `0` elsewhere.
    #[must_use]
    pub const fn platform_port() -> u16 {
        if cfg!(target_os = "linux") {
            Self::LINUX_PORT
        } else {
            0
        }
    }

    /// Body limit for chunk uploads: the chunk size plus 64 KiB.
    #[must_use]
    pub fn chunk_body_limit(&self) -> i64 {
        self.chunk_size + Self::BODY_HEADROOM
    }

    /// The name the phone shows: the first of [`NAME_SOURCES`] that names the
    /// computer, else `Steno`. On the Mac that is the computer name from System
    /// Settings; elsewhere the host name.
    #[must_use]
    pub fn default_service_name() -> String {
        first_name(NAME_SOURCES, NameSource::read)
    }

    /// `<support>/`[`INBOX_DIRECTORY_NAME`](Self::INBOX_DIRECTORY_NAME), rooted
    /// by [`StenoPaths`] so a `HOME` override in tests applies.
    #[must_use]
    pub fn default_inbox_directory() -> PathBuf {
        StenoPaths::default_support_directory().join(Self::INBOX_DIRECTORY_NAME)
    }
}

/// Where [`HandoverConfiguration::default_service_name`] looks, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameSource {
    /// The computer name in System Settings (`SCDynamicStoreCopyComputerName`
    /// through `whoami::devicename`); macOS only.
    ComputerName,
    /// The `HOSTNAME` environment variable.
    HostnameVariable,
    /// `/etc/hostname`, trimmed.
    EtcHostname,
    /// The system's host name (`gethostname`; `GetComputerNameExW` on
    /// Windows) through `whoami::hostname`.
    SystemHostname,
}

impl NameSource {
    /// The name this source gives; `None` when it gives none or an empty one.
    #[must_use]
    pub fn read(self) -> Option<String> {
        let name = match self {
            NameSource::ComputerName if cfg!(target_os = "macos") => whoami::devicename().ok(),
            NameSource::ComputerName => None,
            NameSource::HostnameVariable => std::env::var("HOSTNAME").ok(),
            NameSource::EtcHostname => std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|name| name.trim().to_owned()),
            NameSource::SystemHostname => whoami::hostname().ok(),
        };
        name.filter(|name| !name.is_empty())
    }
}

/// The first name `read` gives for `sources`, in order; `Steno` when none
/// gives one.
fn first_name(sources: &[NameSource], read: impl Fn(NameSource) -> Option<String>) -> String {
    sources
        .iter()
        .find_map(|&source| read(source))
        .unwrap_or_else(|| "Steno".to_owned())
}

/// The sources of the service name on this platform, in order: the Mac asks
/// for the computer name first, then the host name sources.
pub const NAME_SOURCES: &[NameSource] = if cfg!(target_os = "macos") {
    &[
        NameSource::ComputerName,
        NameSource::HostnameVariable,
        NameSource::EtcHostname,
        NameSource::SystemHostname,
    ]
} else {
    &[
        NameSource::HostnameVariable,
        NameSource::EtcHostname,
        NameSource::SystemHostname,
    ]
};

impl Default for HandoverConfiguration {
    fn default() -> Self {
        HandoverConfiguration {
            service_name: Self::default_service_name(),
            advertise: true,
            chunk_size: Self::DEFAULT_CHUNK_SIZE,
            inbox_directory: Self::default_inbox_directory(),
            pairing_window: Duration::from_secs(300),
            port: 0,
            read_timeout: Duration::from_secs(30),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_name_is_the_first_a_source_gives_in_order() {
        let sources = [
            NameSource::ComputerName,
            NameSource::HostnameVariable,
            NameSource::EtcHostname,
            NameSource::SystemHostname,
        ];
        let reader = |names: [Option<&'static str>; 4]| {
            move |source: NameSource| {
                let index = sources.iter().position(|&known| known == source).unwrap();
                names[index].map(str::to_owned)
            }
        };
        let cases = [
            (
                [Some("Studio"), Some("env"), Some("etc"), Some("host")],
                "Studio",
            ),
            ([None, None, Some("etc"), Some("host")], "etc"),
            ([None, None, None, Some("host")], "host"),
            ([None; 4], "Steno"),
        ];
        for (names, expected) in cases {
            assert_eq!(first_name(&sources, reader(names)), expected);
        }
        let expected = if cfg!(target_os = "macos") {
            &sources[..]
        } else {
            &sources[1..]
        };
        assert_eq!(NAME_SOURCES, expected, "the computer name on the Mac only");
        assert!(NameSource::SystemHostname.read().is_some());
    }
}
