//! Bonjour: `_steno._tcp` on the LAN with the TXT record the phone reads
//! (`v`, the protocol version; `id`, the computer's id). The Swift listener
//! publishes through `NWListener.Service`; here `mdns-sd` does it. Both
//! answer the phone's `NWBrowser` the same way.
//!
//! Which network: the Swift listener sets `prohibitedInterfaceTypes =
//! [.cellular, .other]`, so the service lives on Wi-Fi and wired Ethernet
//! and never on a VPN tunnel. Here [`lan_addresses`] keeps the IPv4
//! addresses of the interfaces that are up and not loopback, link-local or
//! point-to-point, and never one in `100.64.0.0/10`, the shared address
//! space (RFC 6598) carrier-grade NAT and mesh VPNs number from. Most
//! tunnels on Linux and macOS (`wg0`, `tun0`, `utun3`) are point-to-point;
//! layer-2 ones (a TAP device, `feth`) and bridges (`docker0`,
//! `bridge100`) are not, and are served. Windows marks only PPP and its
//! own tunnels point-to-point; a Wintun, TAP or Hyper-V adapter there
//! looks like Ethernet, so on Windows an address also needs an adapter
//! that is Ethernet or Wi-Fi, hardware, and up ([`windows_keeps`]), the
//! counterpart of Swift's "not `.other`". The record carries those
//! addresses, and the listener accepts connections on them and on loopback
//! only. The listener reads them again, at most once a second, while it
//! accepts. The record follows them as `NWListener` follows a network
//! change ([`Advertiser`]): it is registered again under the same name,
//! TXT record and port. The daemon's report that it added an IPv4 address
//! the record carries ([`IP_CHECK_SECONDS`]) always registers it, because
//! only a registration made after the daemon joined a new network is
//! announced there; any other wake, including a quiet [`RECHECK`] a minute
//! after the last, registers it only when the addresses moved. The
//! record's host is `steno-<name>-<id>.local.` ([`Advertiser::host_name`]),
//! a name only Steno answers for, where Swift's record uses mDNSResponder's
//! own host.
//! Swift: `Network/HandoverServer.swift`.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use if_addrs::{IfAddr, Interface};
use mdns_sd::{DaemonEvent, RecvTimeoutError, ServiceDaemon, ServiceInfo};
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
/// mesh VPN lives and a home or office network does not.
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

/// How often the daemon reads the interfaces, in seconds: it joins the
/// multicast group of a new network and reports the change then, so the
/// record cannot be announced there sooner. A Wi-Fi switch and its DHCP
/// lease take seconds too, and the phone waits 15 s for the computer to
/// appear before it backs off, so five seconds (`mdns-sd`'s default,
/// stated here so a new default cannot change it) costs one interface
/// read every 5 s and no phone a retry.
pub const IP_CHECK_SECONDS: u32 = 5;

/// How long the advertiser waits after a wake before it reads the
/// addresses anyway, whatever else the daemon sends meanwhile: a report the
/// daemon dropped (its channel to the advertiser was full), a removed
/// address, or a change it does not report (a Windows adapter that stopped
/// counting as hardware), is followed within a minute. Until then the
/// daemon sends a removed address only on an interface in its subnet, so
/// only a second interface on the same LAN can still carry it.
pub const RECHECK: Duration = Duration::from_secs(60);

/// Why the advertiser woke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Change {
    /// The daemon reported that it added this IPv4 LAN address. It sends
    /// the report after its interface check joined the address's multicast
    /// group, and on its own it announces only records that let it choose
    /// the addresses, which this one does not; so the record reaches a
    /// network the daemon has just joined only through a registration made
    /// after the report, even when a recheck already published that
    /// network's address.
    Reported(Ipv4Addr),
    /// [`RECHECK`] passed since the last wake, whatever else the daemon
    /// sent.
    Quiet,
}

/// What tells the advertiser that the computer's addresses may have
/// changed. The product's is the daemon's own interface check; the tests
/// drive a fake one.
pub(crate) trait InterfaceWatcher {
    /// Waits for the next possible change; `None` once the watch has
    /// ended.
    fn changed(&mut self) -> Option<Change>;
}

/// The daemon's report of an IPv4 LAN address it added (`IpAdd`), sent
/// during its interface check, or `recheck` ([`RECHECK`] in the product)
/// passed since the wait began. Every other event (an IPv6 or non-LAN
/// address, a removed address, an answered query) is passed over under the
/// same deadline, so a busy network never postpones the recheck. The watch
/// ends with the daemon.
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

/// What the record carries now, shared by the advertiser and the thread
/// that follows the network.
#[derive(Debug)]
pub(crate) struct Published {
    pub(crate) addresses: Vec<Ipv4Addr>,
    /// Set when the [`Advertiser`] is withdrawn or dropped: nothing is
    /// registered after it.
    pub(crate) withdrawn: bool,
}

/// Follows the network until `watcher` ends or the record is withdrawn:
/// after each change it reads `lan` (sorted and without duplicates, as
/// [`lan_addresses`] gives them, so the order never counts as a move) and
/// registers the record with those addresses when they moved, or when the
/// daemon reported adding one of them ([`Change::Reported`]). A failed
/// registration keeps the old addresses as the published ones and is owed:
/// the next change, quiet or not, tries again. The lock is held across
/// `register`, so withdrawing or dropping the [`Advertiser`], which takes
/// it too, never sees a registration land after its goodbye.
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

/// The published record and the thread that registers it again when the
/// network changes. Dropping it withdraws the record, as
/// [`Advertiser::withdraw`] does.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
    published: Arc<Mutex<Published>>,
}

impl Advertiser {
    /// The service type in the form `mdns-sd` wants.
    pub const TYPE_DOMAIN: &'static str = "_steno._tcp.local.";

    /// The longest instance name a DNS label holds, in bytes.
    pub const INSTANCE_NAME_BYTES: usize = 63;

    /// The longest first label of the record's host, in bytes: one DNS
    /// label.
    const HOST_LABEL_BYTES: usize = 63;

    /// The record's host, `steno-<name>-<id>.local.`: `<name>` is
    /// [`HandoverIdentity::san_label`]'s of `service_name`, cut so the
    /// whole fits one DNS label, and `<id>` the first 8 hex digits of
    /// `mac_id`, which the TXT record publishes anyway. A name only Steno
    /// uses: the computer's own host name belongs to mDNSResponder, Avahi or
    /// Windows, and a second responder claiming it would send a goodbye for
    /// it at withdraw and, with an address the system no longer holds, make
    /// mDNSResponder rename the computer. The id tells two computers of
    /// one name apart without the daemon's probe, whose rename (`-2`) would
    /// not fit a label already cut to the limit.
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

    /// The record for `service_name` on `port` at `addresses`, before it
    /// is published. The instance name is `service_name` cut to
    /// [`Self::INSTANCE_NAME_BYTES`] at a character boundary: a longer
    /// label fails every packet, and a computer name can be longer. The
    /// host is [`Self::host_name`].
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

    /// Publishes the record with the computer's LAN addresses
    /// ([`current_lan_addresses`]) and follows them from then on. With no
    /// address the record is published empty, which no phone can resolve,
    /// and filled once the computer joins a network.
    pub fn publish(service_name: &str, mac_id: Uuid, port: u16) -> mdns_sd::Result<Self> {
        let daemon = ServiceDaemon::new()?;
        let (fullname, published) =
            Self::start(&daemon, service_name, mac_id, port).inspect_err(|_| {
                stop(&daemon);
            })?;
        Ok(Advertiser {
            daemon,
            fullname,
            published,
        })
    }

    fn start(
        daemon: &ServiceDaemon,
        service_name: &str,
        mac_id: Uuid,
        port: u16,
    ) -> mdns_sd::Result<(String, Arc<Mutex<Published>>)> {
        daemon.set_ip_check_interval(IP_CHECK_SECONDS)?;
        // Watch before the first read, so a change between the two is
        // reported.
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
        Ok((fullname, published))
    }

    /// Withdraws the record and stops the daemon; the thread that follows
    /// the network registers nothing after this and ends with the daemon,
    /// or at its next wake when the daemon would not stop (logged). The
    /// same as dropping the advertiser, which does the work, once.
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

/// Stops the daemon. A failure (its command queue full) leaves the
/// daemon's thread running, so it is logged.
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
        assert_eq!(info.get_hostname(), "steno-nicolai-s-mac-0f8fad5b.local.");
    }

    #[test]
    fn the_host_is_a_name_only_steno_uses_told_apart_by_the_id_and_fits_one_label() {
        let mac_id = Uuid::parse_str("0F8FAD5B-D9CB-469F-A165-70867728950E").unwrap();
        let other = Uuid::parse_str("00000001-0000-0000-0000-000000000000").unwrap();
        assert_eq!(
            Advertiser::host_name("MacBook Pro", mac_id),
            "steno-macbook-pro-0f8fad5b.local."
        );
        assert_eq!(
            Advertiser::host_name("MacBook Pro", other),
            "steno-macbook-pro-00000001.local.",
            "two computers of one name"
        );
        assert_eq!(
            Advertiser::host_name("---", mac_id),
            "steno-steno-0f8fad5b.local."
        );
        // `steno-`, 47 bytes of name, then `-` and 8 hex digits fit; the
        // name's `-` at the cut is dropped.
        let long = Advertiser::host_name(&format!("{}-b", "a".repeat(47)), mac_id);
        assert_eq!(long, format!("steno-{}-0f8fad5b.local.", "a".repeat(47)));
        let label = Advertiser::host_name(&"c".repeat(80), mac_id);
        let label = label.strip_suffix(".local.").unwrap();
        assert_eq!(label.len(), Advertiser::HOST_LABEL_BYTES);
        assert!(label.ends_with("c-0f8fad5b"), "the cut takes the name");
    }

    #[test]
    fn a_name_too_long_for_a_label_is_cut_at_a_character_boundary() {
        let mac_id = Uuid::nil();
        let long = format!("{}\u{e9}tude", "a".repeat(62));
        let info = Advertiser::service_info(&long, mac_id, 4242, &[]).unwrap();
        assert_eq!(
            info.get_fullname(),
            format!("{}._steno._tcp.local.", "a".repeat(62))
        );
        let fits = "b".repeat(63);
        let info = Advertiser::service_info(&fits, mac_id, 4242, &[]).unwrap();
        assert_eq!(info.get_fullname(), format!("{fits}._steno._tcp.local."));
    }

    /// A watcher whose closure says which change came; it ends the watch
    /// by returning `None`.
    struct Watch<F: FnMut() -> Option<Change>>(F);

    impl<F: FnMut() -> Option<Change>> InterfaceWatcher for Watch<F> {
        fn changed(&mut self) -> Option<Change> {
            (self.0)()
        }
    }

    /// Runs `follow` from `published` over `steps`, each a change and the
    /// addresses the computer has then, and returns every registration it
    /// tried; the attempts numbered in `failing` (from 1) fail.
    fn registrations(
        published: &Mutex<Published>,
        steps: Vec<(Change, Vec<Ipv4Addr>)>,
        failing: &[usize],
    ) -> Vec<Vec<Ipv4Addr>> {
        let lan = Mutex::new(Vec::new());
        let mut steps = steps.into_iter();
        let mut watcher = Watch(|| {
            let (change, next) = steps.next()?;
            *lan.lock().unwrap() = next;
            Some(change)
        });
        let mut registered = Vec::new();
        follow(
            &mut watcher,
            || lan.lock().unwrap().clone(),
            published,
            |addresses| {
                registered.push(addresses.to_vec());
                if failing.contains(&registered.len()) {
                    Err(mdns_sd::Error::Again)
                } else {
                    Ok(())
                }
            },
        );
        registered
    }

    fn published(addresses: &[Ipv4Addr]) -> Mutex<Published> {
        Mutex::new(Published {
            addresses: addresses.to_vec(),
            withdrawn: false,
        })
    }

    #[test]
    fn a_quiet_recheck_registers_the_record_again_only_when_the_addresses_moved() {
        let home = Ipv4Addr::new(192, 168, 1, 20);
        let office = Ipv4Addr::new(10, 0, 0, 5);
        let record = published(&[home]);
        let quiet = |addresses: Vec<Ipv4Addr>| (Change::Quiet, addresses);
        let registered = registrations(
            &record,
            vec![
                quiet(vec![home]),
                quiet(vec![home, office]),
                quiet(vec![office]),
                quiet(vec![]),
                quiet(vec![]),
                quiet(vec![home]),
            ],
            &[],
        );
        assert_eq!(
            registered,
            vec![vec![home, office], vec![office], vec![], vec![home]],
            "registered again on each move, not for a recheck that moved nothing"
        );
        assert_eq!(record.lock().unwrap().addresses, vec![home]);
    }

    #[test]
    fn a_report_of_an_address_the_record_carries_registers_it_again_even_when_nothing_moved() {
        let office = Ipv4Addr::new(10, 0, 0, 5);
        let docker = Ipv4Addr::new(172, 17, 0, 1);
        let record = published(&[]);
        let registered = registrations(
            &record,
            vec![
                // A recheck reads the address before the daemon has joined
                // its network; the daemon's report then comes with it.
                (Change::Quiet, vec![office]),
                (Change::Reported(office), vec![office]),
                (Change::Quiet, vec![office]),
                (Change::Reported(docker), vec![office]),
                (Change::Reported(office), vec![]),
                (Change::Reported(office), vec![]),
            ],
            &[],
        );
        assert_eq!(
            registered,
            vec![vec![office], vec![office], vec![]],
            "a report registers only for an address the record carries; otherwise only a move does"
        );
    }

    #[test]
    fn a_failed_registration_is_tried_again_at_the_next_change_even_a_quiet_one() {
        let office = Ipv4Addr::new(10, 0, 0, 5);
        let record = published(&[]);
        let steps = [
            Change::Quiet,
            Change::Quiet,
            Change::Reported(office),
            Change::Quiet,
            Change::Quiet,
        ]
        .map(|change| (change, vec![office]));
        // The first registration, after a move, and the one the report
        // forces, with nothing moved.
        let registered = registrations(&record, steps.into(), &[1, 3]);
        assert_eq!(
            registered.len(),
            4,
            "each failure is tried again at the next change"
        );
        assert_eq!(record.lock().unwrap().addresses, vec![office]);
    }

    #[test]
    fn nothing_is_registered_once_the_record_is_withdrawn() {
        let office = Ipv4Addr::new(10, 0, 0, 5);
        let record = published(&[]);
        let mut first = true;
        let mut watcher = Watch(|| {
            assert!(
                std::mem::take(&mut first),
                "the watch goes on after a withdraw"
            );
            record.lock().unwrap().withdrawn = true;
            Some(Change::Reported(office))
        });
        follow(
            &mut watcher,
            || vec![office],
            &record,
            |_| panic!("registered after a withdraw"),
        );
        assert_eq!(record.lock().unwrap().addresses, Vec::<Ipv4Addr>::new());
    }

    #[test]
    fn an_added_ipv4_lan_address_or_a_quiet_recheck_is_a_change_and_a_gone_daemon_ends_the_watch() {
        let office = Ipv4Addr::new(10, 0, 0, 5);
        let (daemon, events) = flume::unbounded();
        // A zero recheck times out at once on an empty channel.
        let mut watcher = DaemonWatcher {
            events,
            recheck: Duration::ZERO,
        };
        assert_eq!(watcher.changed(), Some(Change::Quiet));
        daemon.send(DaemonEvent::IpAdd(office.into())).unwrap();
        assert_eq!(watcher.changed(), Some(Change::Reported(office)));
        let passed_over = [
            DaemonEvent::Respond("en0".to_owned()),
            DaemonEvent::Announce(String::new(), String::new()),
            DaemonEvent::IpAdd("fe80::1".parse().unwrap()),
            DaemonEvent::IpAdd("2001:db8::7".parse().unwrap()),
            DaemonEvent::IpAdd(Ipv4Addr::new(169, 254, 7, 7).into()),
            DaemonEvent::IpAdd(Ipv4Addr::new(100, 64, 0, 2).into()),
            DaemonEvent::IpDel(office.into()),
        ];
        for event in passed_over {
            daemon.send(event).unwrap();
        }
        assert_eq!(
            watcher.changed(),
            Some(Change::Quiet),
            "other events, an IPv6, non-LAN or removed address too, are passed over"
        );
        daemon.send(DaemonEvent::IpAdd(office.into())).unwrap();
        drop(daemon);
        assert_eq!(watcher.changed(), Some(Change::Reported(office)));
        assert_eq!(watcher.changed(), None, "the watch ends with the daemon");
    }

    #[test]
    fn events_passed_over_do_not_postpone_the_quiet_recheck() {
        let (daemon, events) = flume::unbounded();
        let recheck = Duration::from_millis(300);
        let mut watcher = DaemonWatcher { events, recheck };
        // A busy network: an answered query, an IPv6 address and a removed
        // address every 100 ms, for up to 5 s or until the watcher is gone.
        let feeder = std::thread::spawn(move || {
            for _ in 0..50 {
                let busy = [
                    DaemonEvent::Respond("en0".to_owned()),
                    DaemonEvent::IpAdd("fe80::1".parse().unwrap()),
                    DaemonEvent::IpDel(Ipv4Addr::new(10, 0, 0, 5).into()),
                ];
                for event in busy {
                    if daemon.send(event).is_err() {
                        return;
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let began = Instant::now();
        assert_eq!(watcher.changed(), Some(Change::Quiet));
        let waited = began.elapsed();
        drop(watcher);
        feeder.join().unwrap();
        // The recheck comes 300 ms after the wait began; a wait restarted at
        // each event would last as long as the feeder, 5 s. The bound
        // leaves room for a loaded machine.
        assert!(
            waited >= recheck && waited < Duration::from_secs(2),
            "{waited:?}"
        );
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
            v4("mesh0", [100, 101, 7, 9], IfOperStatus::Up, false),
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
            v4_at("Mesh VPN", 12, [100, 64, 0, 10], IfOperStatus::Up, false),
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
