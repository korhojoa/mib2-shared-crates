// SPDX-License-Identifier: MIT OR Apache-2.0
//! SOCKS5 protocol codec - sans-IO, CONNECT-only, no auth (RFC 1928).
//!
//! Programs reach a Bluetooth tether through a local SOCKS5 server; the caller
//! relays each accepted CONNECT over smoltcp. This module is the wire codec
//! only: bytes arrive from a TCP fd in arbitrary chunks, so parsing is an
//! incremental state machine over the caller's growing buffer. IPv4 (ATYP 1)
//! and domain (ATYP 3, resolved by the shell via the lease's DNS) targets are
//! accepted; IPv6 (ATYP 4) is rejected with 0x08, non-CONNECT commands with
//! 0x07. Per RFC 1928 the server closes after an error reply.

/// Protocol version byte.
pub const SOCKS_VER: u8 = 0x05;
/// Auth method 0x00 "NO AUTHENTICATION REQUIRED" - the only one we accept.
pub const METHOD_NO_AUTH: u8 = 0x00;
pub const CMD_CONNECT: u8 = 0x01;
pub const ATYP_IPV4: u8 = 0x01;
pub const ATYP_DOMAIN: u8 = 0x03;
pub const ATYP_IPV6: u8 = 0x04;

/// Method-select reply accepting no-auth (send after `Progress::Greeted`).
pub const GREETING_ACCEPT: [u8; 2] = [SOCKS_VER, 0x00];
/// Method-select reply "no acceptable methods" (send before close on
/// `Reject::NoAcceptableMethod`).
pub const GREETING_NO_METHOD: [u8; 2] = [SOCKS_VER, 0xFF];

// Reply (REP) codes for build_reply.
pub const REP_SUCCESS: u8 = 0x00;
pub const REP_GENERAL_FAILURE: u8 = 0x01;
pub const REP_NETWORK_UNREACHABLE: u8 = 0x03;
pub const REP_HOST_UNREACHABLE: u8 = 0x04;
pub const REP_CONNECTION_REFUSED: u8 = 0x05;
pub const REP_COMMAND_NOT_SUPPORTED: u8 = 0x07;
pub const REP_ATYP_NOT_SUPPORTED: u8 = 0x08;

/// Reply length with an IPv4 BND address - the only form we emit.
pub const REPLY_LEN: usize = 10;

/// A domain name from an ATYP 3 request. Bounded inline storage (the wire
/// length field is one byte, so 255 is the protocol maximum) - no_std-safe.
#[derive(Clone, Copy)]
pub struct Domain {
    buf: [u8; 255],
    len: u8,
}

impl Domain {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len as usize]
    }
}

impl PartialEq for Domain {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}
impl Eq for Domain {}

impl core::fmt::Debug for Domain {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match core::str::from_utf8(self.as_bytes()) {
            Ok(s) => write!(f, "Domain({s:?})"),
            Err(_) => write!(f, "Domain({:?})", self.as_bytes()),
        }
    }
}

/// Parsed CONNECT target. Domain resolution is the shell's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Fixed inline storage keeps this no_std parser allocation-free.
#[allow(clippy::large_enum_variant)]
pub enum Target {
    Ipv4 { addr: [u8; 4], port: u16 },
    Domain { name: Domain, port: u16 },
}

/// Why the handshake was refused, and what (if anything) to send before
/// closing the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// Version byte was not 0x05: RFC 1928 defines no reply - just close.
    BadVersion,
    /// Greeting offered no method 0x00: send `GREETING_NO_METHOD`, close.
    NoAcceptableMethod,
    /// Bad request: send `build_reply` with this REP code, close.
    Request(u8),
}

/// One step of the handshake. "Need more bytes" is distinct from any error;
/// `consumed` tells the caller how much to drain from its buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Fixed inline storage keeps this no_std parser allocation-free.
#[allow(clippy::large_enum_variant)]
pub enum Progress {
    /// Input incomplete - read more and call `advance` again.
    NeedMore,
    /// Greeting accepted: drain `consumed` bytes, send `GREETING_ACCEPT`,
    /// keep calling `advance` for the request.
    Greeted { consumed: usize },
    /// Request parsed: drain `consumed` bytes, connect to `target`, then
    /// send `build_reply` (`REP_SUCCESS` or an error code). Terminal.
    Connect { consumed: usize, target: Target },
    /// Protocol error - see `Reject` for the goodbye bytes. Terminal.
    Reject(Reject),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Greeting,
    Request,
    Done,
}

/// Incremental server-side handshake: greeting -> method select ->
/// request -> target. Call `advance` with all unconsumed bytes after every
/// read, in a loop until it returns `NeedMore`; after `Connect` or `Reject`
/// the handshake is over (further calls return `NeedMore`).
#[derive(Debug)]
pub struct Socks5Handshake {
    state: State,
}

impl Socks5Handshake {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: State::Greeting,
        }
    }

    pub fn advance(&mut self, buf: &[u8]) -> Progress {
        match self.state {
            State::Greeting => self.greeting(buf),
            State::Request => self.request(buf),
            State::Done => Progress::NeedMore,
        }
    }

    fn reject(&mut self, r: Reject) -> Progress {
        self.state = State::Done;
        Progress::Reject(r)
    }

    /// `05 NMETHODS methods...` - accept iff methods contain 0x00.
    fn greeting(&mut self, buf: &[u8]) -> Progress {
        if buf.is_empty() {
            return Progress::NeedMore;
        }
        if buf[0] != SOCKS_VER {
            return self.reject(Reject::BadVersion);
        }
        if buf.len() < 2 {
            return Progress::NeedMore;
        }
        let n = buf[1] as usize;
        if buf.len() < 2 + n {
            return Progress::NeedMore;
        }
        // NMETHODS 0 falls through: an empty list cannot contain 0x00.
        if !buf[2..2 + n].contains(&METHOD_NO_AUTH) {
            return self.reject(Reject::NoAcceptableMethod);
        }
        self.state = State::Request;
        Progress::Greeted { consumed: 2 + n }
    }

    /// `05 CMD 00 ATYP addr port` - errors are decided as soon as the
    /// deciding byte is present (an unknown ATYP has no knowable length,
    /// so waiting for a full message is not even possible).
    fn request(&mut self, buf: &[u8]) -> Progress {
        if buf.is_empty() {
            return Progress::NeedMore;
        }
        if buf[0] != SOCKS_VER {
            return self.reject(Reject::BadVersion);
        }
        if buf.len() < 2 {
            return Progress::NeedMore;
        }
        if buf[1] != CMD_CONNECT {
            return self.reject(Reject::Request(REP_COMMAND_NOT_SUPPORTED));
        }
        if buf.len() < 3 {
            return Progress::NeedMore;
        }
        if buf[2] != 0x00 {
            return self.reject(Reject::Request(REP_GENERAL_FAILURE));
        }
        if buf.len() < 4 {
            return Progress::NeedMore;
        }
        match buf[3] {
            ATYP_IPV4 => {
                const LEN: usize = 4 + 4 + 2;
                if buf.len() < LEN {
                    return Progress::NeedMore;
                }
                let addr = [buf[4], buf[5], buf[6], buf[7]];
                let port = u16::from_be_bytes([buf[8], buf[9]]);
                self.state = State::Done;
                Progress::Connect {
                    consumed: LEN,
                    target: Target::Ipv4 { addr, port },
                }
            }
            ATYP_DOMAIN => {
                if buf.len() < 5 {
                    return Progress::NeedMore;
                }
                let n = buf[4] as usize;
                if n == 0 {
                    return self.reject(Reject::Request(REP_GENERAL_FAILURE));
                }
                let total = 5 + n + 2;
                if buf.len() < total {
                    return Progress::NeedMore;
                }
                let mut name = Domain {
                    buf: [0; 255],
                    len: u8::try_from(n).expect("one-byte domain length"),
                };
                name.buf[..n].copy_from_slice(&buf[5..5 + n]);
                let port = u16::from_be_bytes([buf[5 + n], buf[6 + n]]);
                self.state = State::Done;
                Progress::Connect {
                    consumed: total,
                    target: Target::Domain { name, port },
                }
            }
            // ATYP_IPV6 and anything unknown.
            _ => self.reject(Reject::Request(REP_ATYP_NOT_SUPPORTED)),
        }
    }
}

impl Default for Socks5Handshake {
    fn default() -> Self {
        Self::new()
    }
}

/// Encode `05 REP 00 01 bnd_ip bnd_port` (10 bytes). `REP_SUCCESS` after a
/// successful connect (bnd = the relay's bound address, 0.0.0.0:0 is fine),
/// or an error code - the server closes after sending an error reply.
pub fn build_reply(out: &mut [u8], rep: u8, bnd_ip: [u8; 4], bnd_port: u16) -> Option<usize> {
    if out.len() < REPLY_LEN {
        return None;
    }
    out[0] = SOCKS_VER;
    out[1] = rep;
    out[2] = 0x00;
    out[3] = ATYP_IPV4;
    out[4..8].copy_from_slice(&bnd_ip);
    out[8..10].copy_from_slice(&bnd_port.to_be_bytes());
    Some(REPLY_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Golden client streams: greeting (no-auth offered) + CONNECT request.
    const GREETING: &[u8] = &[0x05, 0x01, 0x00];
    const REQ_IPV4: &[u8] = &[0x05, 0x01, 0x00, 0x01, 1, 2, 3, 4, 0x00, 0x50]; // 1.2.3.4:80
    const REQ_DOMAIN: &[u8] = &[
        0x05, 0x01, 0x00, 0x03, 11, b'e', b'x', b'a', b'm', b'p', b'l', b'e', b'.', b'c', b'o',
        b'm', 0x01, 0xBB, // example.com:443
    ];

    fn domain(s: &[u8]) -> Domain {
        let mut d = Domain {
            buf: [0; 255],
            len: u8::try_from(s.len()).unwrap(),
        };
        d.buf[..s.len()].copy_from_slice(s);
        d
    }

    /// Feed the whole stream through a fresh handshake, draining `consumed`
    /// like a real caller; returns every non-NeedMore step.
    fn drive(stream: &[u8], chunk: usize) -> Vec<Progress> {
        let mut hs = Socks5Handshake::new();
        let mut buf: Vec<u8> = Vec::new();
        let mut steps = Vec::new();
        for c in stream.chunks(chunk.max(1)) {
            buf.extend_from_slice(c);
            loop {
                match hs.advance(&buf) {
                    Progress::NeedMore => break,
                    step => {
                        match step {
                            Progress::Greeted { consumed } | Progress::Connect { consumed, .. } => {
                                buf.drain(..consumed);
                            }
                            _ => {}
                        }
                        let terminal = !matches!(step, Progress::Greeted { .. });
                        steps.push(step);
                        if terminal {
                            return steps;
                        }
                    }
                }
            }
        }
        steps
    }

    #[test]
    fn ipv4_connect_golden() {
        let stream: Vec<u8> = [GREETING, REQ_IPV4].concat();
        let steps = drive(&stream, stream.len());
        assert_eq!(
            steps,
            vec![
                Progress::Greeted { consumed: 3 },
                Progress::Connect {
                    consumed: 10,
                    target: Target::Ipv4 {
                        addr: [1, 2, 3, 4],
                        port: 80
                    },
                },
            ]
        );
    }

    #[test]
    fn domain_connect_golden() {
        let stream: Vec<u8> = [GREETING, REQ_DOMAIN].concat();
        let steps = drive(&stream, stream.len());
        assert_eq!(steps.len(), 2);
        assert_eq!(
            steps[1],
            Progress::Connect {
                consumed: REQ_DOMAIN.len(),
                target: Target::Domain {
                    name: domain(b"example.com"),
                    port: 443
                },
            }
        );
    }

    /// One byte at a time reaches the identical result - the sans-IO point.
    #[test]
    fn chunked_delivery_byte_by_byte() {
        for stream in [
            [GREETING, REQ_IPV4].concat(),
            [GREETING, REQ_DOMAIN].concat(),
        ] {
            let whole = drive(&stream, stream.len());
            let bytewise = drive(&stream, 1);
            assert_eq!(whole, bytewise);
        }
        // A greeting offering several methods, chunked, still selects 0x00.
        let multi = [&[0x05u8, 0x03, 0x02, 0x00, 0x01][..], REQ_IPV4].concat();
        assert_eq!(drive(&multi, 1)[0], Progress::Greeted { consumed: 5 });
    }

    #[test]
    fn no_acceptable_method() {
        // gssapi + user/pass offered, no 0x00
        let steps = drive(&[0x05, 0x02, 0x01, 0x02], 1);
        assert_eq!(steps, vec![Progress::Reject(Reject::NoAcceptableMethod)]);
        // zero methods offered
        let steps = drive(&[0x05, 0x00], 1);
        assert_eq!(steps, vec![Progress::Reject(Reject::NoAcceptableMethod)]);
    }

    #[test]
    fn wrong_version() {
        // SOCKS4 greeting: no reply, just close
        assert_eq!(
            drive(&[0x04, 0x01, 0x00], 1),
            vec![Progress::Reject(Reject::BadVersion)]
        );
        // wrong version resent in the request
        let stream = [GREETING, &[0x04, 0x01, 0x00, 0x01]].concat();
        assert_eq!(
            drive(&stream, 1),
            vec![
                Progress::Greeted { consumed: 3 },
                Progress::Reject(Reject::BadVersion)
            ]
        );
    }

    #[test]
    fn bind_and_udp_rejected() {
        for cmd in [0x02u8, 0x03] {
            let stream = [GREETING, &[0x05, cmd, 0x00, 0x01, 1, 2, 3, 4, 0, 80]].concat();
            assert_eq!(
                drive(&stream, 1).last(),
                Some(&Progress::Reject(Reject::Request(
                    REP_COMMAND_NOT_SUPPORTED
                )))
            );
        }
    }

    #[test]
    fn ipv6_and_unknown_atyp_rejected() {
        for atyp in [ATYP_IPV6, 0x05] {
            let stream = [GREETING, &[0x05, 0x01, 0x00, atyp]].concat();
            assert_eq!(
                drive(&stream, 1).last(),
                Some(&Progress::Reject(Reject::Request(REP_ATYP_NOT_SUPPORTED)))
            );
        }
    }

    #[test]
    fn zero_length_domain_rejected() {
        let stream = [GREETING, &[0x05, 0x01, 0x00, 0x03, 0x00, 0x01, 0xBB]].concat();
        assert_eq!(
            drive(&stream, 1).last(),
            Some(&Progress::Reject(Reject::Request(REP_GENERAL_FAILURE)))
        );
    }

    #[test]
    fn nonzero_reserved_rejected() {
        let stream = [GREETING, &[0x05, 0x01, 0x01, 0x01]].concat();
        assert_eq!(
            drive(&stream, 1).last(),
            Some(&Progress::Reject(Reject::Request(REP_GENERAL_FAILURE)))
        );
    }

    #[test]
    fn reply_encoder_golden() {
        let mut buf = [0u8; 16];
        let n = build_reply(&mut buf, REP_SUCCESS, [10, 0, 0, 1], 1080).unwrap();
        assert_eq!(n, REPLY_LEN);
        assert_eq!(
            &buf[..n],
            &[0x05, 0x00, 0x00, 0x01, 10, 0, 0, 1, 0x04, 0x38]
        );
        // error replies carry the code, bnd 0.0.0.0:0
        for rep in [
            REP_GENERAL_FAILURE,
            REP_NETWORK_UNREACHABLE,
            REP_HOST_UNREACHABLE,
            REP_CONNECTION_REFUSED,
            REP_COMMAND_NOT_SUPPORTED,
            REP_ATYP_NOT_SUPPORTED,
        ] {
            let n = build_reply(&mut buf, rep, [0; 4], 0).unwrap();
            assert_eq!(&buf[..n], &[0x05, rep, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
        }
        // short output buffer refused
        assert!(build_reply(&mut buf[..9], REP_SUCCESS, [0; 4], 0).is_none());
        // greeting reply constants
        assert_eq!(GREETING_ACCEPT, [0x05, 0x00]);
        assert_eq!(GREETING_NO_METHOD, [0x05, 0xFF]);
    }

    /// Every truncation of every stream (good and bad) parses without
    /// panicking and never invents a Connect.
    #[test]
    fn truncation_never_panics() {
        let streams: Vec<Vec<u8>> = vec![
            [GREETING, REQ_IPV4].concat(),
            [GREETING, REQ_DOMAIN].concat(),
            vec![0x05, 0x02, 0x01, 0x02],
            [GREETING, &[0x05, 0x02, 0x00, 0x01, 1, 2, 3, 4, 0, 80]].concat(),
        ];
        for s in &streams {
            let full = drive(s, 1);
            for k in 0..s.len() {
                let partial = drive(&s[..k], 1);
                // a prefix of the steps of the full stream, never more
                assert!(partial.len() <= full.len());
                assert_eq!(partial[..], full[..partial.len()]);
            }
        }
        // truncated buffers fed whole (no chunking) also never panic
        let all: Vec<u8> = [GREETING, REQ_DOMAIN].concat();
        for k in 0..all.len() {
            let mut hs = Socks5Handshake::new();
            let _ = hs.advance(&all[..k]);
        }
    }

    /// Terminal states stay terminal: advance after Connect/Reject is inert.
    #[test]
    fn terminal_is_inert() {
        let stream: Vec<u8> = [GREETING, REQ_IPV4].concat();
        let mut hs = Socks5Handshake::new();
        assert!(matches!(hs.advance(&stream), Progress::Greeted { .. }));
        assert!(matches!(hs.advance(&stream[3..]), Progress::Connect { .. }));
        assert_eq!(hs.advance(&stream[3..]), Progress::NeedMore);
        let mut hs = Socks5Handshake::new();
        assert_eq!(hs.advance(&[0x04]), Progress::Reject(Reject::BadVersion));
        assert_eq!(hs.advance(&[0x04]), Progress::NeedMore);
    }
}

#[cfg(test)]
mod car_repro {
    use super::*;

    /// A greeting and a request can arrive in one buffer.
    /// The parser must preserve the request after it accepts the greeting.
    #[test]
    fn greeting_and_request_in_one_buffer() {
        let mut hs = Socks5Handshake::new();
        let mut buf: Vec<u8> = vec![0x05, 0x01, 0x00];
        buf.extend_from_slice(&[0x05, 0x01, 0x00, 0x01, 93, 184, 216, 34, 0x00, 0x50]);

        match hs.advance(&buf) {
            Progress::Greeted { consumed } => {
                assert_eq!(
                    consumed, 3,
                    "greeting must consume exactly VER NMETHODS METHOD"
                );
                buf.drain(..consumed);
            }
            other => panic!("expected Greeted, got {other:?}"),
        }
        match hs.advance(&buf) {
            Progress::Connect { consumed, target } => {
                assert_eq!(consumed, 10);
                assert_eq!(
                    target,
                    Target::Ipv4 {
                        addr: [93, 184, 216, 34],
                        port: 80
                    }
                );
            }
            other => panic!("expected Connect, got {other:?}"),
        }
    }
}
