//! Bonjour: `_steno._tcp` on the local network with the TXT record the
//! phone reads (`v`, the protocol version; `id`, the computer's id). The
//! Swift listener publishes through `NWListener.Service`; here `mdns-sd`
//! does it. Both answer the phone's `NWBrowser` the same way.

use mdns_sd::{ServiceDaemon, ServiceInfo};
use uuid::Uuid;

use crate::wire;

pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertiser {
    /// The service type in the form `mdns-sd` wants.
    pub const TYPE_DOMAIN: &'static str = "_steno._tcp.local.";

    /// The record for `service_name` on `port`, before it is published.
    pub fn service_info(
        service_name: &str,
        mac_id: Uuid,
        port: u16,
    ) -> mdns_sd::Result<ServiceInfo> {
        let properties = [
            ("v", wire::PROTOCOL_VERSION.to_string()),
            ("id", mac_id.hyphenated().to_string()),
        ];
        let host = format!(
            "{}.local.",
            crate::identity::HandoverIdentity::san_label(service_name).trim_end_matches(".local")
        );
        ServiceInfo::new(
            Self::TYPE_DOMAIN,
            service_name,
            &host,
            "",
            port,
            &properties[..],
        )
        .map(ServiceInfo::enable_addr_auto)
    }

    /// Publishes on every interface the daemon finds.
    pub fn publish(service_name: &str, mac_id: Uuid, port: u16) -> mdns_sd::Result<Self> {
        let info = Self::service_info(service_name, mac_id, port)?;
        let fullname = info.get_fullname().to_owned();
        let daemon = ServiceDaemon::new()?;
        daemon.register(info)?;
        Ok(Advertiser { daemon, fullname })
    }

    /// Withdraws the record and stops the daemon.
    pub fn withdraw(self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_carries_the_version_and_the_id() {
        let mac_id = Uuid::parse_str("0F8FAD5B-D9CB-469F-A165-70867728950E").unwrap();
        let info = Advertiser::service_info("Nicolai's Mac", mac_id, 4242).unwrap();
        assert_eq!(info.get_type(), "_steno._tcp.local.");
        assert_eq!(info.get_fullname(), "Nicolai's Mac._steno._tcp.local.");
        assert_eq!(info.get_port(), 4242);
        assert_eq!(info.get_property_val_str("v"), Some("1"));
        assert_eq!(
            info.get_property_val_str("id"),
            Some("0f8fad5b-d9cb-469f-a165-70867728950e")
        );
        assert!(info.is_addr_auto());
        assert!(info.get_hostname().ends_with(".local."));
    }
}
