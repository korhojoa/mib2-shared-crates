// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stream-multiplexer framing for the AA vendor-channel uplink - sans-IO.
//!
//! The Android Auto vendor extension (`com.vwag.infotainment.gal.*`) is a
//! single ordered byte pipe: `VendorExtension::sendData` out,
//! `handleRawMessage` in. To carry the SOCKS front-end's many concurrent TCP
//! flows over that one pipe we multiplex them with a tiny frame header, the
//! same way BNEP carries many flows over one L2CAP channel. This module is the
//! wire codec ONLY - no sockets, no ring, no allocation. Bytes arrive from the
//! transport in arbitrary chunks, so `parse` is incremental over the caller's
//! growing buffer, and `encode` writes into the caller's slice. The I/O and the
//! per-stream policy (windows, teardown) live in the daemon shell (`aa_relay`),
//! exactly as `socks.rs` is the codec and `pan_relay.rs` the shell.
//!
//! Frame = `ver(1) type(1) stream(2, BE) len(2, BE) payload[len]`. `stream` is
//! meaningless for `Ping`; the initiator of an `Open` owns the id. Odd ids are
//! head-unit-initiated, even ids phone-initiated, so the two ends never collide
//! (the same discipline `socks`/BNEP use for their own namespaces).

/// Protocol version byte - bumped only on an incompatible wire change.
pub const MUX_VER: u8 = 0x01;

/// Fixed header length preceding every frame's payload.
pub const HEADER_LEN: usize = 6;

/// The `len` field is one `u16`, so a single frame's payload cannot exceed
/// this. Larger transfers are split across `Data` frames by the shell.
pub const MAX_PAYLOAD: usize = u16::MAX as usize;

// Frame type discriminants (the `type` byte).
pub const T_OPEN: u8 = 0x01;
pub const T_OPEN_OK: u8 = 0x02;
pub const T_OPEN_ERR: u8 = 0x03;
pub const T_DATA: u8 = 0x04;
pub const T_CLOSE: u8 = 0x05;
pub const T_PING: u8 = 0x06;
pub const T_WINDOW: u8 = 0x07;
pub const T_AUTH: u8 = 0x08;

/// One multiplexer frame, borrowing its payload from the parse buffer (sans-IO,
/// no allocation). `Open`'s target is an untrusted `host:port` byte string the
/// egress end resolves; the codec does not interpret it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame<'a> {
    /// Open a stream to `target` (`b"host:port"`, the SOCKS CONNECT target).
    Open { stream: u16, target: &'a [u8] },
    /// The egress connected; the stream is now relaying.
    OpenOk { stream: u16 },
    /// The connect failed; `rep` is the SOCKS reply code to hand back to the
    /// local client (`socks::REP_*`), so no second error vocabulary exists.
    OpenErr { stream: u16, rep: u8 },
    /// Stream payload in one direction.
    Data { stream: u16, bytes: &'a [u8] },
    /// One half closed; after both halves close the id may be reused.
    Close { stream: u16 },
    /// Keepalive / liveness probe. Carries no stream.
    Ping,
    /// Grant `credit` more bytes of receive window for `stream` (flow control;
    /// the sender must not have more than the granted bytes in flight).
    Window { stream: u16, credit: u32 },
    /// Peer authentication, announced once right after the session starts.
    /// `token` is the shared secret; the receiver honours no `Open` until a
    /// matching one arrives, so an unauthenticated peer can open nothing.
    /// Rides the `stream` field as `AUTH_ANNOUNCE`.
    Auth { token: &'a [u8] },
    /// The answer to a matching `Auth`: same type byte and token, distinguished
    /// only by `stream == AUTH_ANSWER`. It authenticates the sender exactly as
    /// `Auth` does, and is never answered in turn. Both ends answer every
    /// announcement they can verify - the two sides' sessions start
    /// independently, so either may be holding a stale authentication and hear
    /// nothing back otherwise - and this variant is what keeps those two
    /// unconditional answers from becoming an endless ping-pong.
    AuthAck { token: &'a [u8] },
}

/// `Auth` carries no stream of its own, so that field discriminates an
/// announcement from an answer. Peers older than this ignored it and decoded
/// every `T_AUTH` as an announcement, so an `AuthAck` reaching one reads as a
/// plain `Auth` - which is why both ends must be updated together.
pub const AUTH_ANNOUNCE: u16 = 0;
pub const AUTH_ANSWER: u16 = 1;

/// Why a byte sequence is not a valid frame. Terminal - the caller must drop
/// the transport, since a framing error means the stream is desynchronised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxError {
    /// First byte was not `MUX_VER`.
    BadVersion,
    /// `type` byte is not one this version defines.
    UnknownType(u8),
    /// A frame's declared `len` is wrong for its fixed-size type
    /// (`OpenOk`/`Close` want 0, `OpenErr` wants 1, `Window`/`Ping` are fixed).
    BadLength,
}

/// Result of one `parse` step. `NeedMore` is distinct from any error, and
/// `consumed` tells the caller exactly how much of its buffer to drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parsed<'a> {
    /// Not enough bytes yet - read more and call `parse` again.
    NeedMore,
    /// A whole frame: drain `consumed` bytes, then call `parse` again.
    Frame { frame: Frame<'a>, consumed: usize },
    /// Framing error - see `MuxError`; drop the transport.
    Err(MuxError),
}

/// Parse the frame at the front of `buf`. Sans-IO: call after every read, in a
/// loop, draining `consumed` each time, until it returns `NeedMore`. The
/// returned `Frame` borrows `buf`, so drain only after you are done with it.
#[must_use]
pub fn parse(buf: &[u8]) -> Parsed<'_> {
    if buf.len() < HEADER_LEN {
        return Parsed::NeedMore;
    }
    if buf[0] != MUX_VER {
        return Parsed::Err(MuxError::BadVersion);
    }
    let ty = buf[1];
    let stream = u16::from_be_bytes([buf[2], buf[3]]);
    let len = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    if buf.len() < HEADER_LEN + len {
        return Parsed::NeedMore;
    }
    let payload = &buf[HEADER_LEN..HEADER_LEN + len];
    let consumed = HEADER_LEN + len;
    let frame = match ty {
        T_OPEN => Frame::Open {
            stream,
            target: payload,
        },
        T_DATA => Frame::Data {
            stream,
            bytes: payload,
        },
        T_OPEN_OK => {
            if len != 0 {
                return Parsed::Err(MuxError::BadLength);
            }
            Frame::OpenOk { stream }
        }
        T_CLOSE => {
            if len != 0 {
                return Parsed::Err(MuxError::BadLength);
            }
            Frame::Close { stream }
        }
        T_OPEN_ERR => {
            if len != 1 {
                return Parsed::Err(MuxError::BadLength);
            }
            Frame::OpenErr {
                stream,
                rep: payload[0],
            }
        }
        T_PING => {
            if len != 0 {
                return Parsed::Err(MuxError::BadLength);
            }
            Frame::Ping
        }
        T_WINDOW => {
            if len != 4 {
                return Parsed::Err(MuxError::BadLength);
            }
            let credit = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
            Frame::Window { stream, credit }
        }
        T_AUTH if stream == AUTH_ANSWER => Frame::AuthAck { token: payload },
        T_AUTH => Frame::Auth { token: payload },
        other => return Parsed::Err(MuxError::UnknownType(other)),
    };
    Parsed::Frame { frame, consumed }
}

/// Bytes `encode` needs for `frame`: header plus its payload. `Open`/`Data`
/// with an over-long payload cannot be encoded (see `encode`), but this still
/// reports the requested size so a caller can detect that before allocating.
#[must_use]
pub fn encoded_len(frame: &Frame<'_>) -> usize {
    let payload = match frame {
        Frame::Open { target, .. } => target.len(),
        Frame::Data { bytes, .. } => bytes.len(),
        Frame::Auth { token } | Frame::AuthAck { token } => token.len(),
        Frame::OpenErr { .. } => 1,
        Frame::Window { .. } => 4,
        Frame::OpenOk { .. } | Frame::Close { .. } | Frame::Ping => 0,
    };
    HEADER_LEN + payload
}

/// Encode `frame` into `out`, returning the byte count. `None` if `out` is too
/// short OR the payload exceeds `MAX_PAYLOAD` (the `len` field would overflow) -
/// the shell must split `Data`/`Open` payloads below that before calling.
pub fn encode(out: &mut [u8], frame: &Frame<'_>) -> Option<usize> {
    let (ty, stream, payload_len): (u8, u16, usize) = match frame {
        Frame::Open { stream, target } => (T_OPEN, *stream, target.len()),
        Frame::OpenOk { stream } => (T_OPEN_OK, *stream, 0),
        Frame::OpenErr { stream, .. } => (T_OPEN_ERR, *stream, 1),
        Frame::Data { stream, bytes } => (T_DATA, *stream, bytes.len()),
        Frame::Close { stream } => (T_CLOSE, *stream, 0),
        Frame::Ping => (T_PING, 0, 0),
        Frame::Window { stream, .. } => (T_WINDOW, *stream, 4),
        Frame::Auth { token } => (T_AUTH, AUTH_ANNOUNCE, token.len()),
        Frame::AuthAck { token } => (T_AUTH, AUTH_ANSWER, token.len()),
    };
    if payload_len > MAX_PAYLOAD {
        return None;
    }
    let total = HEADER_LEN + payload_len;
    if out.len() < total {
        return None;
    }
    out[0] = MUX_VER;
    out[1] = ty;
    out[2..4].copy_from_slice(&stream.to_be_bytes());
    out[4..6].copy_from_slice(&(u16::try_from(payload_len).ok()?).to_be_bytes());
    match frame {
        Frame::Open { target, .. } => out[HEADER_LEN..total].copy_from_slice(target),
        Frame::Data { bytes, .. } => out[HEADER_LEN..total].copy_from_slice(bytes),
        Frame::Auth { token } | Frame::AuthAck { token } => {
            out[HEADER_LEN..total].copy_from_slice(token);
        }
        Frame::OpenErr { rep, .. } => out[HEADER_LEN] = *rep,
        Frame::Window { credit, .. } => {
            out[HEADER_LEN..total].copy_from_slice(&credit.to_be_bytes());
        }
        Frame::OpenOk { .. } | Frame::Close { .. } | Frame::Ping => {}
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip every frame kind through a generous buffer.
    fn roundtrip(frame: Frame<'_>) {
        let mut buf = [0u8; 64];
        let n = encode(&mut buf, &frame).unwrap();
        assert_eq!(n, encoded_len(&frame));
        match parse(&buf[..n]) {
            Parsed::Frame {
                frame: got,
                consumed,
            } => {
                assert_eq!(got, frame);
                assert_eq!(consumed, n);
            }
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    #[test]
    fn every_kind_roundtrips() {
        roundtrip(Frame::Open {
            stream: 1,
            target: b"broker.example:1883",
        });
        roundtrip(Frame::OpenOk { stream: 3 });
        roundtrip(Frame::OpenErr {
            stream: 3,
            rep: 0x05,
        });
        roundtrip(Frame::Data {
            stream: 7,
            bytes: b"hello world",
        });
        roundtrip(Frame::Data {
            stream: 7,
            bytes: b"",
        });
        roundtrip(Frame::Close { stream: 9 });
        roundtrip(Frame::Ping);
        roundtrip(Frame::Window {
            stream: 11,
            credit: 262_144,
        });
        roundtrip(Frame::Auth {
            token: b"s3cr3t-token",
        });
        roundtrip(Frame::AuthAck {
            token: b"s3cr3t-token",
        });
    }

    #[test]
    fn open_golden_bytes() {
        let mut buf = [0u8; 16];
        let n = encode(
            &mut buf,
            &Frame::Open {
                stream: 0x0102,
                target: b"ab",
            },
        )
        .unwrap();
        // ver, T_OPEN, stream hi/lo, len hi/lo, 'a','b'
        assert_eq!(&buf[..n], &[0x01, 0x01, 0x01, 0x02, 0x00, 0x02, b'a', b'b']);
    }

    /// Feed a back-to-back stream of frames one byte at a time; the incremental
    /// parser must reproduce exactly the frames a whole-buffer parse yields.
    #[test]
    fn chunked_delivery_byte_by_byte() {
        let frames = [
            Frame::Open {
                stream: 1,
                target: b"1.2.3.4:80",
            },
            Frame::OpenOk { stream: 1 },
            Frame::Data {
                stream: 1,
                bytes: b"GET / HTTP/1.0\r\n\r\n",
            },
            Frame::Window {
                stream: 1,
                credit: 65_536,
            },
            Frame::Ping,
            Frame::Close { stream: 1 },
        ];
        let mut wire = Vec::new();
        for f in &frames {
            let mut b = [0u8; 64];
            let n = encode(&mut b, f).unwrap();
            wire.extend_from_slice(&b[..n]);
        }

        let mut buf: Vec<u8> = Vec::new();
        let mut got: Vec<Key> = Vec::new();
        for &byte in &wire {
            buf.push(byte);
            loop {
                // Reduce to an owned key inside the arm so the borrow of buf
                // ends before we drain it.
                let (k, consumed) = match parse(&buf) {
                    Parsed::NeedMore => break,
                    Parsed::Err(e) => panic!("unexpected err {e:?}"),
                    Parsed::Frame { frame, consumed } => (key(&frame), consumed),
                };
                got.push(k);
                buf.drain(..consumed);
            }
        }
        assert_eq!(buf.len(), 0, "trailing undrained bytes");
        let want: Vec<Key> = frames.iter().map(key).collect();
        assert_eq!(got, want);
    }

    // Frame borrows the parse buffer; an owned key lets tests accumulate and
    // compare parsed frames after the borrow ends.
    type Key = (u8, u16, Vec<u8>, u32);
    fn key(f: &Frame<'_>) -> Key {
        match *f {
            Frame::Open { stream, target } => (T_OPEN, stream, target.to_vec(), 0),
            Frame::OpenOk { stream } => (T_OPEN_OK, stream, Vec::new(), 0),
            Frame::OpenErr { stream, rep } => (T_OPEN_ERR, stream, Vec::new(), u32::from(rep)),
            Frame::Data { stream, bytes } => (T_DATA, stream, bytes.to_vec(), 0),
            Frame::Close { stream } => (T_CLOSE, stream, Vec::new(), 0),
            Frame::Ping => (T_PING, 0, Vec::new(), 0),
            Frame::Window { stream, credit } => (T_WINDOW, stream, Vec::new(), credit),
            Frame::Auth { token } => (T_AUTH, AUTH_ANNOUNCE, token.to_vec(), 0),
            Frame::AuthAck { token } => (T_AUTH, AUTH_ANSWER, token.to_vec(), 0),
        }
    }

    #[test]
    fn bad_version_and_unknown_type() {
        assert_eq!(
            parse(&[0x02, T_PING, 0, 0, 0, 0]),
            Parsed::Err(MuxError::BadVersion)
        );
        assert_eq!(
            parse(&[MUX_VER, 0x7f, 0, 0, 0, 0]),
            Parsed::Err(MuxError::UnknownType(0x7f))
        );
    }

    #[test]
    fn fixed_size_types_reject_bad_length() {
        // OpenOk/Close/Ping want len 0; OpenErr wants 1; Window wants 4.
        assert_eq!(
            parse(&[MUX_VER, T_OPEN_OK, 0, 1, 0, 1, 0xff]),
            Parsed::Err(MuxError::BadLength)
        );
        assert_eq!(
            parse(&[MUX_VER, T_CLOSE, 0, 1, 0, 2, 0, 0]),
            Parsed::Err(MuxError::BadLength)
        );
        assert_eq!(
            parse(&[MUX_VER, T_OPEN_ERR, 0, 1, 0, 0]),
            Parsed::Err(MuxError::BadLength)
        );
        assert_eq!(
            parse(&[MUX_VER, T_PING, 0, 0, 0, 3, 1, 2, 3]),
            Parsed::Err(MuxError::BadLength)
        );
        assert_eq!(
            parse(&[MUX_VER, T_WINDOW, 0, 1, 0, 3, 1, 2, 3]),
            Parsed::Err(MuxError::BadLength)
        );
    }

    /// Every prefix of a valid frame stream parses without panicking and never
    /// invents a frame that a full parse would not yield.
    #[test]
    fn truncation_never_panics() {
        let mut wire = Vec::new();
        for f in [
            Frame::Open {
                stream: 5,
                target: b"example.com:443",
            },
            Frame::Data {
                stream: 5,
                bytes: b"payload bytes here",
            },
            Frame::Close { stream: 5 },
        ] {
            let mut b = [0u8; 64];
            let n = encode(&mut b, &f).unwrap();
            wire.extend_from_slice(&b[..n]);
        }
        for k in 0..wire.len() {
            // A single parse on a prefix: NeedMore or a frame fully inside k.
            match parse(&wire[..k]) {
                Parsed::NeedMore => {}
                Parsed::Frame { consumed, .. } => assert!(consumed <= k),
                Parsed::Err(e) => panic!("prefix {k} errored: {e:?}"),
            }
        }
    }

    #[test]
    fn oversize_payload_refused() {
        // A Data payload at exactly MAX_PAYLOAD encodes; one over cannot.
        let big = vec![0u8; MAX_PAYLOAD + 1];
        let mut out = vec![0u8; MAX_PAYLOAD + HEADER_LEN + 8];
        assert_eq!(
            encode(
                &mut out,
                &Frame::Data {
                    stream: 1,
                    bytes: &big
                }
            ),
            None
        );
        assert!(encode(
            &mut out,
            &Frame::Data {
                stream: 1,
                bytes: &big[..MAX_PAYLOAD]
            }
        )
        .is_some());
    }

    #[test]
    fn short_output_refused() {
        let mut out = [0u8; HEADER_LEN - 1];
        assert_eq!(encode(&mut out, &Frame::Ping), None);
    }
}
