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

    /// The host name from `HOSTNAME` or `/etc/hostname`; `Steno` elsewhere,
    /// which the shell replaces with the OS computer name.
    #[must_use]
    pub fn default_service_name() -> String {
        std::env::var("HOSTNAME")
            .ok()
            .filter(|name| !name.is_empty())
            .or_else(|| {
                std::fs::read_to_string("/etc/hostname")
                    .ok()
                    .map(|name| name.trim().to_owned())
                    .filter(|name| !name.is_empty())
            })
            .unwrap_or_else(|| "Steno".to_owned())
    }

    /// `<support>/handover-inbox`; [`StenoPaths`] decides the root so a
    /// `HOME` override in tests applies here too.
    #[must_use]
    pub fn default_inbox_directory() -> PathBuf {
        StenoPaths::default_support_directory().join("handover-inbox")
    }
}

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
