// SPDX-License-Identifier: MIT OR Apache-2.0
//! BT-PAN (BNEP) link-layer helpers - sans-IO Ethernet/ARP codec.
//!
//! BNEP carries raw Ethernet frames. These functions parse headers and make
//! ARP requests without an IP stack.

/// Ethernet header length (no VLAN).
pub const ETH_HDR_LEN: usize = 14;
pub const ETHERTYPE_ARP: u16 = 0x0806;
pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_IPV6: u16 = 0x86DD;

/// The conventional Android Bluetooth tether gateway address.
pub const ANDROID_BT_TETHER_GATEWAY: [u8; 4] = [192, 168, 44, 1];

/// Parsed Ethernet header (payload = frame[`ETH_HDR_LEN`..]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EthHeader {
    pub dst: [u8; 6],
    pub src: [u8; 6],
    pub ethertype: u16,
}

#[must_use]
pub fn parse_eth_header(frame: &[u8]) -> Option<EthHeader> {
    if frame.len() < ETH_HDR_LEN {
        return None;
    }
    let mut dst = [0u8; 6];
    let mut src = [0u8; 6];
    dst.copy_from_slice(&frame[0..6]);
    src.copy_from_slice(&frame[6..12]);
    Some(EthHeader {
        dst,
        src,
        ethertype: (u16::from(frame[12]) << 8) | u16::from(frame[13]),
    })
}

/// Build a broadcast ARP request `who-has target_ip tell sender_ip` from
/// `our_mac`. 42 bytes (Ethernet + ARP); BNEP pads/accepts short frames.
/// With `sender_ip` 0.0.0.0 this doubles as a DHCP-style probe that never
/// claims an address.
pub fn build_arp_probe(
    out: &mut [u8],
    our_mac: &[u8; 6],
    sender_ip: [u8; 4],
    target_ip: [u8; 4],
) -> Option<usize> {
    const LEN: usize = ETH_HDR_LEN + 28;
    if out.len() < LEN {
        return None;
    }
    out[0..6].fill(0xff); // broadcast
    out[6..12].copy_from_slice(our_mac);
    out[12..14].copy_from_slice(&ETHERTYPE_ARP.to_be_bytes());
    let a = &mut out[ETH_HDR_LEN..LEN];
    a[0..2].copy_from_slice(&1u16.to_be_bytes()); // htype: Ethernet
    a[2..4].copy_from_slice(&ETHERTYPE_IPV4.to_be_bytes()); // ptype: IPv4
    a[4] = 6; // hlen
    a[5] = 4; // plen
    a[6..8].copy_from_slice(&1u16.to_be_bytes()); // op: request
    a[8..14].copy_from_slice(our_mac); // sender hw
    a[14..18].copy_from_slice(&sender_ip);
    a[18..24].fill(0); // target hw: unknown
    a[24..28].copy_from_slice(&target_ip);
    Some(LEN)
}

/// Read the source IPv4 address from an Ethernet frame.
#[must_use]
pub fn ipv4_source(frame: &[u8]) -> Option<[u8; 4]> {
    let h = parse_eth_header(frame)?;
    if h.ethertype != ETHERTYPE_IPV4 || frame.len() < ETH_HDR_LEN + 20 {
        return None;
    }
    let ip = &frame[ETH_HDR_LEN..];
    if ip[0] >> 4 != 4 {
        return None;
    }
    let mut src = [0u8; 4];
    src.copy_from_slice(&ip[12..16]);
    Some(src)
}

/// If `frame` is an ARP reply for `target_ip` (i.e. from the probed host),
/// return the responder's MAC - the stage-1 "link is alive both ways" proof.
#[must_use]
pub fn arp_reply_from(frame: &[u8], target_ip: [u8; 4]) -> Option<[u8; 6]> {
    let h = parse_eth_header(frame)?;
    if h.ethertype != ETHERTYPE_ARP || frame.len() < ETH_HDR_LEN + 28 {
        return None;
    }
    let a = &frame[ETH_HDR_LEN..];
    let op = (u16::from(a[6]) << 8) | u16::from(a[7]);
    if op != 2 {
        return None; // not a reply
    }
    if a[14..18] != target_ip {
        return None; // reply from someone else
    }
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&a[8..14]);
    Some(mac)
}

// --- DHCP (the negotiated way onto the tether: DISCOVER/OFFER/REQUEST/ACK) --

const BOOTP_FIXED: usize = 236;
const DHCP_COOKIE: [u8; 4] = [0x63, 0x82, 0x53, 0x63];
const IP_HDR: usize = 20;
const UDP_HDR: usize = 8;

/// RFC 1071 ones'-complement sum (IPv4 header checksum).
fn ip_checksum(hdr: &[u8]) -> u16 {
    let mut sum = 0u32;
    let mut i = 0;
    while i + 1 < hdr.len() {
        sum += (u32::from(hdr[i]) << 8) | u32::from(hdr[i + 1]);
        i += 2;
    }
    if i < hdr.len() {
        sum += u32::from(hdr[i]) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !u16::from_le_bytes([sum.to_le_bytes()[0], sum.to_le_bytes()[1]])
}

/// Build a DHCP message (op codes: 1=DISCOVER, 3=REQUEST) as a full broadcast
/// Ethernet frame: eth | IPv4 (0.0.0.0 -> 255.255.255.255, UDP) | UDP 68->67 |
/// BOOTP + options. UDP checksum 0 (optional over IPv4). `req` carries
/// (`offered_ip`, `server_id`) for REQUEST.
fn build_dhcp(
    out: &mut [u8],
    our_mac: [u8; 6],
    xid: u32,
    msg_type: u8,
    req: Option<([u8; 4], [u8; 4])>,
) -> Option<usize> {
    // options: 53(1) + 61(7) + 55(4) [+ 50(6) + 54(6)] + 255 + pad to even
    let opt_len = 3 + 9 + 6 + if req.is_some() { 12 } else { 0 } + 1;
    let bootp_len = BOOTP_FIXED + 4 + opt_len;
    let total = ETH_HDR_LEN + IP_HDR + UDP_HDR + bootp_len;
    if out.len() < total {
        return None;
    }
    out[..total].fill(0);
    // Ethernet
    out[0..6].fill(0xff);
    out[6..12].copy_from_slice(&our_mac);
    out[12..14].copy_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
    // IPv4
    let ip_len = u16::try_from(IP_HDR + UDP_HDR + bootp_len).ok()?;
    {
        let ip = &mut out[ETH_HDR_LEN..ETH_HDR_LEN + IP_HDR];
        ip[0] = 0x45;
        ip[2..4].copy_from_slice(&ip_len.to_be_bytes());
        ip[8] = 64; // ttl
        ip[9] = 17; // udp
                    // src 0.0.0.0 already; dst broadcast:
        ip[16..20].fill(0xff);
        let csum = ip_checksum(&ip[..IP_HDR]);
        ip[10..12].copy_from_slice(&csum.to_be_bytes());
    }
    // UDP 68 -> 67
    {
        let udp = &mut out[ETH_HDR_LEN + IP_HDR..ETH_HDR_LEN + IP_HDR + UDP_HDR];
        udp[0..2].copy_from_slice(&68u16.to_be_bytes());
        udp[2..4].copy_from_slice(&67u16.to_be_bytes());
        udp[4..6].copy_from_slice(&(u16::try_from(UDP_HDR + bootp_len).ok()?).to_be_bytes());
        // checksum 0 = none (legal for IPv4 UDP)
    }
    // BOOTP
    let b = ETH_HDR_LEN + IP_HDR + UDP_HDR;
    out[b] = 1; // op: BOOTREQUEST
    out[b + 1] = 1; // htype ethernet
    out[b + 2] = 6; // hlen
    out[b + 4..b + 8].copy_from_slice(&xid.to_be_bytes());
    out[b + 10] = 0x80; // flags: BROADCAST reply (we have no IP yet)
    out[b + 28..b + 34].copy_from_slice(&our_mac);
    let mut o = b + BOOTP_FIXED;
    out[o..o + 4].copy_from_slice(&DHCP_COOKIE);
    o += 4;
    out[o] = 53;
    out[o + 1] = 1;
    out[o + 2] = msg_type;
    o += 3;
    out[o] = 61; // client id
    out[o + 1] = 7;
    out[o + 2] = 1;
    out[o + 3..o + 9].copy_from_slice(&our_mac);
    o += 9;
    out[o] = 55; // param request: mask, router, dns
    out[o + 1] = 3;
    out[o + 2] = 1;
    out[o + 3] = 3;
    out[o + 4] = 6;
    o += 5;
    // (55 has len 3 -> wrote 5 bytes; opt_len accounted 6 incl. one pad)
    o += 1;
    if let Some((ip, server)) = req {
        out[o] = 50; // requested ip
        out[o + 1] = 4;
        out[o + 2..o + 6].copy_from_slice(&ip);
        o += 6;
        out[o] = 54; // server id
        out[o + 1] = 4;
        out[o + 2..o + 6].copy_from_slice(&server);
        o += 6;
    }
    out[o] = 255;
    Some(total)
}

pub fn build_dhcp_discover(out: &mut [u8], our_mac: &[u8; 6], xid: u32) -> Option<usize> {
    build_dhcp(out, *our_mac, xid, 1, None)
}

pub fn build_dhcp_request(
    out: &mut [u8],
    our_mac: &[u8; 6],
    xid: u32,
    offered_ip: [u8; 4],
    server_id: [u8; 4],
) -> Option<usize> {
    build_dhcp(out, *our_mac, xid, 3, Some((offered_ip, server_id)))
}

/// A parsed DHCP server reply relevant to us (xid + chaddr matched).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DhcpReply {
    /// 2 = OFFER, 5 = ACK, 6 = NAK.
    pub msg_type: u8,
    pub your_ip: [u8; 4],
    pub server_id: [u8; 4],
    pub router: Option<[u8; 4]>,
    pub netmask: Option<[u8; 4]>,
    /// First server from option 6 (we request it in option 55; stage 2's
    /// resolver needs it - phones hand out their own tether address here).
    pub dns: Option<[u8; 4]>,
    /// Option 51, seconds. A phone tether hands out SHORT leases and stops
    /// forwarding for an address whose lease lapsed - renew at T1 (half).
    pub lease_secs: Option<u32>,
}

/// Parse a frame as a DHCP reply to OUR transaction. None if it's anything
/// else. Tolerates broadcast or unicast delivery.
#[must_use]
pub fn parse_dhcp_reply(frame: &[u8], xid: u32, our_mac: &[u8; 6]) -> Option<DhcpReply> {
    let h = parse_eth_header(frame)?;
    if h.ethertype != ETHERTYPE_IPV4 {
        return None;
    }
    let ip = frame.get(ETH_HDR_LEN..)?;
    if ip.len() < IP_HDR || ip[0] >> 4 != 4 || ip[9] != 17 {
        return None;
    }
    let ihl = ((ip[0] & 0x0f) as usize) * 4;
    let udp = ip.get(ihl..)?;
    if udp.len() < UDP_HDR {
        return None;
    }
    let dst_port = (u16::from(udp[2]) << 8) | u16::from(udp[3]);
    if dst_port != 68 {
        return None;
    }
    let b = udp.get(UDP_HDR..)?;
    if b.len() < BOOTP_FIXED + 4 || b[0] != 2 {
        return None; // not a BOOTREPLY
    }
    if b[4..8] != xid.to_be_bytes() || b[28..34] != *our_mac {
        return None; // someone else's transaction
    }
    if b[BOOTP_FIXED..BOOTP_FIXED + 4] != DHCP_COOKIE {
        return None;
    }
    let mut your_ip = [0u8; 4];
    your_ip.copy_from_slice(&b[16..20]);
    let mut reply = DhcpReply {
        msg_type: 0,
        your_ip,
        server_id: [0; 4],
        router: None,
        netmask: None,
        dns: None,
        lease_secs: None,
    };
    let mut o = BOOTP_FIXED + 4;
    while o + 1 < b.len() {
        let (code, len) = (b[o], b[o + 1] as usize);
        if code == 255 {
            break;
        }
        if code == 0 {
            o += 1;
            continue; // pad
        }
        let val = b.get(o + 2..o + 2 + len)?;
        match (code, len) {
            (53, 1) => reply.msg_type = val[0],
            (54, 4) => reply.server_id = [val[0], val[1], val[2], val[3]],
            (3, l) if l >= 4 => reply.router = Some([val[0], val[1], val[2], val[3]]),
            (1, 4) => reply.netmask = Some([val[0], val[1], val[2], val[3]]),
            (6, l) if l >= 4 => reply.dns = Some([val[0], val[1], val[2], val[3]]),
            (51, 4) => {
                reply.lease_secs = Some(u32::from_be_bytes([val[0], val[1], val[2], val[3]]));
            }
            _ => {}
        }
        o += 2 + len;
    }
    if reply.msg_type == 0 {
        return None;
    }
    Some(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [0x00, 0x1A, 0x7D, 0xDA, 0x71, 0x14];

    #[test]
    fn arp_probe_golden() {
        let mut buf = [0u8; 64];
        let n = build_arp_probe(&mut buf, &MAC, [0, 0, 0, 0], ANDROID_BT_TETHER_GATEWAY).unwrap();
        assert_eq!(n, 42);
        assert_eq!(&buf[0..6], &[0xff; 6]); // broadcast dst
        assert_eq!(&buf[6..12], &MAC);
        assert_eq!(&buf[12..14], &[0x08, 0x06]); // ARP
        assert_eq!(&buf[14..22], &[0, 1, 8, 0, 6, 4, 0, 1]); // eth/ipv4/req
        assert_eq!(&buf[22..28], &MAC); // sender hw
        assert_eq!(&buf[28..32], &[0, 0, 0, 0]); // sender ip
        assert_eq!(&buf[38..42], &[192, 168, 44, 1]); // target ip
        assert!(build_arp_probe(&mut [0u8; 10], &MAC, [0; 4], [0; 4]).is_none());
    }

    #[test]
    fn dhcp_discover_wire_format() {
        let mut buf = [0u8; 400];
        let n = build_dhcp_discover(&mut buf, &MAC, 0xDEAD_BEEF).unwrap();
        // eth: broadcast, IPv4
        assert_eq!(&buf[0..6], &[0xff; 6]);
        assert_eq!(&buf[12..14], &[0x08, 0x00]);
        // ip: v4/ihl5, proto udp, 0.0.0.0 -> 255.255.255.255
        assert_eq!(buf[14], 0x45);
        assert_eq!(buf[23], 17);
        assert_eq!(&buf[26..30], &[0, 0, 0, 0]);
        assert_eq!(&buf[30..34], &[255, 255, 255, 255]);
        // ip total length matches the frame
        let ip_len = ((buf[16] as usize) << 8) | buf[17] as usize;
        assert_eq!(ETH_HDR_LEN + ip_len, n);
        // header checksum verifies (sum over header == 0 when valid)
        let mut hdr = [0u8; 20];
        hdr.copy_from_slice(&buf[14..34]);
        let stored = (u16::from(hdr[10]) << 8) | u16::from(hdr[11]);
        hdr[10] = 0;
        hdr[11] = 0;
        assert_eq!(ip_checksum(&hdr), stored);
        // udp 68 -> 67
        assert_eq!(&buf[34..38], &[0, 68, 0, 67]);
        // bootp: op 1, xid, broadcast flag, chaddr
        assert_eq!(buf[42], 1);
        assert_eq!(&buf[46..50], &0xDEAD_BEEFu32.to_be_bytes());
        assert_eq!(buf[52], 0x80);
        assert_eq!(&buf[70..76], &MAC);
        // cookie + option 53 = DISCOVER
        let b = 42;
        assert_eq!(&buf[b + 236..b + 240], &DHCP_COOKIE);
        assert_eq!(&buf[b + 240..b + 243], &[53, 1, 1]);
    }

    /// DORA round trip against a synthetic server reply.
    #[test]
    fn dhcp_reply_parse() {
        let mut buf = [0u8; 400];
        let packet_len = build_dhcp_discover(&mut buf, &MAC, 0x1234_5678).unwrap();
        // mutate our own DISCOVER into the server's OFFER
        buf[34..38].copy_from_slice(&[0, 67, 0, 68]); // reply direction 67 -> 68
        buf[42] = 2; // BOOTREPLY
        buf[58..62].copy_from_slice(&[192, 168, 44, 17]); // yiaddr
        let bootp_offset = 42;
        buf[bootp_offset + 240..bootp_offset + 243].copy_from_slice(&[53, 1, 2]); // OFFER
                                                                                  // replace client-id option with server-id + router
        buf[bootp_offset + 243..bootp_offset + 249].copy_from_slice(&[54, 4, 192, 168, 44, 1]);
        buf[bootp_offset + 249..bootp_offset + 255].copy_from_slice(&[3, 4, 192, 168, 44, 1]);
        buf[bootp_offset + 255..bootp_offset + 261].copy_from_slice(&[6, 4, 10, 74, 7, 7]); // dns
        buf[bootp_offset + 261..bootp_offset + 267].copy_from_slice(&[51, 4, 0, 0, 0x0e, 0x10]); // lease 3600
        buf[bootp_offset + 267] = 255;
        // options grew past the DISCOVER-sized frame; the parser bounds on
        // the slice, not the IP length field, and buf has room
        let packet_len = packet_len + 13;
        let reply = parse_dhcp_reply(&buf[..packet_len], 0x1234_5678, &MAC).unwrap();
        assert_eq!(reply.msg_type, 2);
        assert_eq!(reply.your_ip, [192, 168, 44, 17]);
        assert_eq!(reply.server_id, [192, 168, 44, 1]);
        assert_eq!(reply.router, Some([192, 168, 44, 1]));
        assert_eq!(reply.dns, Some([10, 74, 7, 7]));
        assert_eq!(reply.lease_secs, Some(3600));
        // wrong xid / wrong mac -> not ours
        assert!(parse_dhcp_reply(&buf[..packet_len], 0x1111_1111, &MAC).is_none());
        assert!(parse_dhcp_reply(&buf[..packet_len], 0x1234_5678, &[9; 6]).is_none());
        // truncations never panic
        for truncated_len in 0..packet_len {
            let _ = parse_dhcp_reply(&buf[..truncated_len], 0x1234_5678, &MAC);
        }
        // REQUEST carries the offered ip + server id options
        let mut rq = [0u8; 400];
        let request_len =
            build_dhcp_request(&mut rq, &MAC, 0x1234_5678, reply.your_ip, reply.server_id).unwrap();
        let opts = &rq[42 + 240..request_len];
        let has = |code: u8, val: &[u8]| {
            let mut option_offset = 0;
            while option_offset + 1 < opts.len() && opts[option_offset] != 255 {
                if opts[option_offset] == 0 {
                    option_offset += 1;
                    continue;
                }
                let option_len = opts[option_offset + 1] as usize;
                if opts[option_offset] == code
                    && &opts[option_offset + 2..option_offset + 2 + option_len] == val
                {
                    return true;
                }
                option_offset += 2 + option_len;
            }
            false
        };
        assert!(has(53, &[3])); // REQUEST
        assert!(has(50, &[192, 168, 44, 17]));
        assert!(has(54, &[192, 168, 44, 1]));
        assert!(has(55, &[1, 3, 6])); // param request incl. dns
    }

    #[test]
    fn ipv4_source_extraction() {
        let mut f = [0u8; 34];
        f[12..14].copy_from_slice(&[0x08, 0x00]); // IPv4
        f[14] = 0x45; // v4, ihl 5
        f[26..30].copy_from_slice(&[192, 168, 44, 1]);
        assert_eq!(ipv4_source(&f), Some([192, 168, 44, 1]));
        f[12..14].copy_from_slice(&[0x86, 0xDD]); // IPv6 -> None
        assert_eq!(ipv4_source(&f), None);
        for n in 0..f.len() {
            let _ = ipv4_source(&f[..n]);
        }
    }

    #[test]
    fn eth_parse_and_arp_reply() {
        let phone_mac = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        // hand-built reply: who-has answered by the gateway
        let mut f = [0u8; 42];
        f[0..6].copy_from_slice(&MAC);
        f[6..12].copy_from_slice(&phone_mac);
        f[12..14].copy_from_slice(&[0x08, 0x06]);
        f[14..22].copy_from_slice(&[0, 1, 8, 0, 6, 4, 0, 2]); // op=reply
        f[22..28].copy_from_slice(&phone_mac); // sender hw = responder
        f[28..32].copy_from_slice(&ANDROID_BT_TETHER_GATEWAY); // sender ip
        f[32..38].copy_from_slice(&MAC);
        // target ip = ours (0.0.0.0)
        let h = parse_eth_header(&f).unwrap();
        assert_eq!(h.ethertype, ETHERTYPE_ARP);
        assert_eq!(h.src, phone_mac);
        assert_eq!(
            arp_reply_from(&f, ANDROID_BT_TETHER_GATEWAY),
            Some(phone_mac)
        );
        // wrong responder ip -> None
        assert_eq!(arp_reply_from(&f, [10, 0, 0, 1]), None);
        // an ARP REQUEST from the phone is not a reply
        f[21] = 1;
        assert_eq!(arp_reply_from(&f, ANDROID_BT_TETHER_GATEWAY), None);
        // truncated frames never panic
        for n in 0..f.len() {
            let _ = parse_eth_header(&f[..n]);
            let _ = arp_reply_from(&f[..n], ANDROID_BT_TETHER_GATEWAY);
        }
    }
}
