//! The async (tokio) runner: opens UDP sockets on chosen IPv4 interfaces,
//! sends a few rounds of probes, listens for a bounded time, and merges the
//! answers into a [`DeviceSet`].
//!
//! Every socket is bound to an interface's own address, so probes leave by
//! that interface and the native search's fixed reply port (3000) can be
//! bound next to another client holding it on the wildcard address
//! (docs/protocol.md, "Our own search").

use std::collections::HashSet;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until};

use crate::native;
use crate::onvif;
use crate::{DeviceKind, DeviceSet, DiscoveredDevice, Service, ServiceKind};

/// An IPv4 interface discovery can run on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interface {
    /// The system's name for it (`en0`, `Ethernet`, ...).
    pub name: String,
    /// Its address.
    pub address: Ipv4Addr,
    /// Its subnet broadcast address, if it has one.
    pub broadcast: Option<Ipv4Addr>,
}

/// The IPv4 interfaces that are up and not loopback or point-to-point.
pub fn interfaces() -> io::Result<Vec<Interface>> {
    Ok(all_interfaces()?
        .into_iter()
        .filter(|(i, usable)| *usable && !i.address.is_loopback())
        .map(|(i, _)| i)
        .collect())
}

fn all_interfaces() -> io::Result<Vec<(Interface, bool)>> {
    let mut out = Vec::new();
    for i in if_addrs::get_if_addrs()? {
        let usable = i.is_oper_up() && !i.is_p2p();
        if let if_addrs::IfAddr::V4(v4) = i.addr {
            let broadcast = v4.broadcast.or_else(|| {
                (v4.prefixlen < 31)
                    .then(|| Ipv4Addr::from(u32::from(v4.ip) | !u32::from(v4.netmask)))
            });
            out.push((
                Interface {
                    name: i.name,
                    address: v4.ip,
                    broadcast,
                },
                usable,
            ));
        }
    }
    Ok(out)
}

/// What to send where. The defaults are the real protocol ports; tests
/// point them at mock devices on loopback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ports {
    /// Where the native search request goes; `None`: each interface's
    /// broadcast address (else 255.255.255.255), port 2000.
    pub search_target: Option<SocketAddrV4>,
    /// The local port devices send search records to (3000).
    pub search_reply_port: u16,
    /// Where the native probe goes; `None`: each interface's broadcast
    /// address (else 255.255.255.255), port 2015.
    pub probe_target: Option<SocketAddrV4>,
    /// Where ONVIF probes go (the WS-Discovery multicast group).
    pub onvif_target: SocketAddrV4,
}

impl Default for Ports {
    fn default() -> Self {
        Self {
            search_target: None,
            search_reply_port: native::SEARCH_REPLY_PORT,
            probe_target: None,
            onvif_target: onvif::MULTICAST_TARGET,
        }
    }
}

/// How to search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// Interface addresses to search on; empty: every interface from
    /// [`interfaces`].
    pub interfaces: Vec<Ipv4Addr>,
    /// How long to listen, from the first probe.
    pub duration: Duration,
    /// Probe rounds, spread over the first part of `duration` (at least 1).
    pub rounds: u32,
    /// Send the native search request (UDP 2000).
    pub native_search: bool,
    /// Send the native probe (UDP 2015), which battery cameras answer.
    pub native_probe: bool,
    /// Send ONVIF WS-Discovery probes.
    pub onvif: bool,
    /// Destinations and the reply port.
    pub ports: Ports,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            interfaces: Vec::new(),
            duration: Duration::from_secs(3),
            rounds: 2,
            native_search: true,
            native_probe: true,
            onvif: true,
            ports: Ports::default(),
        }
    }
}

/// The outcome of a search.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// The devices found, merged, in order of first answer.
    pub devices: Vec<DiscoveredDevice>,
    /// Per-interface problems (a port in use, a send that failed); the
    /// search went on without that socket.
    pub errors: Vec<String>,
    /// Datagrams received that were not answers (malformed, foreign, or
    /// answering someone else's probe).
    pub ignored: u64,
}

/// Searches with `options` and returns the devices found.
pub async fn discover(options: &Options) -> io::Result<Report> {
    discover_with(options, |_| {}).await
}

/// Searches with `options`, calling `on_update` each time a device is
/// found or an answer adds to what is known about it. Fails only if no
/// socket could be opened at all (or listing interfaces failed); other
/// problems are listed in [`Report::errors`]. Dropping the future stops
/// the search and closes every socket.
pub async fn discover_with<F>(options: &Options, mut on_update: F) -> io::Result<Report>
where
    F: FnMut(&DiscoveredDevice),
{
    let mut report = Report::default();
    let ifaces = resolve_interfaces(&options.interfaces)?;
    let (tx, mut rx) = mpsc::channel::<Datagram>(256);
    let mut receivers = JoinSet::new();
    let mut senders: Vec<Sender> = Vec::new();

    for iface in &ifaces {
        let bcast = iface.broadcast.unwrap_or(Ipv4Addr::BROADCAST);
        let mut open = |backend: Backend, port: u16, dest: SocketAddrV4| match open_udp(
            iface.address,
            port,
            backend,
        ) {
            Ok(socket) => {
                let socket = Arc::new(socket);
                receivers.spawn(receive(socket.clone(), backend, tx.clone()));
                senders.push(Sender {
                    socket,
                    backend,
                    dest,
                    iface: iface.address,
                    failed: false,
                });
            }
            Err(e) => report.errors.push(format!(
                "{}: {} on {}:{port}: {e}",
                iface.name,
                backend.as_str(),
                iface.address
            )),
        };
        if options.native_search {
            let dest = options
                .ports
                .search_target
                .unwrap_or(SocketAddrV4::new(bcast, native::SEARCH_PORT));
            open(Backend::Search, options.ports.search_reply_port, dest);
        }
        if options.native_probe {
            let dest = options
                .ports
                .probe_target
                .unwrap_or(SocketAddrV4::new(bcast, native::PROBE_PORT));
            open(Backend::Probe, 0, dest);
        }
        if options.onvif {
            open(Backend::Onvif, 0, options.ports.onvif_target);
        }
    }
    drop(tx);
    if senders.is_empty() {
        if report.errors.is_empty() {
            return Ok(report);
        }
        return Err(io::Error::other(report.errors.join("; ")));
    }

    let rounds = options.rounds.max(1);
    let start = Instant::now();
    let deadline = start + options.duration;
    let interval = options.duration / (rounds + 1);
    let mut next_round = start;
    let mut round = 0;
    let mut set = DeviceSet::new();
    let mut tids: HashSet<u32> = HashSet::new();
    let mut message_ids: HashSet<String> = HashSet::new();

    loop {
        tokio::select! {
            biased;
            _ = sleep_until(deadline) => break,
            _ = sleep_until(next_round), if round < rounds => {
                for s in &mut senders {
                    s.send_round(&mut tids, &mut message_ids, &mut report.errors).await;
                }
                round += 1;
                next_round += interval;
            }
            datagram = rx.recv() => {
                let Some(d) = datagram else { break };
                let found = interpret(&d, &tids, &message_ids);
                if found.is_empty() {
                    report.ignored += 1;
                    tracing::trace!(backend = d.backend.as_str(), len = d.data.len(), "ignored datagram");
                }
                for f in found {
                    if let Some(device) = set.merge(f) {
                        on_update(device);
                    }
                }
            }
        }
    }
    receivers.abort_all();
    while receivers.join_next().await.is_some() {}
    if report.ignored > 0 {
        tracing::debug!(ignored = report.ignored, "discovery ignored datagrams");
    }
    report.devices = set.into_devices();
    Ok(report)
}

fn resolve_interfaces(chosen: &[Ipv4Addr]) -> io::Result<Vec<Interface>> {
    if chosen.is_empty() {
        let found = interfaces()?;
        if found.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no IPv4 interface is up",
            ));
        }
        return Ok(found);
    }
    // Look the chosen addresses up for their names and broadcast
    // addresses; an address the system does not list (or a failed lookup)
    // is still tried, with the limited broadcast.
    let known = all_interfaces().unwrap_or_default();
    Ok(chosen
        .iter()
        .map(|&address| {
            known
                .iter()
                .find(|(i, _)| i.address == address)
                .map(|(i, _)| i.clone())
                .unwrap_or(Interface {
                    name: address.to_string(),
                    address,
                    broadcast: (!address.is_loopback()).then_some(Ipv4Addr::BROADCAST),
                })
        })
        .collect())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Search,
    Probe,
    Onvif,
}

impl Backend {
    fn as_str(self) -> &'static str {
        match self {
            Backend::Search => "native search",
            Backend::Probe => "native probe",
            Backend::Onvif => "ONVIF",
        }
    }
}

struct Datagram {
    backend: Backend,
    from: SocketAddr,
    data: Vec<u8>,
}

struct Sender {
    socket: Arc<UdpSocket>,
    backend: Backend,
    dest: SocketAddrV4,
    iface: Ipv4Addr,
    failed: bool,
}

impl Sender {
    async fn send_round(
        &mut self,
        tids: &mut HashSet<u32>,
        message_ids: &mut HashSet<String>,
        errors: &mut Vec<String>,
    ) {
        let mut payloads: Vec<Vec<u8>> = Vec::new();
        match self.backend {
            // The official client sends each search request twice.
            Backend::Search => {
                payloads.push(native::SEARCH_REQUEST.to_vec());
                payloads.push(native::SEARCH_REQUEST.to_vec());
            }
            Backend::Probe => {
                let port = self.socket.local_addr().map(|a| a.port()).unwrap_or(0);
                let tid = u32::from_le_bytes(random_bytes::<4>());
                tids.insert(tid);
                payloads.push(native::build_probe(tid, port));
            }
            Backend::Onvif => {
                for ty in [
                    onvif::ProbeType::NetworkVideoTransmitter,
                    onvif::ProbeType::Device,
                ] {
                    let id = onvif::new_message_id();
                    payloads.push(onvif::build_probe(&id, ty).into_bytes());
                    message_ids.insert(id);
                }
            }
        }
        for p in payloads {
            if let Err(e) = self.socket.send_to(&p, self.dest).await
                && !self.failed
            {
                // Report each socket's failure once, not every round.
                self.failed = true;
                tracing::warn!(backend = self.backend.as_str(), error = %e, "discovery send failed");
                errors.push(format!(
                    "{} from {}: send failed: {e}",
                    self.backend.as_str(),
                    self.iface
                ));
            }
        }
    }
}

fn open_udp(address: Ipv4Addr, port: u16, backend: Backend) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // On Unix, SO_REUSEADDR lets this socket bind the interface's address
    // while another client holds the port on the wildcard address. Windows
    // allows that without the option, and there the option would let two
    // sockets share one exact address, so it is left off.
    #[cfg(not(windows))]
    if port != 0 {
        socket.set_reuse_address(true)?;
    }
    socket.set_broadcast(true)?;
    if backend == Backend::Onvif {
        socket.set_multicast_if_v4(&address)?;
        socket.set_multicast_ttl_v4(1)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::V4(SocketAddrV4::new(address, port)).into())?;
    UdpSocket::from_std(socket.into())
}

async fn receive(socket: Arc<UdpSocket>, backend: Backend, tx: mpsc::Sender<Datagram>) {
    let mut buf = vec![0u8; onvif::MAX_DATAGRAM + 1];
    let mut logged = false;
    loop {
        match socket.recv_from(&mut buf).await {
            Ok((n, from)) => {
                if n > onvif::MAX_DATAGRAM {
                    continue;
                }
                let d = Datagram {
                    backend,
                    from,
                    data: buf[..n].to_vec(),
                };
                if tx.send(d).await.is_err() {
                    return;
                }
            }
            // Windows reports an ICMP "port unreachable" for an earlier
            // send as a reset on the next receive; it says nothing about
            // this socket.
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {}
            Err(e) => {
                if !logged {
                    logged = true;
                    tracing::warn!(backend = backend.as_str(), error = %e, "discovery receive failed");
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

/// Turns one datagram into what it says about devices (empty: ignore it).
fn interpret(
    d: &Datagram,
    tids: &HashSet<u32>,
    message_ids: &HashSet<String>,
) -> Vec<DiscoveredDevice> {
    let source = d.from.ip();
    match d.backend {
        Backend::Search => native::parse_search_record(&d.data)
            .map(|r| from_search_record(source, r))
            .into_iter()
            .collect(),
        Backend::Probe => native::parse_probe_reply(&d.data)
            .filter(|r| tids.contains(&r.tid))
            .map(|r| {
                let mut dev = DiscoveredDevice::new(source);
                dev.battery_percent = r.battery_percent;
                dev.device_id = r.uid;
                dev.services.push(Service {
                    kind: ServiceKind::Native,
                    port: None,
                    url: None,
                });
                dev
            })
            .into_iter()
            .collect(),
        Backend::Onvif => {
            let Some(pm) = onvif::parse_probe_matches(&d.data) else {
                return Vec::new();
            };
            if pm
                .relates_to
                .as_ref()
                .is_some_and(|r| !message_ids.contains(r))
            {
                return Vec::new();
            }
            pm.matches
                .into_iter()
                .map(|m| from_probe_match(source, m))
                .collect()
        }
    }
}

fn from_search_record(source: IpAddr, r: native::SearchRecord) -> DiscoveredDevice {
    let mut dev = DiscoveredDevice::new(source);
    if r.address.is_some_and(|a| IpAddr::V4(a) != source) {
        tracing::debug!("search record names another address than its source");
    }
    dev.kind = match r.kind {
        native::RecordKind::Camera => DeviceKind::Camera,
        native::RecordKind::Nvr => DeviceKind::Nvr,
        native::RecordKind::Other(_) => DeviceKind::Unknown,
    };
    dev.name = Some(r.name.clone()).filter(|s| !s.is_empty());
    dev.channels = (r.channels > 0).then_some(u32::from(r.channels));
    dev.mac = (r.mac != [0; 6]).then(|| r.mac_string());
    dev.device_id = Some(r.uid).filter(|s| !s.is_empty());
    dev.services.push(Service {
        kind: ServiceKind::Native,
        port: (r.tcp_port != 0).then_some(r.tcp_port),
        url: None,
    });
    dev
}

fn from_probe_match(source: IpAddr, m: onvif::ProbeMatch) -> DiscoveredDevice {
    let mut dev = DiscoveredDevice::new(source);
    dev.name = m.name();
    dev.model = m.model();
    // Prefer the service address on the host that answered.
    let url = m
        .xaddrs
        .iter()
        .find(|u| onvif::url_host_port(u).is_some_and(|(h, _)| IpAddr::V4(h) == source))
        .or_else(|| m.xaddrs.first())
        .cloned();
    let port = url
        .as_deref()
        .and_then(onvif::url_host_port)
        .map(|(_, p)| p);
    dev.device_id = m.endpoint;
    dev.services.push(Service {
        kind: ServiceKind::Onvif,
        port,
        url,
    });
    dev
}

/// Non-cryptographic random bytes (transaction and message IDs), from the
/// standard library's randomly keyed hasher.
pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut out = [0u8; N];
    for chunk in out.chunks_mut(8) {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        if let Ok(t) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            h.write_u128(t.as_nanos());
        }
        let v = h.finish().to_le_bytes();
        chunk.copy_from_slice(&v[..chunk.len()]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_ids_differ() {
        let a = random_bytes::<16>();
        let b = random_bytes::<16>();
        assert_ne!(a, b);
    }

    #[test]
    fn interface_listing_works() {
        // Lists without error on every platform; content depends on the host.
        let all = all_interfaces().unwrap();
        assert!(interfaces().unwrap().len() <= all.len());
    }

    #[test]
    fn chosen_unknown_interfaces_fall_back_to_the_limited_broadcast() {
        let doc = Ipv4Addr::new(192, 0, 2, 77);
        let r = resolve_interfaces(&[doc, Ipv4Addr::LOCALHOST]).unwrap();
        assert_eq!(r[0].broadcast, Some(Ipv4Addr::BROADCAST));
        assert_eq!(r[1].address, Ipv4Addr::LOCALHOST);
    }
}
