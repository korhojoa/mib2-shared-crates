// SPDX-License-Identifier: MIT OR Apache-2.0
//! Turning a DHCP lease into interface configuration for `bnep0`.
//!
//! Pure lease arithmetic and default values: no I/O or process creation.
//! The caller converts `IfaceConfig` into interface and route commands.
//! Apply the existing lease instead of starting a second DHCP transaction
//! on the same link.

/// Everything needed to configure the interface, all of it derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IfaceConfig {
    pub ip: [u8; 4],
    pub netmask: [u8; 4],
    /// `ip | !netmask` - the directed broadcast for the leased subnet.
    pub broadcast: [u8; 4],
    /// Option 3 (router) when the server offered one, else the server
    /// identifier. A phone tether is its own gateway, so these agree in
    /// practice; the fallback matters for servers that omit option 3.
    pub gateway: [u8; 4],
    /// Option 6, first entry. `None` when the lease carried no DNS, in which
    /// case the caller MUST NOT touch resolv.conf: overwriting a working
    /// resolver with nothing is worse than leaving it alone.
    pub dns: Option<[u8; 4]>,
    /// Ethernet MTU we advertise on bnep0. BNEP carries ~1691-byte payloads,
    /// so a full 1500 fits with room for the BNEP header.
    pub mtu: u16,
}

/// The advertised MTU. The interface driver must accept frames of this size.
pub const IFACE_MTU: u16 = 1500;

/// Classful default for a lease that carried no option 1. RFC 2131 leaves
/// the netmask optional and a few tethers really do omit it; guessing
/// classfully is what every DHCP client on earth does in that case, and it is
/// right for the 10/172.16/192.168 ranges phones hand out.
#[must_use]
pub fn classful_netmask(ip: [u8; 4]) -> [u8; 4] {
    match ip[0] {
        0..=127 => [255, 0, 0, 0],
        128..=191 => [255, 255, 0, 0],
        _ => [255, 255, 255, 0],
    }
}

/// `ip | !netmask`.
#[must_use]
pub fn broadcast_of(ip: [u8; 4], netmask: [u8; 4]) -> [u8; 4] {
    let mut b = [0u8; 4];
    for i in 0..4 {
        b[i] = ip[i] | !netmask[i];
    }
    b
}

/// `ip & netmask`.
#[must_use]
pub fn network_of(ip: [u8; 4], netmask: [u8; 4]) -> [u8; 4] {
    let mut n = [0u8; 4];
    for i in 0..4 {
        n[i] = ip[i] & netmask[i];
    }
    n
}

/// Contiguous-prefix length of a netmask, or `None` if the mask has a hole
/// in it (which no sane server sends, and which `ifconfig` would reject).
#[must_use]
pub fn prefix_len(netmask: [u8; 4]) -> Option<u8> {
    let m = u32::from_be_bytes(netmask);
    let ones = m.leading_ones();
    if ones == 32 || (m << ones) == 0 {
        u8::try_from(ones).ok()
    } else {
        None
    }
}

impl IfaceConfig {
    /// Derive the configuration from a lease.
    ///
    /// `your_ip` is the leased address, `netmask` option 1, `router` option 3,
    /// `server_id` option 54 and `dns` the first entry of option 6 - i.e.
    /// exactly the fields `btsvc_core::pan::DhcpReply` carries.
    #[must_use]
    pub fn from_lease(
        your_ip: [u8; 4],
        netmask: Option<[u8; 4]>,
        router: Option<[u8; 4]>,
        server_id: [u8; 4],
        dns: Option<[u8; 4]>,
    ) -> Self {
        let netmask = netmask.unwrap_or_else(|| classful_netmask(your_ip));
        IfaceConfig {
            ip: your_ip,
            netmask,
            broadcast: broadcast_of(your_ip, netmask),
            gateway: router.unwrap_or(server_id),
            dns,
            mtu: IFACE_MTU,
        }
    }

    /// True when the gateway is reachable on the leased subnet. A gateway
    /// outside it needs an interface route first, which we do not create -
    /// so this is worth logging rather than silently producing a default
    /// route that cannot resolve.
    #[must_use]
    pub fn gateway_on_link(&self) -> bool {
        network_of(self.gateway, self.netmask) == network_of(self.ip, self.netmask)
    }

    /// Sanity gate before anything is applied to a live interface. A lease
    /// that fails this is a lease we refuse to act on.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.ip != [0, 0, 0, 0]
            && self.ip[0] != 127
            && self.gateway != [0, 0, 0, 0]
            && prefix_len(self.netmask).is_some_and(|p| (8..=30).contains(&p))
            && self.ip != self.broadcast
            && network_of(self.ip, self.netmask) != self.ip
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Example lease with one address for the router and DNS server.
    #[test]
    fn phone_lease() {
        let c = IfaceConfig::from_lease(
            [192, 0, 2, 60],
            Some([255, 255, 255, 0]),
            Some([192, 0, 2, 197]),
            [192, 0, 2, 197],
            Some([192, 0, 2, 197]),
        );
        assert_eq!(c.ip, [192, 0, 2, 60]);
        assert_eq!(c.netmask, [255, 255, 255, 0]);
        assert_eq!(c.broadcast, [192, 0, 2, 255]);
        assert_eq!(c.gateway, [192, 0, 2, 197]);
        assert_eq!(c.dns, Some([192, 0, 2, 197]));
        assert_eq!(c.mtu, 1500);
        assert!(c.gateway_on_link());
        assert!(c.is_usable());
    }

    #[test]
    fn router_option_missing_falls_back_to_the_server_id() {
        let c = IfaceConfig::from_lease(
            [192, 168, 44, 20],
            Some([255, 255, 255, 0]),
            None,
            [192, 168, 44, 1],
            None,
        );
        assert_eq!(c.gateway, [192, 168, 44, 1]);
        assert_eq!(
            c.dns, None,
            "no DNS offered stays None \u{2014} do not invent one"
        );
        assert!(c.is_usable());
    }

    #[test]
    fn netmask_missing_falls_back_classfully() {
        // 10/8 is class A: a phone that omits option 1 on a 10.x lease gets
        // /8, which is wrong-ish but routable and is what every client does.
        let c = IfaceConfig::from_lease([10, 0, 0, 5], None, None, [10, 0, 0, 1], None);
        assert_eq!(c.netmask, [255, 0, 0, 0]);
        assert_eq!(c.broadcast, [10, 255, 255, 255]);
        let c = IfaceConfig::from_lease([172, 20, 1, 5], None, None, [172, 20, 1, 1], None);
        assert_eq!(c.netmask, [255, 255, 0, 0]);
        let c = IfaceConfig::from_lease([192, 168, 1, 5], None, None, [192, 168, 1, 1], None);
        assert_eq!(c.netmask, [255, 255, 255, 0]);
    }

    #[test]
    fn broadcast_and_network_arithmetic() {
        assert_eq!(
            broadcast_of([10, 1, 2, 3], [255, 255, 252, 0]),
            [10, 1, 3, 255]
        );
        assert_eq!(network_of([10, 1, 2, 3], [255, 255, 252, 0]), [10, 1, 0, 0]);
        assert_eq!(
            broadcast_of([1, 2, 3, 4], [255, 255, 255, 255]),
            [1, 2, 3, 4]
        );
    }

    #[test]
    fn prefix_lengths() {
        assert_eq!(prefix_len([255, 255, 255, 0]), Some(24));
        assert_eq!(prefix_len([255, 255, 255, 252]), Some(30));
        assert_eq!(prefix_len([255, 0, 0, 0]), Some(8));
        assert_eq!(prefix_len([0, 0, 0, 0]), Some(0));
        assert_eq!(prefix_len([255, 255, 255, 255]), Some(32));
        // Non-contiguous masks are refused rather than rounded.
        assert_eq!(prefix_len([255, 0, 255, 0]), None);
        assert_eq!(prefix_len([255, 255, 0, 1]), None);
    }

    #[test]
    fn unusable_leases_are_rejected() {
        // No address at all.
        assert!(
            !IfaceConfig::from_lease([0, 0, 0, 0], None, None, [10, 0, 0, 1], None).is_usable()
        );
        // Loopback.
        assert!(!IfaceConfig::from_lease(
            [127, 0, 0, 1],
            Some([255, 0, 0, 0]),
            None,
            [127, 0, 0, 1],
            None
        )
        .is_usable());
        // No gateway anywhere.
        assert!(!IfaceConfig::from_lease(
            [10, 0, 0, 5],
            Some([255, 255, 255, 0]),
            None,
            [0, 0, 0, 0],
            None
        )
        .is_usable());
        // The leased address IS the broadcast address.
        assert!(!IfaceConfig::from_lease(
            [10, 0, 0, 255],
            Some([255, 255, 255, 0]),
            Some([10, 0, 0, 1]),
            [10, 0, 0, 1],
            None
        )
        .is_usable());
        // The leased address IS the network address.
        assert!(!IfaceConfig::from_lease(
            [10, 0, 0, 0],
            Some([255, 255, 255, 0]),
            Some([10, 0, 0, 1]),
            [10, 0, 0, 1],
            None
        )
        .is_usable());
        // A /31 leaves no host addresses; refuse it rather than configure a
        // subnet where ip == broadcast.
        assert!(!IfaceConfig::from_lease(
            [10, 0, 0, 1],
            Some([255, 255, 255, 254]),
            Some([10, 0, 0, 0]),
            [10, 0, 0, 0],
            None
        )
        .is_usable());
        // Non-contiguous mask.
        assert!(!IfaceConfig::from_lease(
            [10, 0, 0, 5],
            Some([255, 0, 255, 0]),
            Some([10, 0, 0, 1]),
            [10, 0, 0, 1],
            None
        )
        .is_usable());
    }

    #[test]
    fn off_link_gateway_is_flagged_but_not_fatal() {
        let c = IfaceConfig::from_lease(
            [10, 0, 0, 5],
            Some([255, 255, 255, 0]),
            Some([10, 9, 9, 1]),
            [10, 0, 0, 1],
            None,
        );
        assert!(!c.gateway_on_link());
        // Still "usable" - the caller logs the oddity and proceeds; refusing
        // here would break the (rare, legal) point-to-point tether layout.
        assert!(c.is_usable());
    }
}
