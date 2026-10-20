// SPDX-License-Identifier: MIT OR Apache-2.0
//! Android Auto Wireless frame codec and handshake state.
//!
//! The caller supplies RFCOMM transport. Each frame has a big-endian length
//! and message ID followed by a proto2 payload. `WifiInfoResponse` uses fields
//! ssid=1, key=2 and bssid=3.

/// Frame header length.
pub const HEADER_LEN: usize = 4;
/// RFCOMM MTU the shell registers with; also bounds our frame buffers.
pub const RFCOMM_MTU: usize = 512;

/// `MessageId` (aa-proxy-rs `MessageId` enum).
pub mod msg {
    pub const WIFI_START_REQUEST: u16 = 1;
    pub const WIFI_INFO_REQUEST: u16 = 2;
    pub const WIFI_INFO_RESPONSE: u16 = 3;
    pub const WIFI_VERSION_REQUEST: u16 = 4;
    pub const WIFI_VERSION_RESPONSE: u16 = 5;
    pub const WIFI_CONNECT_STATUS: u16 = 6;
    pub const WIFI_START_RESPONSE: u16 = 7;
    pub const WIFI_PING_REQUEST: u16 = 8;
    pub const WIFI_PING_RESPONSE: u16 = 9;
    pub const WIFI_SETUP_INFO: u16 = 11;
}

/// WifiInfoResponse.SecurityMode `WPA2_PERSONAL`.
pub const SEC_WPA2_PERSONAL: u32 = 8;
/// WifiInfoResponse.AccessPointType DYNAMIC.
pub const AP_TYPE_DYNAMIC: u32 = 1;

// --- proto2 primitives (LEB128 varint; wire types 0 and 2 only) -------------

fn put_varint(out: &mut [u8], off: &mut usize, mut v: u32) -> Option<()> {
    loop {
        if *off >= out.len() {
            return None;
        }
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        out[*off] = b;
        *off += 1;
        if v == 0 {
            return Some(());
        }
    }
}

fn put_tag(out: &mut [u8], off: &mut usize, field: u32, wire_type: u32) -> Option<()> {
    put_varint(out, off, (field << 3) | wire_type)
}

fn put_string(out: &mut [u8], off: &mut usize, field: u32, s: &str) -> Option<()> {
    let b = s.as_bytes();
    put_tag(out, off, field, 2)?;
    put_varint(out, off, u32::try_from(b.len()).ok()?)?;
    if *off + b.len() > out.len() {
        return None;
    }
    out[*off..*off + b.len()].copy_from_slice(b);
    *off += b.len();
    Some(())
}

fn put_varint_field(out: &mut [u8], off: &mut usize, field: u32, v: u32) -> Option<()> {
    put_tag(out, off, field, 0)?;
    put_varint(out, off, v)
}

// --- frames ------------------------------------------------------------------

/// Encode a full frame (header + payload) into `out`. Returns total length,
/// or None if it would overflow.
pub fn frame_encode(out: &mut [u8], msg_id: u16, payload: &[u8]) -> Option<usize> {
    let total = HEADER_LEN + payload.len();
    if payload.len() > u16::MAX as usize || total > out.len() {
        return None;
    }
    let plen = u16::try_from(payload.len()).ok()?;
    out[0] = (plen >> 8) as u8;
    out[1] = (plen & 0xff) as u8;
    out[2] = (msg_id >> 8) as u8;
    out[3] = (msg_id & 0xff) as u8;
    out[HEADER_LEN..total].copy_from_slice(payload);
    Some(total)
}

/// Parse one frame from the front of `buf`. Returns `(msg_id, payload_range,
/// consumed)` if a complete frame is present, None if more data is needed.
/// The payload is returned as a range into `buf` so the caller keeps ownership.
#[must_use]
pub fn frame_parse(buf: &[u8]) -> Option<(u16, core::ops::Range<usize>, usize)> {
    if buf.len() < HEADER_LEN {
        return None;
    }
    let plen = ((buf[0] as usize) << 8) | buf[1] as usize;
    if buf.len() < HEADER_LEN + plen {
        return None;
    }
    let msg_id = (u16::from(buf[2]) << 8) | u16::from(buf[3]);
    Some((msg_id, HEADER_LEN..HEADER_LEN + plen, HEADER_LEN + plen))
}

// --- messages ----------------------------------------------------------------

/// `WifiStartRequest` `{ ip_address=1 (string), port=2 (int32) }`.
pub fn encode_wifi_start_request(out: &mut [u8], ip: &str, port: u32) -> Option<usize> {
    let mut off = 0;
    put_string(out, &mut off, 1, ip)?;
    put_varint_field(out, &mut off, 2, port)?;
    Some(off)
}

/// `WifiInfoResponse` `{ ssid=1, key=2, bssid=3, security_mode=4, ap_type=5 }`.
/// Note: key=2/bssid=3 is the aa-proxy-rs (runtime-proven) order; see module doc.
pub fn encode_wifi_info_response(
    out: &mut [u8],
    ssid: &str,
    key: &str,
    bssid: &str,
    security_mode: u32,
    ap_type: u32,
) -> Option<usize> {
    let mut off = 0;
    put_string(out, &mut off, 1, ssid)?;
    put_string(out, &mut off, 2, key)?;
    put_string(out, &mut off, 3, bssid)?;
    put_varint_field(out, &mut off, 4, security_mode)?;
    put_varint_field(out, &mut off, 5, ap_type)?;
    Some(off)
}

/// `WifiConnectStatus`: field 1 varint. `[08 00]` = joined OK; nonzero = the
/// phone failed the Wi-Fi join. None = malformed payload.
#[must_use]
pub fn wifi_connect_status_ok(payload: &[u8]) -> Option<bool> {
    if payload.len() < 2 || payload[0] != 0x08 {
        return None;
    }
    Some(payload[1] == 0)
}

// --- handshake state machine (HU role) ---------------------------------------

/// The strings the handshake hands the phone. Borrowed per-call so the core
/// stays allocation-free; the shell owns the storage.
#[derive(Clone, Copy)]
pub struct Cfg<'a> {
    /// TCP endpoint the phone connects to for the AA session (uap0 gateway).
    pub ip: &'a str,
    pub port: u32,
    /// Live uap0 AP credentials - MUST match what the AP broadcasts.
    pub ssid: &'a str,
    pub key: &'a str,
    pub bssid: &'a str,
}

/// Wireless handshake stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Idle,
    /// Sent `WifiStartRequest`; waiting for `WifiInfoRequest`.
    SentStartReq,
    /// Sent `WifiInfoResponse`; waiting for `WifiStartResponse`.
    SentInfoResp,
    /// Waiting for `WifiConnectStatus`.
    WaitConnectStatus,
    Done,
}

/// What a chunk of inbound data amounted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing decisive - handshake still in progress.
    Pending,
    /// Stage 5 OK: the phone joined Wi-Fi; the AA session lands on ip:port.
    Pass,
    /// The phone reported a Wi-Fi join failure (stage machine resets to Idle).
    Fail,
}

/// Authorization outlives the bootstrap channel, but never precedes Wi-Fi success.
#[derive(Default)]
pub struct Authorization {
    passed: bool,
}

impl Authorization {
    #[must_use]
    pub const fn new() -> Self {
        Self { passed: false }
    }

    pub fn observe(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Pass => self.passed = true,
            Outcome::Fail => self.revoke(),
            Outcome::Pending => {}
        }
    }

    pub fn revoke(&mut self) {
        self.passed = false;
    }

    #[must_use]
    pub fn granted(&self) -> bool {
        self.passed
    }
}

/// One AAWG RFCOMM session: rx reassembly + the 5-stage ping-pong. The `tx`
/// sink receives complete encoded frames; the protocol is strictly ping-pong,
/// so at most one frame is emitted per call (the shell's one-frame queue is
/// sufficient).
pub struct Conn {
    stage: Stage,
    rx: [u8; HEADER_LEN + RFCOMM_MTU],
    rx_len: usize,
}

impl Default for Conn {
    fn default() -> Self {
        Self::new()
    }
}

impl Conn {
    #[must_use]
    pub const fn new() -> Self {
        Conn {
            stage: Stage::Idle,
            rx: [0; HEADER_LEN + RFCOMM_MTU],
            rx_len: 0,
        }
    }

    #[must_use]
    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// Channel closed / session reset.
    pub fn reset(&mut self) {
        self.stage = Stage::Idle;
        self.rx_len = 0;
    }

    /// The AAWG channel just opened: emit stage 1 (`WifiStartRequest`).
    pub fn on_open(&mut self, cfg: &Cfg, tx: &mut impl FnMut(&[u8])) {
        self.rx_len = 0;
        let mut pl = [0u8; 128];
        let mut frame = [0u8; HEADER_LEN + 128];
        if let Some(n) = encode_wifi_start_request(&mut pl, cfg.ip, cfg.port) {
            if let Some(fl) = frame_encode(&mut frame, msg::WIFI_START_REQUEST, &pl[..n]) {
                tx(&frame[..fl]);
                self.stage = Stage::SentStartReq;
            }
        }
    }

    /// Inbound RFCOMM bytes (arbitrary chunk boundaries). Reassembles frames
    /// and advances the stage machine; emits at most one response frame.
    pub fn on_data(&mut self, cfg: &Cfg, data: &[u8], tx: &mut impl FnMut(&[u8])) -> Outcome {
        if self.rx_len + data.len() > self.rx.len() {
            self.rx_len = 0; // Reset reassembly on overflow.
        }
        let n = data.len().min(self.rx.len() - self.rx_len);
        self.rx[self.rx_len..self.rx_len + n].copy_from_slice(&data[..n]);
        self.rx_len += n;

        let mut outcome = Outcome::Pending;
        while let Some((id, pl_range, consumed)) = frame_parse(&self.rx[..self.rx_len]) {
            let mut pl_buf = [0u8; RFCOMM_MTU];
            let pl_len = pl_range.len();
            pl_buf[..pl_len].copy_from_slice(&self.rx[pl_range]);
            self.rx.copy_within(consumed..self.rx_len, 0);
            self.rx_len -= consumed;
            match self.on_frame(cfg, id, &pl_buf[..pl_len], tx) {
                Outcome::Pending => {}
                o => outcome = o,
            }
        }
        outcome
    }

    fn on_frame(
        &mut self,
        cfg: &Cfg,
        msg_id: u16,
        payload: &[u8],
        tx: &mut impl FnMut(&[u8]),
    ) -> Outcome {
        match self.stage {
            Stage::SentStartReq => {
                if msg_id == msg::WIFI_INFO_REQUEST {
                    let mut pl = [0u8; 256];
                    let mut frame = [0u8; HEADER_LEN + 256];
                    if let Some(n) = encode_wifi_info_response(
                        &mut pl,
                        cfg.ssid,
                        cfg.key,
                        cfg.bssid,
                        SEC_WPA2_PERSONAL,
                        AP_TYPE_DYNAMIC,
                    ) {
                        if let Some(fl) =
                            frame_encode(&mut frame, msg::WIFI_INFO_RESPONSE, &pl[..n])
                        {
                            tx(&frame[..fl]);
                            self.stage = Stage::SentInfoResp;
                        }
                    }
                }
                Outcome::Pending
            }
            Stage::SentInfoResp => {
                if msg_id == msg::WIFI_START_RESPONSE {
                    self.stage = Stage::WaitConnectStatus;
                }
                Outcome::Pending
            }
            Stage::WaitConnectStatus => {
                if msg_id == msg::WIFI_CONNECT_STATUS {
                    if let Some(true) = wifi_connect_status_ok(payload) {
                        self.stage = Stage::Done;
                        Outcome::Pass
                    } else {
                        self.stage = Stage::Idle;
                        Outcome::Fail
                    }
                } else {
                    Outcome::Pending
                }
            }
            // Idle/Done: WifiVersionRequest / WifiPingRequest may arrive; the
            // proven minimal path (aa-proxy-rs send_params) ignores them.
            _ => Outcome::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Cfg<'static> {
        Cfg {
            ip: "192.0.2.1",
            port: 5288,
            ssid: "MIB2-AP",
            key: "hunter22",
            bssid: "02:00:00:12:34:56",
        }
    }

    #[test]
    fn authorization_requires_success_and_survives_bootstrap_close() {
        let mut authorization = Authorization::new();
        let mut conn = Conn::new();
        let cfg = cfg();
        conn.on_open(&cfg, &mut |_| {});
        assert!(
            !authorization.granted(),
            "opening RFCOMM is not Wi-Fi success"
        );
        for id in [msg::WIFI_INFO_REQUEST, msg::WIFI_START_RESPONSE] {
            authorization.observe(conn.on_data(&cfg, &frame_of(id, &[]), &mut |_| {}));
            assert!(
                !authorization.granted(),
                "credentials alone cannot grant a session"
            );
        }
        authorization.observe(conn.on_data(
            &cfg,
            &frame_of(msg::WIFI_CONNECT_STATUS, &[0x08, 0]),
            &mut |_| {},
        ));
        assert!(authorization.granted());
        conn.reset(); // the shell clears channel state on ordinary RFCOMM close
        assert!(
            authorization.granted(),
            "successful data-plane grant outlives RFCOMM"
        );
        authorization.observe(Outcome::Pending);
        assert!(authorization.granted());
        authorization.revoke(); // policy/peer/epoch revocation or a new attempt
        assert!(!authorization.granted());
    }

    #[test]
    fn interrupted_and_failed_handshakes_never_authorize_a_stream() {
        let cfg = cfg();
        for fail in [false, true] {
            let mut authorization = Authorization::new();
            let mut conn = Conn::new();
            conn.on_open(&cfg, &mut |_| {});
            for id in [msg::WIFI_INFO_REQUEST, msg::WIFI_START_RESPONSE] {
                authorization.observe(conn.on_data(&cfg, &frame_of(id, &[]), &mut |_| {}));
            }
            if fail {
                authorization.observe(conn.on_data(
                    &cfg,
                    &frame_of(msg::WIFI_CONNECT_STATUS, &[0x08, 1]),
                    &mut |_| {},
                ));
            }
            conn.reset();
            assert!(!authorization.granted());
        }
        let mut authorization = Authorization::new();
        authorization.observe(Outcome::Pass);
        authorization.observe(Outcome::Fail);
        assert!(
            !authorization.granted(),
            "explicit failure revokes even a previous pass"
        );
    }

    /// Golden bytes, hand-derived from the proto2 wire rules.
    #[test]
    fn wifi_start_request_golden() {
        let mut out = [0u8; 64];
        let n = encode_wifi_start_request(&mut out, "192.0.2.1", 5288).unwrap();
        let mut want = vec![0x0A, 9];
        want.extend_from_slice(b"192.0.2.1");
        want.extend_from_slice(&[0x10, 0xA8, 0x29]); // 5288 = 0x14A8 -> A8 29
        assert_eq!(&out[..n], &want[..]);
    }

    #[test]
    fn wifi_info_response_golden_field_order() {
        let mut out = [0u8; 128];
        let n =
            encode_wifi_info_response(&mut out, "S", "K", "B", SEC_WPA2_PERSONAL, AP_TYPE_DYNAMIC)
                .unwrap();
        // ssid=1, key=2, bssid=3, sec=4 (8), ap=5 (1) - the aa-proxy-rs order.
        let want = [
            0x0A, 1, b'S', // ssid
            0x12, 1, b'K', // key   Note: field 2
            0x1A, 1, b'B', // bssid Note: field 3
            0x20, 8, // security_mode WPA2_PERSONAL
            0x28, 1, // access_point_type DYNAMIC
        ];
        assert_eq!(&out[..n], &want[..]);
    }

    #[test]
    fn frame_roundtrip_and_chunking() {
        let mut frame = [0u8; 64];
        let fl = frame_encode(&mut frame, msg::WIFI_INFO_REQUEST, &[1, 2, 3]).unwrap();
        assert_eq!(fl, HEADER_LEN + 3);
        assert_eq!(&frame[..HEADER_LEN], &[0, 3, 0, 2]);
        // whole
        let (id, pl, used) = frame_parse(&frame[..fl]).unwrap();
        assert_eq!((id, used), (msg::WIFI_INFO_REQUEST, fl));
        assert_eq!(&frame[pl], &[1, 2, 3]);
        // partial -> need more
        assert!(frame_parse(&frame[..fl - 1]).is_none());
        assert!(frame_parse(&frame[..2]).is_none());
    }

    #[test]
    fn connect_status_decode() {
        assert_eq!(wifi_connect_status_ok(&[0x08, 0x00]), Some(true));
        assert_eq!(wifi_connect_status_ok(&[0x08, 0xFD]), Some(false));
        // -i64::MAX form seen from real phones
        let fail = [
            0x08, 0xFD, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01,
        ];
        assert_eq!(wifi_connect_status_ok(&fail), Some(false));
        assert_eq!(wifi_connect_status_ok(&[0x08]), None);
        assert_eq!(wifi_connect_status_ok(&[0x12, 0x00]), None);
        assert_eq!(wifi_connect_status_ok(&[]), None);
    }

    fn frame_of(msg_id: u16, payload: &[u8]) -> Vec<u8> {
        let mut buf = vec![0u8; HEADER_LEN + payload.len()];
        let n = frame_encode(&mut buf, msg_id, payload).unwrap();
        buf.truncate(n);
        buf
    }

    /// The full 5-stage ping-pong, exactly as `send_params` drives it.
    #[test]
    fn handshake_pass() {
        let cfg = cfg();
        let mut conn = Conn::new();
        let mut sent: Vec<Vec<u8>> = Vec::new();

        conn.on_open(&cfg, &mut |f: &[u8]| sent.push(f.to_vec()));
        assert_eq!(conn.stage(), Stage::SentStartReq);
        assert_eq!(sent.len(), 1);
        let (id, _, _) = frame_parse(&sent[0]).unwrap();
        assert_eq!(id, msg::WIFI_START_REQUEST);

        // <- WifiInfoRequest  =>  -> WifiInfoResponse
        let o = conn.on_data(&cfg, &frame_of(msg::WIFI_INFO_REQUEST, &[]), &mut |f| {
            sent.push(f.to_vec());
        });
        assert_eq!(o, Outcome::Pending);
        assert_eq!(conn.stage(), Stage::SentInfoResp);
        assert_eq!(sent.len(), 2);
        let (id, pl, _) = frame_parse(&sent[1]).unwrap();
        assert_eq!(id, msg::WIFI_INFO_RESPONSE);
        // ssid is the first field of the payload
        assert_eq!(&sent[1][pl][..2], &[0x0A, 7]);

        // <- WifiStartResponse
        let o = conn.on_data(&cfg, &frame_of(msg::WIFI_START_RESPONSE, &[]), &mut |f| {
            sent.push(f.to_vec());
        });
        assert_eq!(o, Outcome::Pending);
        assert_eq!(conn.stage(), Stage::WaitConnectStatus);

        // <- WifiConnectStatus OK
        let o = conn.on_data(
            &cfg,
            &frame_of(msg::WIFI_CONNECT_STATUS, &[0x08, 0x00]),
            &mut |f| {
                sent.push(f.to_vec());
            },
        );
        assert_eq!(o, Outcome::Pass);
        assert_eq!(conn.stage(), Stage::Done);
        assert_eq!(sent.len(), 2); // no more tx after stage 3
    }

    /// Same flow with every inbound byte delivered one at a time (RFCOMM gives
    /// arbitrary chunk boundaries) and all three phone frames coalesced.
    #[test]
    fn handshake_pass_chunked_and_coalesced() {
        let cfg = cfg();
        let mut conn = Conn::new();
        let mut sent = 0usize;

        conn.on_open(&cfg, &mut |_f: &[u8]| sent += 1);
        // byte-at-a-time WifiInfoRequest
        for &b in &frame_of(msg::WIFI_INFO_REQUEST, &[]) {
            conn.on_data(&cfg, &[b], &mut |_f| sent += 1);
        }
        assert_eq!(conn.stage(), Stage::SentInfoResp);
        assert_eq!(sent, 2);

        // StartResponse + ConnectStatus in ONE delivery
        let mut both = frame_of(msg::WIFI_START_RESPONSE, &[]);
        both.extend_from_slice(&frame_of(msg::WIFI_CONNECT_STATUS, &[0x08, 0x00]));
        let o = conn.on_data(&cfg, &both, &mut |_f| sent += 1);
        assert_eq!(o, Outcome::Pass);
        assert_eq!(sent, 2);
    }

    #[test]
    fn handshake_fail_resets() {
        let cfg = cfg();
        let mut conn = Conn::new();
        let mut sent = 0usize;
        conn.on_open(&cfg, &mut |_f: &[u8]| sent += 1);
        conn.on_data(&cfg, &frame_of(msg::WIFI_INFO_REQUEST, &[]), &mut |_f| {
            sent += 1;
        });
        conn.on_data(&cfg, &frame_of(msg::WIFI_START_RESPONSE, &[]), &mut |_f| {
            sent += 1;
        });
        let fail = [
            0x08, 0xFD, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01,
        ];
        let o = conn.on_data(
            &cfg,
            &frame_of(msg::WIFI_CONNECT_STATUS, &fail),
            &mut |_f| {
                sent += 1;
            },
        );
        assert_eq!(o, Outcome::Fail);
        assert_eq!(conn.stage(), Stage::Idle);
    }

    /// Pings/version requests out of turn are ignored (the `send_params` minimal
    /// path); garbage that overflows the rx buffer resyncs instead of panicking.
    #[test]
    fn ignores_unexpected_and_survives_garbage() {
        let cfg = cfg();
        let mut conn = Conn::new();
        let mut sent = 0usize;
        conn.on_open(&cfg, &mut |_f: &[u8]| sent += 1);
        conn.on_data(&cfg, &frame_of(msg::WIFI_PING_REQUEST, &[]), &mut |_f| {
            sent += 1;
        });
        conn.on_data(
            &cfg,
            &frame_of(msg::WIFI_VERSION_REQUEST, &[1]),
            &mut |_f| sent += 1,
        );
        assert_eq!(conn.stage(), Stage::SentStartReq);
        assert_eq!(sent, 1);
        // a giant bogus length header forces the resync path
        let bogus = [0xFF, 0xFF, 0x00, 0x01];
        conn.on_data(&cfg, &bogus, &mut |_f| sent += 1);
        for _ in 0..40 {
            conn.on_data(&cfg, &[0u8; 100], &mut |_f| sent += 1);
        }
        // still functional after reset
        conn.reset();
        conn.on_open(&cfg, &mut |_f: &[u8]| sent += 1);
        assert_eq!(conn.stage(), Stage::SentStartReq);
    }
}
