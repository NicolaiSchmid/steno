//! Bonjour: `_steno._tcp` on the local network with the TXT record the
//! phone reads (`v`, the protocol version; `id`, the computer's id). The
//! Swift listener publishes through `NWListener.Service`; here `mdns-sd`
//! does it. Both answer the phone's `NWBrowser` the same way.
//!
//! Which network: the Swift listener sets `prohibitedInterfaceTypes =
//! [.cellular, .other]`, so the service lives on Wi-Fi and wired Ethernet
//! and never on a VPN tunnel. Here [`lan_addresses`] keeps the IPv4
//! addresses of the interfaces that are up and not loopback, link-local or
//! point-to-point, and never one in `100.64.0.0/10`, the shared address
//! space Tailscale numbers its tunnel from. On Linux and macOS every tunnel
//! interface (`wg0`, `tailscale0`, `tun0`, `utun3`) is point-to-point.
//! Windows reports only PPP and its own tunnels so, and a Wintun, TAP or
//! Hyper-V adapter there looks like Ethernet; on Windows an address also
//! needs an adapter that is Ethernet or Wi-Fi, hardware, and up
//! ([`windows_keeps`]), the counterpart of Swift's "not `.other`". The
//! record carries those addresses, and the listener accepts connections on
//! them and on loopback only. The addresses are read when the service
//! starts and again, at most once a second, while it accepts; the record is
//! not re-published when the computer changes network, whereas
//! `NWListener` follows the change. Swift: `Network/HandoverServer.swift`.

use std::net::{IpAddr, Ipv4Addr};

use if_addrs::{IfAddr, Interface};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use uuid::Uuid;

use crate::identity::HandoverIdentity;
use crate::wire;

/// The IPv4 addresses of the interfaces the service may use: up, not
/// loopback, not link-local, not point-to-point, not in `100.64.0.0/10`.
/// Sorted, without duplicates. This is the whole rule on Linux and macOS;
/// Windows adds [`windows_keeps`] ([`windows_lan_addresses`]).
pub fn lan_addresses<'a>(interfaces: impl IntoIterator<Item = &'a Interface>) -> Vec<Ipv4Addr> {
    lan_addresses_where(interfaces, |_, _| true)
}

/// [`lan_addresses`] with the Windows adapter rule on top: `adapter` gives
/// the facts of the adapter with an interface index, and an address whose
/// adapter has none is dropped.
pub fn windows_lan_addresses<'a>(
    interfaces: impl IntoIterator<Item = &'a Interface>,
    adapter: impl Fn(u32) -> Option<WindowsAdapter>,
) -> Vec<Ipv4Addr> {
    lan_addresses_where(interfaces, |interface, address| {
        interface
            .index
            .and_then(&adapter)
            .is_some_and(|adapter| windows_keeps(adapter, address))
    })
}

fn lan_addresses_where<'a>(
    interfaces: impl IntoIterator<Item = &'a Interface>,
    keep: impl Fn(&Interface, Ipv4Addr) -> bool,
) -> Vec<Ipv4Addr> {
    let mut addresses: Vec<Ipv4Addr> = interfaces
        .into_iter()
        .filter(|interface| interface.is_oper_up() && !interface.is_p2p())
        .filter_map(|interface| match &interface.addr {
            IfAddr::V4(v4) => Some((interface, v4.ip)),
            IfAddr::V6(_) => None,
        })
        .filter(|&(interface, address)| is_lan_address(address) && keep(interface, address))
        .map(|(_, address)| address)
        .collect();
    addresses.sort_unstable();
    addresses.dedup();
    addresses
}

/// Not loopback, not link-local, not unspecified, not in the shared
/// address space `100.64.0.0/10` (RFC 6598), where a carrier-grade NAT or a
/// Tailscale tunnel lives and a home or office network does not.
fn is_lan_address(address: Ipv4Addr) -> bool {
    let [first, second, ..] = address.octets();
    let shared = first == 100 && second & 0b1100_0000 == 64;
    !address.is_loopback() && !address.is_link_local() && !address.is_unspecified() && !shared
}

/// What the Windows rule reads of an adapter (`MIB_IF_ROW2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowsAdapter {
    /// `Type`, the IANA interface type.
    pub if_type: u32,
    /// `InterfaceAndOperStatusFlags.HardwareInterface`: a physical adapter,
    /// not a Wintun, TAP or Hyper-V virtual one.
    pub hardware: bool,
    /// `OperStatus` is `IfOperStatusUp`.
    pub oper_up: bool,
}

impl WindowsAdapter {
    /// `IF_TYPE_ETHERNET_CSMACD`.
    pub const ETHERNET: u32 = 6;
    /// `IF_TYPE_IEEE80211`.
    pub const WIFI: u32 = 71;
}

/// Whether `address` on `adapter` is a LAN address on Windows: the
/// adapter is Ethernet or Wi-Fi, hardware, and up, and the address passes
/// the rule of every platform. Swift's "wired or Wi-Fi, not `.other`".
#[must_use]
pub fn windows_keeps(adapter: WindowsAdapter, address: Ipv4Addr) -> bool {
    matches!(
        adapter.if_type,
        WindowsAdapter::ETHERNET | WindowsAdapter::WIFI
    ) && adapter.hardware
        && adapter.oper_up
        && is_lan_address(address)
}

/// [`lan_addresses`] of the computer now (on Windows,
/// [`windows_lan_addresses`]); empty when the system does not say, which
/// refuses every connection but loopback.
#[must_use]
pub fn current_lan_addresses() -> Vec<Ipv4Addr> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    #[cfg(windows)]
    {
        let Some(adapters) = windows_adapters::by_index() else {
            return Vec::new();
        };
        windows_lan_addresses(&interfaces, |index| adapters.get(&index).copied())
    }
    #[cfg(not(windows))]
    lan_addresses(&interfaces)
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

/// The adapter table through `GetIfTable2`, by interface index (the
/// `IfIndex` if-addrs reports for an IPv4 address).
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_adapters {
    use std::collections::BTreeMap;

    use windows_sys::Win32::Foundation::NO_ERROR;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        FreeMibTable, GetIfTable2, MIB_IF_ROW2, MIB_IF_TABLE2,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;

    use super::WindowsAdapter;

    /// Bit 0 of `InterfaceAndOperStatusFlags`.
    const HARDWARE_INTERFACE: u8 = 1;

    /// The table `GetIfTable2` allocated, released when dropped.
    struct Table(*mut MIB_IF_TABLE2);

    impl Drop for Table {
        fn drop(&mut self) {
            // SAFETY: the pointer is the non-null table of a `GetIfTable2`
            // that returned `NO_ERROR`, and this is its one release.
            unsafe { FreeMibTable(self.0.cast()) };
        }
    }

    /// Every adapter's facts; `None` when the system does not say.
    pub(super) fn by_index() -> Option<BTreeMap<u32, WindowsAdapter>> {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        // SAFETY: `GetIfTable2` writes one pointer through a pointer to a
        // live local.
        if unsafe { GetIfTable2(&raw mut table) } != NO_ERROR || table.is_null() {
            return None;
        }
        let table = Table(table);
        // SAFETY: on `NO_ERROR` the pointer is a live, aligned
        // `MIB_IF_TABLE2` the system allocated; only the count is read.
        let count = usize::try_from(unsafe { (*table.0).NumEntries }).unwrap_or(0);
        // SAFETY: the same table. `&raw const` makes no reference, so the
        // pointer keeps the provenance of the whole allocation, which holds
        // `NumEntries` rows past the one the declared `[MIB_IF_ROW2; 1]`
        // covers.
        let first = unsafe { &raw const (*table.0).Table }.cast::<MIB_IF_ROW2>();
        // SAFETY: the system wrote `count` initialised rows from `first`,
        // aligned as the table's `Table` field is; the slice is dropped
        // before `table` releases them.
        let rows = unsafe { std::slice::from_raw_parts(first, count) };
        let adapters = rows
            .iter()
            .map(|row| {
                let adapter = WindowsAdapter {
                    if_type: row.Type,
                    hardware: row.InterfaceAndOperStatusFlags._bitfield & HARDWARE_INTERFACE != 0,
                    oper_up: row.OperStatus == IfOperStatusUp,
                };
                (row.InterfaceIndex, adapter)
            })
            .collect();
        Some(adapters)
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
        v4_at(name, 1, ip, status, p2p)
    }

    fn v4_at(name: &str, index: u32, ip: [u8; 4], status: IfOperStatus, p2p: bool) -> Interface {
        Interface {
            name: name.to_owned(),
            addr: IfAddr::V4(Ifv4Addr {
                ip: Ipv4Addr::from(ip),
                netmask: Ipv4Addr::new(255, 255, 255, 0),
                prefixlen: 24,
                broadcast: None,
            }),
            index: Some(index),
            oper_status: status,
            is_p2p: p2p,
            #[cfg(windows)]
            adapter_name: String::new(),
        }
    }

    fn adapter(if_type: u32, hardware: bool, oper_up: bool) -> WindowsAdapter {
        WindowsAdapter {
            if_type,
            hardware,
            oper_up,
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
            v4("utun3", [10, 8, 0, 2], IfOperStatus::Up, true),
            v4("tailscale0", [100, 101, 7, 9], IfOperStatus::Up, false),
            v4("en4", [100, 127, 255, 1], IfOperStatus::Up, false),
            v4("en6", [100, 63, 255, 255], IfOperStatus::Up, false),
            v4("en5", [100, 128, 0, 1], IfOperStatus::Up, false),
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
            vec![
                Ipv4Addr::new(10, 0, 0, 5),
                Ipv4Addr::new(100, 63, 255, 255),
                Ipv4Addr::new(100, 128, 0, 1),
                Ipv4Addr::new(192, 168, 1, 20)
            ]
        );
        assert_eq!(lan_addresses(&[]), Vec::<Ipv4Addr>::new());
    }

    #[test]
    fn on_windows_only_a_hardware_ethernet_or_wifi_adapter_that_is_up_qualifies() {
        let lan = Ipv4Addr::new(192, 168, 1, 20);
        let cases = [
            ("Ethernet", adapter(6, true, true), lan, true),
            ("Wi-Fi", adapter(71, true, true), lan, true),
            ("Ethernet, down", adapter(6, true, false), lan, false),
            (
                "type 53 marked hardware",
                adapter(53, true, true),
                lan,
                false,
            ),
            (
                "Wintun",
                adapter(53, false, true),
                Ipv4Addr::new(10, 6, 0, 2),
                false,
            ),
            (
                "TAP, type 6",
                adapter(6, false, true),
                Ipv4Addr::new(10, 8, 0, 6),
                false,
            ),
            (
                "Hyper-V vEthernet",
                adapter(6, false, true),
                Ipv4Addr::new(172, 20, 16, 1),
                false,
            ),
            (
                "PPP",
                adapter(23, true, true),
                Ipv4Addr::new(10, 64, 0, 3),
                false,
            ),
            (
                "loopback",
                adapter(24, false, true),
                Ipv4Addr::LOCALHOST,
                false,
            ),
            (
                "loopback address on Ethernet",
                adapter(6, true, true),
                Ipv4Addr::LOCALHOST,
                false,
            ),
            (
                "link-local on Wi-Fi",
                adapter(71, true, true),
                Ipv4Addr::new(169, 254, 3, 4),
                false,
            ),
            (
                "100.64/10 on Ethernet",
                adapter(6, true, true),
                Ipv4Addr::new(100, 64, 0, 10),
                false,
            ),
        ];
        for (what, adapter, address, kept) in cases {
            assert_eq!(windows_keeps(adapter, address), kept, "{what}");
        }
    }

    #[test]
    fn on_windows_each_address_is_judged_by_its_own_adapter() {
        let interfaces = vec![
            v4_at("Ethernet", 4, [192, 168, 1, 3], IfOperStatus::Up, false),
            v4_at("Wi-Fi", 7, [10, 0, 0, 9], IfOperStatus::Up, false),
            v4_at("Tailscale", 12, [100, 64, 0, 10], IfOperStatus::Up, false),
            v4_at("TAP", 15, [10, 8, 0, 6], IfOperStatus::Up, false),
            v4_at("vEthernet", 21, [172, 20, 16, 1], IfOperStatus::Up, false),
            v4_at("Unknown", 30, [192, 168, 2, 3], IfOperStatus::Up, false),
        ];
        let adapters = std::collections::BTreeMap::from([
            (4, adapter(6, true, true)),
            (7, adapter(71, true, true)),
            (12, adapter(53, false, true)),
            (15, adapter(6, false, true)),
            (21, adapter(6, false, true)),
        ]);
        assert_eq!(
            windows_lan_addresses(&interfaces, |index| adapters.get(&index).copied()),
            vec![Ipv4Addr::new(10, 0, 0, 9), Ipv4Addr::new(192, 168, 1, 3)]
        );
    }

    #[test]
    fn the_system_lan_addresses_can_be_read() {
        let lan = current_lan_addresses();
        println!("lan_addresses: {lan:?}");
        assert!(
            lan.iter().all(|address| is_lan_address(*address)),
            "{lan:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn on_windows_the_adapter_table_has_a_row_for_every_ipv4_interface() {
        /// `IF_TYPE_SOFTWARE_LOOPBACK`.
        const LOOPBACK: u32 = 24;
        let adapters = windows_adapters::by_index().expect("GetIfTable2 answers");
        let loopback: Vec<_> = adapters
            .values()
            .filter(|adapter| adapter.if_type == LOOPBACK)
            .collect();
        assert!(!loopback.is_empty(), "a loopback row: {adapters:?}");
        for adapter in loopback {
            assert!(!adapter.hardware && adapter.oper_up, "{adapter:?}");
        }
        for interface in if_addrs::get_if_addrs().unwrap() {
            if let (IfAddr::V4(_), Some(index)) = (&interface.addr, interface.index)
                && index != 0
            {
                assert!(
                    adapters.contains_key(&index),
                    "{} at index {index}: {adapters:?}",
                    interface.name
                );
            }
        }
    }

    #[test]
    fn loopback_and_the_lan_addresses_are_accepted_a_tunnel_is_not() {
        let lan = vec![Ipv4Addr::new(192, 168, 1, 20)];
        assert!(accepts_local_address("127.0.0.1".parse().unwrap(), &lan));
        assert!(accepts_local_address("::1".parse().unwrap(), &lan));
        assert!(accepts_local_address("192.168.1.20".parse().unwrap(), &lan));
        assert!(!accepts_local_address("100.64.0.2".parse().unwrap(), &lan));
        assert!(!accepts_local_address("10.8.0.6".parse().unwrap(), &lan));
        assert!(!accepts_local_address("192.168.1.20".parse().unwrap(), &[]));
    }
}
