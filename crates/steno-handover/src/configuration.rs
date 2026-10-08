//! How the listener binds and where partial uploads live, and the clock
//! the service runs on. Swift: `HandoverConfiguration.swift`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use steno_core::StenoPaths;

/// The one time source: it stamps receipts and devices and decides when the
/// pairing window has closed. Tests advance it.
pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// How the computer's side of the handover listens and where it keeps
/// partial uploads. Tests use `advertise: false` and a fresh temporary
/// inbox, so nothing leaves 127.0.0.1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoverConfiguration {
    /// Bonjour instance name; the phone shows it. Defaults to
    /// [`HandoverConfiguration::default_service_name`].
    pub service_name: String,
    /// Publish `_steno._tcp` and serve on the LAN addresses
    /// ([`server::advertise::current_lan_addresses`](crate::server::advertise::current_lan_addresses))
    /// and loopback; `false` binds loopback only.
    pub advertise: bool,
    /// The largest chunk the computer accepts; the phone declares its own
    /// chunk size per recording and it must not exceed this. 16 MiB in the
    /// product.
    pub chunk_size: i64,
    /// Where partial uploads and their metadata live until the intake takes
    /// the finished file.
    pub inbox_directory: PathBuf,
    /// How long a pairing QR code stays valid on the injected clock.
    pub pairing_window: Duration,
    /// `0` lets the system choose; the port is published through Bonjour.
    pub port: u16,
    /// How long a connection may stay silent while the computer waits for
    /// the client (a request line, the rest of a body, the next request)
    /// before it is closed. Not counted while the engine is handling a
    /// request.
    pub read_timeout: Duration,
}

impl HandoverConfiguration {
    pub const DEFAULT_CHUNK_SIZE: i64 = 16 * 1024 * 1024;
    /// Headroom over the chunk size for the request body limit.
    pub const BODY_HEADROOM: i64 = 64 * 1024;
    /// Upper bound for the JSON bodies of the small routes.
    pub const JSON_BODY_LIMIT: i64 = 64 * 1024;

    /// Body limit for chunk uploads: the chunk size plus 64 KiB.
    #[must_use]
    pub fn chunk_body_limit(&self) -> i64 {
        self.chunk_size + Self::BODY_HEADROOM
    }

    /// The name the phone shows: the first of [`NAME_SOURCES`] that names
    /// the computer, else `Steno`. On the Mac that is the computer name
    /// from System Settings, the name Swift's `defaultServiceName` reads
    /// (`Host.current().localizedName`), so a phone shows the same name
    /// after the handoff; on Linux and Windows the host name.
    /// Swift: `HandoverConfiguration.defaultServiceName`.
    #[must_use]
    pub fn default_service_name() -> String {
        first_name(NAME_SOURCES, NameSource::read)
    }

    /// `<support>/handover-inbox`; [`StenoPaths`] decides the root so a
    /// `HOME` override in tests applies here too.
    #[must_use]
    pub fn default_inbox_directory() -> PathBuf {
        StenoPaths::default_support_directory().join("handover-inbox")
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
    /// The name this source gives; `None` when it gives none or an empty
    /// one.
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

/// The sources of the service name on this platform, in order: the Mac
/// asks for the computer name first, as Swift did; Linux and Windows use
/// the host name. The host name sources are the Mac's fallback, as
/// Swift's `ProcessInfo.processInfo.hostName` was.
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

    /// The Mac's sources; Linux and Windows use all but the first.
    const MAC_SOURCES: [NameSource; 4] = [
        NameSource::ComputerName,
        NameSource::HostnameVariable,
        NameSource::EtcHostname,
        NameSource::SystemHostname,
    ];

    #[test]
    fn the_mac_asks_for_the_computer_name_first_and_linux_and_windows_for_the_host_name() {
        if cfg!(target_os = "macos") {
            assert_eq!(NAME_SOURCES, &MAC_SOURCES[..]);
        } else {
            assert_eq!(NAME_SOURCES, &MAC_SOURCES[1..]);
            assert_eq!(
                NameSource::ComputerName.read(),
                None,
                "no computer name here"
            );
        }
        assert!(
            NameSource::SystemHostname.read().is_some(),
            "every platform has a host name"
        );
    }

    #[test]
    fn the_default_is_the_first_name_a_source_gives_in_order() {
        let reader = |names: [Option<&'static str>; 4]| {
            move |source: NameSource| {
                let index = MAC_SOURCES
                    .iter()
                    .position(|&known| known == source)
                    .unwrap();
                names[index].map(str::to_owned)
            }
        };
        let all = reader([Some("Studio"), Some("env"), Some("etc"), Some("host")]);
        assert_eq!(first_name(&MAC_SOURCES, all), "Studio");
        assert_eq!(first_name(&MAC_SOURCES[1..], all), "env");
        assert_eq!(
            first_name(
                &MAC_SOURCES,
                reader([None, None, Some("etc"), Some("host")])
            ),
            "etc"
        );
        assert_eq!(
            first_name(&MAC_SOURCES, reader([None, None, None, Some("host")])),
            "host"
        );
        assert_eq!(first_name(&MAC_SOURCES, reader([None; 4])), "Steno");
    }

    /// The name Swift published: `scutil --get ComputerName` reads the
    /// same `SCDynamicStoreCopyComputerName` as `Host.current().localizedName`.
    #[cfg(target_os = "macos")]
    #[test]
    fn on_the_mac_the_computer_name_is_the_one_system_settings_shows() {
        let output = std::process::Command::new("/usr/sbin/scutil")
            .args(["--get", "ComputerName"])
            .output()
            .expect("scutil runs");
        let shown = String::from_utf8(output.stdout).unwrap();
        let shown = shown.strip_suffix('\n').unwrap_or(&shown);
        if !output.status.success() || shown.is_empty() {
            assert_eq!(NameSource::ComputerName.read(), None);
            return;
        }
        assert_eq!(NameSource::ComputerName.read().as_deref(), Some(shown));
        assert_eq!(HandoverConfiguration::default_service_name(), shown);
    }
}
