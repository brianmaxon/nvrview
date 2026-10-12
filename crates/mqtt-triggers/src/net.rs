//! Which broker addresses count as the local network.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

fn v4(a: Ipv4Addr) -> bool {
    let o = a.octets();
    a.is_loopback()
        || a.is_private()
        || a.is_link_local()
        // the shared address space of RFC 6598, used by some overlay networks
        || (o[0] == 100 && (64..128).contains(&o[1]))
}

fn v6(a: Ipv6Addr) -> bool {
    if let Some(m) = a.to_ipv4_mapped() {
        return v4(m);
    }
    let s = a.segments();
    a.is_loopback()
        // fc00::/7 unique local
        || (s[0] & 0xfe00) == 0xfc00
        // fe80::/10 link local
        || (s[0] & 0xffc0) == 0xfe80
}

/// Loopback, private, link-local, and shared (carrier-grade) addresses.
pub fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => v4(a),
        IpAddr::V6(a) => v6(a),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    #[test]
    fn classes() {
        let local = [
            v4(127, 0, 0, 1),
            v4(10, 1, 2, 3),
            v4(172, 16, 0, 1),
            v4(172, 31, 255, 255),
            v4(192, 168, 0, 9),
            v4(169, 254, 1, 1),
            v4(100, 64, 0, 1),
            v4(100, 127, 255, 255),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1)),
            IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)),
            IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0xc0a8, 0x0101)),
        ];
        for a in local {
            assert!(is_local(a), "{a}");
        }
        let public = [
            v4(192, 0, 2, 1),
            v4(203, 0, 113, 5),
            v4(8, 8, 8, 8),
            v4(172, 32, 0, 1),
            v4(100, 128, 0, 1),
            v4(100, 63, 255, 255),
            IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
            IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x0808, 0x0808)),
        ];
        for a in public {
            assert!(!is_local(a), "{a}");
        }
    }
}
