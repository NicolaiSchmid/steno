//! Bonjour: `_steno._tcp` on the LAN with the TXT record the phone reads
//! (`v`, the protocol version; `id`, the computer's id), published through
//! `mdns-sd`. The service lives on Wi-Fi and wired Ethernet only, never on a
//! VPN tunnel ([`lan_addresses`]); the listener accepts connections on those
//! addresses and on loopback only, and the record follows them as they change.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use if_addrs::{IfAddr, Interface};
use mdns_sd::{DaemonEvent, RecvTimeoutError, ServiceDaemon, ServiceInfo};
use uuid::Uuid;

use crate::identity::HandoverIdentity;
use crate::wire;

/// The IPv4 addresses the service may use: up, not loopback, link-local or
/// point-to-point, not in `100.64.0.0/10`. Sorted, without duplicates.
/// Windows adds [`windows_keeps`], since its virtual adapters look like
/// Ethernet.
pub fn lan_addresses<'a>(interfaces: impl IntoIterator<Item = &'a Interface>) -> Vec<Ipv4Addr> {
    lan_addresses_where(interfaces, |_, _| true)
}

/// [`lan_addresses`] with the Windows adapter rule on top; an address whose
/// adapter `adapter` does not know is dropped.
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

/// Not loopback, link-local or unspecified, and not in the shared address
/// space `100.64.0.0/10` (RFC 6598) where carrier-grade NAT and mesh VPNs
/// live.
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
    /// `InterfaceAndOperStatusFlags.HardwareInterface`: a physical adapter, not
    /// a Wintun, TAP or Hyper-V one.
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

/// Whether `address` on `adapter` is a LAN address on Windows: an Ethernet
/// or Wi-Fi hardware adapter that is up, and an address that passes the
/// common rule.
#[must_use]
pub fn windows_keeps(adapter: WindowsAdapter, address: Ipv4Addr) -> bool {
    matches!(
        adapter.if_type,
        WindowsAdapter::ETHERNET | WindowsAdapter::WIFI
    ) && adapter.hardware
        && adapter.oper_up
        && is_lan_address(address)
}

/// [`lan_addresses`] of the computer now; empty when the system does not
/// say, which refuses every connection but loopback.
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

/// Whether a connection that arrived on `local` is on a network the service
/// lives on: loopback, or one of `lan`.
#[must_use]
pub fn accepts_local_address(local: IpAddr, lan: &[Ipv4Addr]) -> bool {
    match local {
        IpAddr::V4(local) => local.is_loopback() || lan.contains(&local),
        IpAddr::V6(local) => local.is_loopback(),
    }
}

/// The adapter table through `GetIfTable2`, by interface index.
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

/// How often the daemon reads the interfaces, in seconds. The phone waits
/// 15 s before it backs off, so 5 s (`mdns-sd`'s default, pinned here)
/// costs no phone a retry.
pub const IP_CHECK_SECONDS: u32 = 5;

/// How long the advertiser waits after a wake before it reads the addresses
/// anyway, so a dropped report, a removed address or an unreported change is
/// followed within a minute.
pub const RECHECK: Duration = Duration::from_secs(60);

/// Why the advertiser woke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Change {
    /// The daemon added this IPv4 LAN address. The record reaches a network the
    /// daemon just joined only through a registration made after this report,
    /// so it always registers.
    Reported(Ipv4Addr),
    /// [`RECHECK`] passed since the last wake.
    Quiet,
}

/// What tells the advertiser the addresses may have changed: the daemon's
/// interface check, or a fake in tests.
pub(crate) trait InterfaceWatcher {
    /// Waits for the next possible change; `None` once the watch has ended.
    fn changed(&mut self) -> Option<Change>;
}

/// The daemon's `IpAdd` report of an IPv4 LAN address, or `recheck` passed
/// since the wait began. Other events are passed over under the same
/// deadline, so a busy network never postpones the recheck.
struct DaemonWatcher {
    events: mdns_sd::Receiver<DaemonEvent>,
    recheck: Duration,
}

impl InterfaceWatcher for DaemonWatcher {
    fn changed(&mut self) -> Option<Change> {
        let deadline = Instant::now() + self.recheck;
        loop {
            match self.events.recv_deadline(deadline) {
                Ok(DaemonEvent::IpAdd(IpAddr::V4(address))) if is_lan_address(address) => {
                    return Some(Change::Reported(address));
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => return Some(Change::Quiet),
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
}

/// What the record carries now, shared with the thread that follows the
/// network.
#[derive(Debug)]
pub(crate) struct Published {
    pub(crate) addresses: Vec<Ipv4Addr>,
    /// Set when the [`Advertiser`] is withdrawn or dropped; nothing registers
    /// after it.
    pub(crate) withdrawn: bool,
}

/// Follows the network until `watcher` ends or the record is withdrawn:
/// after each change it registers the record again when the addresses moved
/// or the daemon reported adding one. A failed registration is retried at
/// the next change. The lock is held across `register`, so no registration
/// lands after the withdraw's goodbye.
pub(crate) fn follow(
    watcher: &mut impl InterfaceWatcher,
    lan: impl Fn() -> Vec<Ipv4Addr>,
    published: &Mutex<Published>,
    mut register: impl FnMut(&[Ipv4Addr]) -> mdns_sd::Result<()>,
) {
    let mut owed = false;
    while let Some(change) = watcher.changed() {
        let addresses = lan();
        let mut published = published.lock().unwrap_or_else(PoisonError::into_inner);
        if published.withdrawn {
            return;
        }
        let moved = addresses != published.addresses;
        let reported = matches!(change, Change::Reported(added) if addresses.contains(&added));
        if !moved && !reported && !owed {
            continue;
        }
        match register(&addresses) {
            Ok(()) => {
                owed = false;
                if addresses.is_empty() {
                    warn_no_network();
                } else if moved {
                    tracing::info!(
                        target: "steno::handover",
                        "the network changed: the Bonjour record now carries {addresses:?}"
                    );
                } else {
                    tracing::debug!(
                        target: "steno::handover",
                        "the Bonjour record is announced again with {addresses:?} ({change:?})"
                    );
                }
                published.addresses = addresses;
            }
            Err(error) => {
                owed = true;
                tracing::warn!(
                    target: "steno::handover",
                    "registering the Bonjour record again after a network change: {error}"
                );
            }
        }
    }
}

/// Logs that the record carries no address, so no phone can find the
/// computer.
fn warn_no_network() {
    tracing::warn!(
        target: "steno::handover",
        "no Wi-Fi or wired network: the phone cannot find this computer until it joins one"
    );
}

/// The published record and the thread that re-registers it on network
/// changes. Dropping it withdraws the record.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
    published: Arc<Mutex<Published>>,
    /// The port of the record registered (`ServiceInfo::get_port`).
    #[cfg(test)]
    port: u16,
}

impl Advertiser {
    /// The service type in the form `mdns-sd` wants.
    pub const TYPE_DOMAIN: &'static str = "_steno._tcp.local.";

    /// The longest instance name a DNS label holds, in bytes.
    pub const INSTANCE_NAME_BYTES: usize = 63;

    /// The longest first label of the record's host, in bytes.
    const HOST_LABEL_BYTES: usize = 63;

    /// The record's host, `steno-<name>-<id>.local.`: `<name>` is the SAN label
    /// of `service_name`, cut to fit one DNS label, `<id>` the first 8 hex
    /// digits of `mac_id`. A name only Steno answers for: claiming the
    /// computer's own host name would fight the system's responder.
    #[must_use]
    pub fn host_name(service_name: &str, mac_id: Uuid) -> String {
        const PREFIX: &str = "steno-";
        let id = format!("-{:08x}", mac_id.as_fields().0);
        let name = HandoverIdentity::san_label(service_name);
        let name = name.strip_suffix(".local").unwrap_or(&name);
        let room = Self::HOST_LABEL_BYTES - PREFIX.len() - id.len();
        // `san_label` is ASCII, so any byte is a character boundary.
        let name = name[..name.len().min(room)].trim_end_matches('-');
        format!("{PREFIX}{name}{id}.local.")
    }

    /// The record for `service_name` on `port` at `addresses`. The instance
    /// name is cut to [`Self::INSTANCE_NAME_BYTES`] at a character boundary, as
    /// a longer label fails every packet.
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
        let host = Self::host_name(service_name, mac_id);
        let addresses: Vec<IpAddr> = addresses.iter().copied().map(IpAddr::V4).collect();
        let mut end = service_name.len().min(Self::INSTANCE_NAME_BYTES);
        while !service_name.is_char_boundary(end) {
            end -= 1;
        }
        ServiceInfo::new(
            Self::TYPE_DOMAIN,
            &service_name[..end],
            &host,
            &addresses[..],
            port,
            &properties[..],
        )
    }

    /// Publishes the record with the computer's LAN addresses and follows them.
    /// With no address it is published empty and filled once the computer joins
    /// a network.
    pub fn publish(service_name: &str, mac_id: Uuid, port: u16) -> mdns_sd::Result<Self> {
        let daemon = ServiceDaemon::new()?;
        #[cfg_attr(
            not(test),
            expect(unused_variables, reason = "only the tests read the port")
        )]
        let (fullname, registered_port, published) =
            Self::start(&daemon, service_name, mac_id, port).inspect_err(|_| {
                stop(&daemon);
            })?;
        Ok(Advertiser {
            daemon,
            fullname,
            published,
            #[cfg(test)]
            port: registered_port,
        })
    }

    /// The port of the record registered (`ServiceInfo::get_port`).
    #[cfg(test)]
    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    fn start(
        daemon: &ServiceDaemon,
        service_name: &str,
        mac_id: Uuid,
        port: u16,
    ) -> mdns_sd::Result<(String, u16, Arc<Mutex<Published>>)> {
        daemon.set_ip_check_interval(IP_CHECK_SECONDS)?;
        // Watch before the first read, so a change between the two is reported.
        let mut watcher = DaemonWatcher {
            events: daemon.monitor()?,
            recheck: RECHECK,
        };
        let addresses = current_lan_addresses();
        if addresses.is_empty() {
            warn_no_network();
        }
        let info = Self::service_info(service_name, mac_id, port, &addresses)?;
        let fullname = info.get_fullname().to_owned();
        let registered_port = info.get_port();
        daemon.register(info)?;
        let published = Arc::new(Mutex::new(Published {
            addresses,
            withdrawn: false,
        }));
        let following = published.clone();
        let daemon = daemon.clone();
        let service_name = service_name.to_owned();
        let spawned = std::thread::Builder::new()
            .name("steno-handover-bonjour".to_owned())
            .spawn(move || {
                follow(
                    &mut watcher,
                    current_lan_addresses,
                    &following,
                    |addresses| {
                        daemon.register(Self::service_info(&service_name, mac_id, port, addresses)?)
                    },
                );
            });
        if let Err(error) = spawned {
            tracing::warn!(
                target: "steno::handover",
                "the Bonjour record will not follow a network change: {error}"
            );
        }
        Ok((fullname, registered_port, published))
    }

    /// Withdraws the record and stops the daemon, as dropping the advertiser
    /// does; nothing is registered after this.
    pub fn withdraw(self) {
        drop(self);
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        let mut published = self
            .published
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        published.withdrawn = true;
        let _ = self.daemon.unregister(&self.fullname);
        stop(&self.daemon);
    }
}

/// Stops the daemon; a failure (its command queue full) is logged.
fn stop(daemon: &ServiceDaemon) {
    if let Err(error) = daemon.shutdown() {
        tracing::warn!(
            target: "steno::handover",
            "stopping the Bonjour daemon: {error}"
        );
    }
}

#[cfg(test)]
mod tests {
    use if_addrs::{IfOperStatus, Ifv4Addr, Ifv6Addr};

    use super::*;

    fn v4(name: &str, index: u32, ip: [u8; 4], status: IfOperStatus, p2p: bool) -> Interface {
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
    fn the_record_carries_the_version_the_id_the_addresses_and_a_host_only_steno_uses() {
        let mac_id = Uuid::parse_str("0F8FAD5B-D9CB-469F-A165-70867728950E").unwrap();
        let address = Ipv4Addr::new(192, 168, 1, 20);
        let info = Advertiser::service_info("Nicolai's Mac", mac_id, 4242, &[address]).unwrap();
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
            [&address]
        );
        assert_eq!(info.get_hostname(), "steno-nicolai-s-mac-0f8fad5b.local.");

        // Both labels fit DNS: the instance name is cut at a character boundary,
        // the host's name part before the id.
        let long = format!("{}\u{e9}tude", "a".repeat(62));
        let info = Advertiser::service_info(&long, mac_id, 4242, &[]).unwrap();
        assert_eq!(
            info.get_fullname(),
            format!("{}._steno._tcp.local.", "a".repeat(62))
        );
        let host = Advertiser::host_name(&"c".repeat(80), mac_id);
        let label = host.strip_suffix(".local.").unwrap();
        assert_eq!(label.len(), Advertiser::HOST_LABEL_BYTES);
        assert!(label.ends_with("c-0f8fad5b"), "{label}");
    }

    /// A watcher whose closure says which change came; `None` ends it.
    struct Watch<F: FnMut() -> Option<Change>>(F);

    impl<F: FnMut() -> Option<Change>> InterfaceWatcher for Watch<F> {
        fn changed(&mut self) -> Option<Change> {
            (self.0)()
        }
    }

    #[test]
    fn the_record_is_registered_again_when_the_addresses_move_a_report_names_one_or_a_try_failed() {
        let home = Ipv4Addr::new(192, 168, 1, 20);
        let office = Ipv4Addr::new(10, 0, 0, 5);
        let docker = Ipv4Addr::new(172, 17, 0, 1);
        // A change, the addresses then, and whether that registration fails.
        let steps = [
            (Change::Quiet, vec![home], false),
            (Change::Quiet, vec![home, office], false),
            (Change::Quiet, vec![office], true),
            (Change::Quiet, vec![office], false),
            (Change::Reported(office), vec![office], false),
            (Change::Reported(docker), vec![office], false),
            (Change::Quiet, vec![], false),
        ];
        let expected = [
            vec![home, office],
            vec![office],
            vec![office],
            vec![office],
            vec![],
        ];
        let published = Mutex::new(Published {
            addresses: vec![home],
            withdrawn: false,
        });
        let lan = Mutex::new(Vec::new());
        let failing = Mutex::new(false);
        let mut steps = steps.into_iter();
        let mut watcher = Watch(|| {
            let (change, next, fails) = steps.next()?;
            *lan.lock().unwrap() = next;
            *failing.lock().unwrap() = fails;
            Some(change)
        });
        let mut registered = Vec::new();
        follow(
            &mut watcher,
            || lan.lock().unwrap().clone(),
            &published,
            |addresses| {
                registered.push(addresses.to_vec());
                if *failing.lock().unwrap() {
                    Err(mdns_sd::Error::Again)
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(registered, expected);
        assert_eq!(published.lock().unwrap().addresses, Vec::<Ipv4Addr>::new());

        // Nothing registers once the record is withdrawn.
        published.lock().unwrap().withdrawn = true;
        let mut once = Some(Change::Reported(office));
        follow(
            &mut Watch(|| once.take()),
            || vec![office],
            &published,
            |_| panic!("registered after a withdraw"),
        );
    }

    #[test]
    fn the_daemon_watcher_reports_an_added_lan_address_or_a_quiet_recheck() {
        let office = Ipv4Addr::new(10, 0, 0, 5);
        let (daemon, events) = flume::unbounded();
        let mut watcher = DaemonWatcher {
            events,
            recheck: Duration::ZERO,
        };
        assert_eq!(watcher.changed(), Some(Change::Quiet));
        let passed_over = [
            DaemonEvent::Respond("en0".to_owned()),
            DaemonEvent::IpAdd("fe80::1".parse().unwrap()),
            DaemonEvent::IpAdd(Ipv4Addr::new(169, 254, 7, 7).into()),
            DaemonEvent::IpAdd(Ipv4Addr::new(100, 64, 0, 2).into()),
            DaemonEvent::IpDel(office.into()),
            DaemonEvent::IpAdd(office.into()),
        ];
        for event in passed_over {
            daemon.send(event).unwrap();
        }
        drop(daemon);
        assert_eq!(watcher.changed(), Some(Change::Reported(office)));
        assert_eq!(watcher.changed(), None, "the watch ends with the daemon");
    }

    #[test]
    fn only_lan_interfaces_qualify_and_only_they_and_loopback_are_served() {
        let interfaces = vec![
            v4("lo0", 1, [127, 0, 0, 1], IfOperStatus::Up, false),
            v4("en0", 1, [192, 168, 1, 20], IfOperStatus::Up, false),
            v4("en1", 1, [10, 0, 0, 5], IfOperStatus::Up, false),
            v4("en1", 1, [10, 0, 0, 5], IfOperStatus::Up, false),
            v4("en2", 1, [10, 0, 1, 5], IfOperStatus::Down, false),
            v4("en3", 1, [169, 254, 7, 7], IfOperStatus::Up, false),
            v4("utun3", 1, [10, 8, 0, 2], IfOperStatus::Up, true),
            v4("mesh0", 1, [100, 101, 7, 9], IfOperStatus::Up, false),
            v4("en6", 1, [100, 63, 255, 255], IfOperStatus::Up, false),
            v4("en5", 1, [100, 128, 0, 1], IfOperStatus::Up, false),
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
        let lan = lan_addresses(&interfaces);
        let expected = [
            [10, 0, 0, 5],
            [100, 63, 255, 255],
            [100, 128, 0, 1],
            [192, 168, 1, 20],
        ];
        assert_eq!(lan, expected.map(Ipv4Addr::from));

        for (local, served) in [
            ("127.0.0.1", true),
            ("::1", true),
            ("192.168.1.20", true),
            ("100.64.0.2", false),
            ("10.8.0.6", false),
        ] {
            assert_eq!(
                accepts_local_address(local.parse().unwrap(), &lan),
                served,
                "{local}"
            );
        }
        assert!(!accepts_local_address("192.168.1.20".parse().unwrap(), &[]));
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
            ("TAP, type 6", adapter(6, false, true), lan, false),
            ("PPP", adapter(23, true, true), lan, false),
            (
                "loopback on Ethernet",
                adapter(6, true, true),
                Ipv4Addr::LOCALHOST,
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
        let interfaces = vec![
            v4("Ethernet", 4, [192, 168, 1, 3], IfOperStatus::Up, false),
            v4("Wi-Fi", 7, [10, 0, 0, 9], IfOperStatus::Up, false),
            v4("vEthernet", 21, [172, 20, 16, 1], IfOperStatus::Up, false),
            v4("Unknown", 30, [192, 168, 2, 3], IfOperStatus::Up, false),
        ];
        let adapters = std::collections::BTreeMap::from([
            (4, adapter(6, true, true)),
            (7, adapter(71, true, true)),
            (21, adapter(6, false, true)),
        ]);
        assert_eq!(
            windows_lan_addresses(&interfaces, |index| adapters.get(&index).copied()),
            [Ipv4Addr::new(10, 0, 0, 9), Ipv4Addr::new(192, 168, 1, 3)]
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
}
