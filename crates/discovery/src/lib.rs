//! LAN discovery of NVRs and cameras.
//!
//! Two backends report into one protocol-neutral result type,
//! [`DiscoveredDevice`]:
//!
//! - **native** ([`native`]): the UDP search and probe that NVRs and cameras
//!   speaking the port-9000 protocol answer (docs/protocol.md, "LAN
//!   discovery");
//! - **ONVIF** ([`onvif`]): WS-Discovery, the public standard most other IP
//!   cameras answer.
//!
//! The codecs are sans-I/O; [`runner`] broadcasts on chosen interfaces,
//! listens for a bounded time, and merges the answers.
//!
//! Privacy: replies carry device IDs and addresses. Apps show the model,
//! name and address and keep [`DiscoveredDevice::device_id`] internal.
//! Discovery only sends on the local network, and only when asked: there
//! is no background scanning.

pub mod native;
pub mod onvif;
pub mod runner;
mod xml;

use std::net::IpAddr;

pub use runner::{Interface, Options, Report, discover, discover_with, interfaces};

/// What kind of device answered, as far as its answer says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceKind {
    /// A recorder with channels.
    Nvr,
    /// A camera (including doorbells).
    Camera,
    /// The answer did not say.
    Unknown,
}

impl DeviceKind {
    /// Lower-case name, for tables and JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            DeviceKind::Nvr => "nvr",
            DeviceKind::Camera => "camera",
            DeviceKind::Unknown => "unknown",
        }
    }
}

/// A way to connect to a discovered device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServiceKind {
    /// The native protocol over TCP (port 9000 by default).
    Native,
    /// ONVIF, over HTTP at the device service URL.
    Onvif,
}

impl ServiceKind {
    /// Lower-case name, for tables and JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            ServiceKind::Native => "native",
            ServiceKind::Onvif => "onvif",
        }
    }
}

/// One way to reach a device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    /// The protocol.
    pub kind: ServiceKind,
    /// TCP port, if the answer said.
    pub port: Option<u16>,
    /// Service URL (ONVIF device service), if any.
    pub url: Option<String>,
}

/// A device found on the network. Fields an answer did not carry are
/// `None`; answers from several backends for the same device are merged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredDevice {
    /// The address the answer came from.
    pub address: IpAddr,
    /// NVR, camera, or unknown.
    pub kind: DeviceKind,
    /// The name the user gave the device.
    pub name: Option<String>,
    /// Model (from ONVIF scopes; the native search does not carry it).
    pub model: Option<String>,
    /// Channel count, when the answer carries one.
    pub channels: Option<u32>,
    /// Battery charge in percent (battery-powered cameras).
    pub battery_percent: Option<u8>,
    /// MAC address, lower case with colons.
    pub mac: Option<String>,
    /// How to connect.
    pub services: Vec<Service>,
    /// The device's own ID (UID or ONVIF endpoint reference). Identifying:
    /// keep it internal (matching, deduplication), never show or log it.
    pub device_id: Option<String>,
}

impl DiscoveredDevice {
    /// A device at `address` with nothing else known.
    pub fn new(address: IpAddr) -> Self {
        Self {
            address,
            kind: DeviceKind::Unknown,
            name: None,
            model: None,
            channels: None,
            battery_percent: None,
            mac: None,
            services: Vec::new(),
            device_id: None,
        }
    }

    /// The TCP port of the native protocol, if known.
    pub fn native_port(&self) -> Option<u16> {
        self.service(ServiceKind::Native).and_then(|s| s.port)
    }

    /// The service of one kind, if the device offered it.
    pub fn service(&self, kind: ServiceKind) -> Option<&Service> {
        self.services.iter().find(|s| s.kind == kind)
    }

    /// Whether `other` describes the same device: same device ID when both
    /// have one, else the same address.
    fn same_device(&self, other: &DiscoveredDevice) -> bool {
        match (&self.device_id, &other.device_id) {
            (Some(a), Some(b)) if self.address != other.address => a == b,
            _ => self.address == other.address,
        }
    }

    /// Fills what `other` knows and `self` does not. Returns whether
    /// anything changed.
    fn absorb(&mut self, other: DiscoveredDevice) -> bool {
        let before = self.clone();
        if self.kind == DeviceKind::Unknown {
            self.kind = other.kind;
        }
        fill(&mut self.name, other.name);
        fill(&mut self.model, other.model);
        fill(&mut self.channels, other.channels);
        // A newer battery reading replaces an older one.
        if other.battery_percent.is_some() {
            self.battery_percent = other.battery_percent;
        }
        fill(&mut self.mac, other.mac);
        fill(&mut self.device_id, other.device_id);
        for s in other.services {
            match self.services.iter_mut().find(|x| x.kind == s.kind) {
                Some(x) => {
                    fill(&mut x.port, s.port);
                    fill(&mut x.url, s.url);
                }
                None => self.services.push(s),
            }
        }
        *self != before
    }
}

fn fill<T>(slot: &mut Option<T>, value: Option<T>) {
    if slot.is_none() {
        *slot = value;
    }
}

/// Most devices one search keeps; answers beyond it are ignored.
pub const MAX_DEVICES: usize = 1024;

/// Merged results of one search, in order of first answer.
#[derive(Clone, Debug, Default)]
pub struct DeviceSet {
    devices: Vec<DiscoveredDevice>,
}

impl DeviceSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Merges one answer. Returns the merged device if it is new or
    /// changed, `None` if the answer added nothing (a duplicate) or the set
    /// is full.
    pub fn merge(&mut self, found: DiscoveredDevice) -> Option<&DiscoveredDevice> {
        if let Some(i) = self.devices.iter().position(|d| d.same_device(&found)) {
            return self.devices[i].absorb(found).then(|| &self.devices[i]);
        }
        if self.devices.len() >= MAX_DEVICES {
            return None;
        }
        self.devices.push(found);
        self.devices.last()
    }

    /// The devices found so far.
    pub fn devices(&self) -> &[DiscoveredDevice] {
        &self.devices
    }

    /// The devices, consuming the set.
    pub fn into_devices(self) -> Vec<DiscoveredDevice> {
        self.devices
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn at(last: u8) -> DiscoveredDevice {
        DiscoveredDevice::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, last)))
    }

    #[test]
    fn duplicates_merge_and_report_no_change() {
        let mut set = DeviceSet::new();
        let mut a = at(10);
        a.name = Some("Camera 1".into());
        a.device_id = Some("UID-EXAMPLE".into());
        assert!(set.merge(a.clone()).is_some());
        assert!(set.merge(a.clone()).is_none());
        assert_eq!(set.devices().len(), 1);
    }

    #[test]
    fn backends_merge_by_address() {
        let mut set = DeviceSet::new();
        let mut native = at(10);
        native.kind = DeviceKind::Nvr;
        native.services.push(Service {
            kind: ServiceKind::Native,
            port: Some(9000),
            url: None,
        });
        let mut onvif = at(10);
        onvif.model = Some("Model X".into());
        onvif.services.push(Service {
            kind: ServiceKind::Onvif,
            port: Some(8000),
            url: Some("http://192.0.2.10:8000/onvif/device_service".into()),
        });
        set.merge(native);
        let merged = set.merge(onvif).unwrap().clone();
        assert_eq!(merged.kind, DeviceKind::Nvr);
        assert_eq!(merged.model.as_deref(), Some("Model X"));
        assert_eq!(merged.native_port(), Some(9000));
        assert_eq!(merged.services.len(), 2);
    }

    #[test]
    fn same_id_at_another_address_is_the_same_device() {
        let mut set = DeviceSet::new();
        let mut a = at(10);
        a.device_id = Some("UID-EXAMPLE".into());
        let mut b = at(11);
        b.device_id = Some("UID-EXAMPLE".into());
        b.battery_percent = Some(80);
        set.merge(a);
        set.merge(b);
        assert_eq!(set.devices().len(), 1);
        assert_eq!(set.devices()[0].battery_percent, Some(80));
        // Different IDs at different addresses stay apart.
        let mut c = at(12);
        c.device_id = Some("UID-OTHER".into());
        set.merge(c);
        assert_eq!(set.devices().len(), 2);
    }

    #[test]
    fn the_set_is_bounded() {
        let mut set = DeviceSet::new();
        for i in 0..MAX_DEVICES + 10 {
            let mut d = DiscoveredDevice::new(IpAddr::V4(Ipv4Addr::from(0xC000_0200 + i as u32)));
            d.name = Some(format!("Camera {i}"));
            set.merge(d);
        }
        assert_eq!(set.devices().len(), MAX_DEVICES);
    }
}
