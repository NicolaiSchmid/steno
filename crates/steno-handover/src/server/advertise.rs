//! Bonjour: `_steno._tcp` on the local network with the TXT record the
//! phone reads (`v`, the protocol version; `id`, the computer's id). The
//! Swift listener publishes through `NWListener.Service`; here `mdns-sd`
//! does it. Both answer the phone's `NWBrowser` the same way.
//!
//! Which network: the Swift listener sets `prohibitedInterfaceTypes =
//! [.cellular, .other]`, so the service lives on Wi-Fi and wired Ethernet
//! and never on a VPN tunnel. Here [`lan_addresses`] picks the IPv4
//! addresses of the interfaces that are up, not loopback, not link-local
//! and not point-to-point (the tunnels); the record carries those
//! addresses and the listener accepts connections on those addresses and
//! on loopback only. The addresses are read when the service starts and
//! again for each connection; the record is not re-published when the
//! computer changes network, where `NWListener` follows the change.

use std::net::{IpAddr, Ipv4Addr};

use if_addrs::{IfAddr, Interface};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use uuid::Uuid;

use crate::identity::HandoverIdentity;
use crate::wire;

/// The IPv4 addresses of the interfaces the service may use: up, not
/// loopback, not link-local, not point-to-point. Sorted, without
/// duplicates.
pub fn lan_addresses<'a>(interfaces: impl IntoIterator<Item = &'a Interface>) -> Vec<Ipv4Addr> {
    let mut addresses: Vec<Ipv4Addr> = interfaces
        .into_iter()
        .filter(|interface| {
            interface.is_oper_up()
                && !interface.is_p2p()
                && !interface.is_loopback()
                && !interface.is_link_local()
        })
        .filter_map(|interface| match &interface.addr {
            IfAddr::V4(v4) => Some(v4.ip),
            IfAddr::V6(_) => None,
        })
        .collect();
    addresses.sort_unstable();
    addresses.dedup();
    addresses
}

/// [`lan_addresses`] of the computer now; empty when the system does not
/// say.
#[must_use]
pub fn current_lan_addresses() -> Vec<Ipv4Addr> {
    if_addrs::get_if_addrs().map_or_else(|_| Vec::new(), |interfaces| lan_addresses(&interfaces))
}

/// Whether a connection that arrived on `local` (the listener's side) is
/// on a network the service lives on: loopback, or one of `lan`.
#[must_use]
pub fn accepts_local_address(local: IpAddr, lan: &[Ipv4Addr]) -> bool {
    match local {
        IpAddr::V4(local) => local.is_loopback() || lan.contains(&local),
        IpAddr::V6(local) => local.is_loopback(),
    }
}

pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertiser {
    /// The service type in the form `mdns-sd` wants.
    pub const TYPE_DOMAIN: &'static str = "_steno._tcp.local.";

    /// The record for `service_name` on `port` at `addresses`, before it
    /// is published.
    pub fn service_info(
        service_name: &str,
        mac_id: Uuid,
        port: u16,
        addresses: &[Ipv4Addr],
    ) -> mdns_sd::Result<ServiceInfo> {
        let properties = [
            ("v", wire::PROTOCOL_VERSION.to_string()),
            ("id", mac_id.hyphenated().to_string()),
        ];
        let host = format!("{}.", HandoverIdentity::san_label(service_name));
        let addresses: Vec<IpAddr> = addresses.iter().copied().map(IpAddr::V4).collect();
        ServiceInfo::new(
            Self::TYPE_DOMAIN,
            service_name,
            &host,
            &addresses[..],
            port,
            &properties[..],
        )
    }

    /// Publishes the record with `addresses`. An empty list publishes a
    /// record no phone can resolve; the caller logs that.
    pub fn publish(
        service_name: &str,
        mac_id: Uuid,
        port: u16,
        addresses: &[Ipv4Addr],
    ) -> mdns_sd::Result<Self> {
        let info = Self::service_info(service_name, mac_id, port, addresses)?;
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
    use if_addrs::{IfOperStatus, Ifv4Addr, Ifv6Addr};

    use super::*;

    fn v4(name: &str, ip: [u8; 4], status: IfOperStatus, p2p: bool) -> Interface {
        Interface {
            name: name.to_owned(),
            addr: IfAddr::V4(Ifv4Addr {
                ip: Ipv4Addr::from(ip),
                netmask: Ipv4Addr::new(255, 255, 255, 0),
                prefixlen: 24,
                broadcast: None,
            }),
            index: Some(1),
            oper_status: status,
            is_p2p: p2p,
            #[cfg(windows)]
            adapter_name: String::new(),
        }
    }

    #[test]
    fn the_record_carries_the_version_the_id_and_the_addresses() {
        let mac_id = Uuid::parse_str("0F8FAD5B-D9CB-469F-A165-70867728950E").unwrap();
        let address = Ipv4Addr::new(192, 168, 1, 20);
        let info = Advertiser::service_info("Nicolai's Mac", mac_id, 4242, &[address]).unwrap();
        assert_eq!(info.get_type(), "_steno._tcp.local.");
        assert_eq!(info.get_fullname(), "Nicolai's Mac._steno._tcp.local.");
        assert_eq!(info.get_port(), 4242);
        assert_eq!(info.get_property_val_str("v"), Some("1"));
        assert_eq!(
            info.get_property_val_str("id"),
            Some("0f8fad5b-d9cb-469f-a165-70867728950e")
        );
        assert!(!info.is_addr_auto(), "the addresses are chosen here");
        assert_eq!(
            info.get_addresses_v4().into_iter().collect::<Vec<_>>(),
            vec![&address]
        );
        assert!(info.get_hostname().ends_with(".local."));
    }

    #[test]
    fn only_the_lan_interfaces_qualify() {
        let interfaces = vec![
            v4("lo0", [127, 0, 0, 1], IfOperStatus::Up, false),
            v4("en0", [192, 168, 1, 20], IfOperStatus::Up, false),
            v4("en1", [10, 0, 0, 5], IfOperStatus::Up, false),
            v4("en1", [10, 0, 0, 5], IfOperStatus::Up, false),
            v4("en2", [10, 0, 1, 5], IfOperStatus::Down, false),
            v4("en3", [169, 254, 7, 7], IfOperStatus::Up, false),
            v4("utun3", [100, 64, 0, 2], IfOperStatus::Up, true),
            Interface {
                name: "en0".to_owned(),
                addr: IfAddr::V6(Ifv6Addr {
                    ip: "fe80::1".parse().unwrap(),
                    netmask: "ffff:ffff:ffff:ffff::".parse().unwrap(),
                    prefixlen: 64,
                    broadcast: None,
                }),
                index: Some(2),
                oper_status: IfOperStatus::Up,
                is_p2p: false,
                #[cfg(windows)]
                adapter_name: String::new(),
            },
        ];
        assert_eq!(
            lan_addresses(&interfaces),
            vec![Ipv4Addr::new(10, 0, 0, 5), Ipv4Addr::new(192, 168, 1, 20)]
        );
        assert_eq!(lan_addresses(&[]), Vec::<Ipv4Addr>::new());
    }

    #[test]
    fn loopback_and_the_lan_addresses_are_accepted_a_tunnel_is_not() {
        let lan = vec![Ipv4Addr::new(192, 168, 1, 20)];
        assert!(accepts_local_address("127.0.0.1".parse().unwrap(), &lan));
        assert!(accepts_local_address("::1".parse().unwrap(), &lan));
        assert!(accepts_local_address("192.168.1.20".parse().unwrap(), &lan));
        assert!(!accepts_local_address("100.64.0.2".parse().unwrap(), &lan));
        assert!(!accepts_local_address("192.168.1.20".parse().unwrap(), &[]));
    }
}
