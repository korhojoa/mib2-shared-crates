// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) mib2-shared-crates contributors

//! MPEG-TS packet and block construction for the MOST video channel.
//!
//! A block contains 64 packets of 188 bytes. The caller selects PCR placement
//! and supplies the clock values. Complete blocks leave through `MuxIo::emit`.
//! The caller controls I/O, retries, counters and logging.

pub const TS_PACKET_SIZE: usize = 188;
pub const TS_BLOCK_PACKETS: usize = 64;
pub const TS_BLOCK_SIZE: usize = TS_PACKET_SIZE * TS_BLOCK_PACKETS;
pub const VIDEO_PID: u32 = 0x0101;
pub const PMT_PID: u32 = 0x0100;
const MAX_PCR_PER_BLOCK: usize = 16;
/// `adaptation_field_length` of a packet that is adaptation field only (no
/// payload): everything after the 4-byte header and the length byte itself.
const AF_ONLY_LEN: u8 = 183;
const _: () = assert!(AF_ONLY_LEN as usize == TS_PACKET_SIZE - 5);

fn crc32_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    for (i, slot) in (0u32..).zip(t.iter_mut()) {
        let mut c = i << 24;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04C1_1DB7
            } else {
                c << 1
            };
            k += 1;
        }
        *slot = c;
    }
    t
}

fn ts_crc32(tab: &[u32; 256], d: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in d {
        // the & 0xFF mask proves the index < 256; get + unwrap_or(0) makes
        // that structural rather than something a reader has to re-derive
        let idx = (((crc >> 24) ^ u32::from(b)) & 0xFF) as usize;
        crc = (crc << 8) ^ tab.get(idx).copied().unwrap_or(0);
    }
    crc
}

/// The fixed-size reference is the no-panic spelling: length is checked once,
/// at the call site's `try_into`, instead of five times at `p[0]..p[5]`.
fn pcr_encode(p: &mut [u8; 6], pcr90: u64) {
    let b = pcr90 & 0x1_FFFF_FFFF;
    p[0] = ((b >> 25) & 0xFF) as u8;
    p[1] = ((b >> 17) & 0xFF) as u8;
    p[2] = ((b >> 9) & 0xFF) as u8;
    p[3] = ((b >> 1) & 0xFF) as u8;
    p[4] = (u8::from(b & 1 != 0) << 7) | 0x7E;
    p[5] = 0x00;
}

/// PCR position and timestamp policy. Select the policy required by the
/// consumer's stream.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PcrPlacement {
    /// dio-extender's shape: a PCR-only packet opens every block
    /// (`block_begin`), and every PCR in the block is RE-STAMPED with
    /// `MuxIo::now90()` at the moment the block is emitted -- the decoder sees
    /// transmit time. Regular 64-packet PCR spacing.
    BlockHead,
    /// gal-extender's shape: the PCR rides the video PES-start packet, stamped
    /// AT BUILD TIME with the `pcr90` handed to `send_video`, and is never
    /// restamped -- `MuxIo::now90()` is not consulted at all. No bare PCR
    /// packets; PCR spacing follows frame boundaries. gal's shipping stream is
    /// this plus `pts_lead_ms = 0` (PTS == PCR).
    PesStart,
}

/// The consumer's side of the seam: the transmission clock and the wire.
///
/// `now90` is read once per finished block, at the moment the block is
/// finalised, and stamped into every PCR slot in it -- that is what makes the
/// PCR the decoder sees the TRANSMIT time rather than the build time. The
/// consumer decides what clock that is (wall, virtual for --fast replays,
/// or whatever the shim's pacing uses).
///
/// `emit` always receives exactly `TS_BLOCK_SIZE` bytes: a partial block has
/// already been padded with null packets, because the MOST driver takes whole
/// 12032-byte blocks only -- a short write is an EMSGSIZE, not a picture.
pub trait MuxIo {
    /// 90 kHz transmission clock.
    fn now90(&mut self) -> u64;
    /// One finished, PCR-stamped, 12032-byte block.
    fn emit(&mut self, block: &[u8]);
}

pub struct Mux {
    block: [u8; TS_BLOCK_SIZE],
    block_pos: usize,
    pcr_off: [usize; MAX_PCR_PER_BLOCK],
    pcr_n: usize,
    cc: [u8; 8192],
    last_video_cc: u8,
    /// PTS = PCR + this lead, in ms. Format, not policy: it is baked into the
    /// PES header bytes and must match what the shipping muxer writes.
    pub pts_lead_ms: u64,
    /// See [`PcrPlacement`]. The default is `BlockHead`.
    pub pcr_placement: PcrPlacement,
    crc_tab: [u32; 256],
}

impl Mux {
    #[must_use]
    pub fn new() -> Mux {
        Mux {
            block: [0; TS_BLOCK_SIZE],
            block_pos: 0,
            pcr_off: [0; MAX_PCR_PER_BLOCK],
            pcr_n: 0,
            cc: [0; 8192],
            last_video_cc: 0x0F,
            pts_lead_ms: 300,
            pcr_placement: PcrPlacement::BlockHead,
            crc_tab: crc32_table(),
        }
    }

    fn write_len(&mut self, io: &mut dyn MuxIo, mut len: usize) {
        if len == 0 {
            return;
        }
        if len < TS_BLOCK_SIZE {
            // pad the tail of a partial block with null packets. len is always
            // packet-aligned (block_pos * TS_PACKET_SIZE), so the tail divides
            // into whole packets and as_chunks_mut leaves no remainder.
            if let Some(tail) = self.block.get_mut(len..) {
                for pk in tail.as_chunks_mut::<TS_PACKET_SIZE>().0 {
                    pk.fill(0xFF);
                    if let Some(hdr) = pk.get_mut(..4) {
                        hdr.copy_from_slice(&[0x47, 0x1F, 0xFF, 0x10]);
                    }
                }
            }
            len = TS_BLOCK_SIZE;
        }
        // Stamp PCR at transmission. Only BlockHead placement registers PCR
        // offsets; under PesStart pcr_n stays 0 and the io clock is never
        // consulted -- the build-time pcr90 in the packet is final.
        if self.pcr_n > 0 {
            let now90 = io.now90();
            for &off in self.pcr_off.iter().take(self.pcr_n.min(MAX_PCR_PER_BLOCK)) {
                let end = off.saturating_add(6);
                if end > len {
                    continue;
                }
                if let Some(p) = self
                    .block
                    .get_mut(off..end)
                    .and_then(|s| <&mut [u8; 6]>::try_from(s).ok())
                {
                    pcr_encode(p, now90);
                }
            }
        }
        io.emit(self.block.get(..len).unwrap_or(&self.block));
    }

    /// Is there a partially-filled block waiting to go out?
    #[must_use]
    pub fn block_pending(&self) -> bool {
        self.block_pos > 0
    }

    pub fn flush_block(&mut self, io: &mut dyn MuxIo) {
        if self.block_pos == 0 {
            return;
        }
        self.write_len(io, self.block_pos * TS_PACKET_SIZE);
        self.block_pos = 0;
        self.pcr_n = 0;
    }

    /// Open a block with a PCR-only packet, so every block carries a timestamp.
    ///
    /// Under `PcrPlacement::PesStart` this is a no-op: that wire format has no
    /// bare PCR packets, so blocks simply start with their first real packet
    /// (and a bare `block_begin` + `flush_block` heartbeat produces nothing).
    pub fn block_begin(&mut self) {
        if self.block_pos != 0 {
            return;
        }
        if self.pcr_placement == PcrPlacement::PesStart {
            return;
        }
        let cc = self.last_video_cc & 0x0F;
        let pkt: &mut [u8; TS_PACKET_SIZE] = match self
            .block
            .get_mut(..TS_PACKET_SIZE)
            .and_then(|s| s.try_into().ok())
        {
            Some(p) => p,
            None => return, // unreachable: the block is 64 packets long
        };
        pkt.fill(0xFF);
        pkt[0] = 0x47;
        pkt[1] = ((VIDEO_PID >> 8) & 0x1F) as u8;
        pkt[2] = (VIDEO_PID & 0xFF) as u8;
        pkt[3] = 0x20 | cc;
        pkt[4] = AF_ONLY_LEN;
        pkt[5] = 0x10; // PCR flag
        self.pcr_n = 1;
        self.pcr_off[0] = 6;
        self.block_pos = 1;
    }

    fn push(&mut self, io: &mut dyn MuxIo, pkt: &[u8; TS_PACKET_SIZE]) {
        self.block_begin();
        let off = self.block_pos * TS_PACKET_SIZE;
        match self.block.get_mut(off..off + TS_PACKET_SIZE) {
            Some(dst) => dst.copy_from_slice(pkt),
            None => return, // unreachable: block_pos < TS_BLOCK_PACKETS invariant
        }
        self.block_pos += 1;
        if self.block_pos >= TS_BLOCK_PACKETS {
            self.write_len(io, TS_BLOCK_SIZE);
            self.block_pos = 0;
            self.pcr_n = 0;
        }
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "every (private) caller passes payload.len() <= TS_PACKET_SIZE - 4 = 184 (PAT 17, PMT 22, send_video clamps to first_avail / TS_PACKET_SIZE - 4), so both adaptation_field_length values are <= 183 and fit a u8"
    )]
    fn write_packet(
        &mut self,
        io: &mut dyn MuxIo,
        pid: u32,
        pusi: bool,
        has_pcr: bool,
        pcr90: u64,
        payload: &[u8],
    ) {
        // Bounded byte cursor over the packet. Every write here is provably in
        // range (the stuffing arithmetic guarantees the payload fits), but
        // `put` makes an overrun structurally impossible: a write past the end
        // is discarded instead of writing beyond the packet.
        fn put(pkt: &mut [u8; TS_PACKET_SIZE], idx: &mut usize, v: u8) {
            if let Some(slot) = pkt.get_mut(*idx) {
                *slot = v;
            }
            *idx += 1;
        }

        let len = payload.len();
        let mut pkt = [0u8; TS_PACKET_SIZE];
        let mut idx = 0usize;
        let ci = (pid & 0x1FFF) as usize;
        let cc = self.cc.get(ci).copied().unwrap_or(0);
        if let Some(slot) = self.cc.get_mut(ci) {
            *slot = (cc + 1) & 0x0F;
        }

        put(&mut pkt, &mut idx, 0x47);
        put(
            &mut pkt,
            &mut idx,
            (if pusi { 0x40 } else { 0 }) | ((pid >> 8) & 0x1F) as u8,
        );
        put(&mut pkt, &mut idx, (pid & 0xFF) as u8);

        if has_pcr {
            put(&mut pkt, &mut idx, 0x30 | cc);
            let stuff = (TS_PACKET_SIZE - 4 - 8).saturating_sub(len);
            put(&mut pkt, &mut idx, (7 + stuff) as u8);
            put(&mut pkt, &mut idx, 0x10);
            if let Some(p) = pkt
                .get_mut(idx..idx.saturating_add(6))
                .and_then(|s| <&mut [u8; 6]>::try_from(s).ok())
            {
                pcr_encode(p, pcr90);
            }
            idx = idx.saturating_add(6);
            for _ in 0..stuff {
                put(&mut pkt, &mut idx, 0xFF);
            }
        } else {
            // Add stuffing when the payload has fewer than 184 bytes.
            // Saturation gives zero when no adaptation field fits.
            let stuff = (TS_PACKET_SIZE - 4).saturating_sub(len);
            if stuff > 0 {
                put(&mut pkt, &mut idx, 0x30 | cc);
                put(&mut pkt, &mut idx, (stuff - 1) as u8);
                if stuff > 1 {
                    put(&mut pkt, &mut idx, 0x00);
                    for _ in 0..stuff - 2 {
                        put(&mut pkt, &mut idx, 0xFF);
                    }
                }
            } else {
                put(&mut pkt, &mut idx, 0x10 | cc);
            }
        }
        if let Some(dst) = pkt.get_mut(idx..idx.saturating_add(len)) {
            dst.copy_from_slice(payload);
        }
        idx = idx.saturating_add(len);
        while idx < TS_PACKET_SIZE {
            put(&mut pkt, &mut idx, 0xFF);
        }
        self.push(io, &pkt);
        if (pid & 0x1FFF) == VIDEO_PID {
            self.last_video_cc = cc;
        }
    }

    pub fn send_pat(&mut self, io: &mut dyn MuxIo) {
        let mut sec = [
            0x00u8, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00, 0x00, 0x00, 0x01, 0xE1, 0x00, 0, 0, 0, 0,
        ];
        let [c0, c1, c2, c3] = ts_crc32(&self.crc_tab, &sec[..12]).to_be_bytes();
        sec[12] = c0;
        sec[13] = c1;
        sec[14] = c2;
        sec[15] = c3;
        let mut p = [0u8; 17];
        p[0] = 0x00; // pointer_field
        p[1..17].copy_from_slice(&sec);
        self.write_packet(io, 0x0000, true, false, 0, &p);
    }

    pub fn send_pmt(&mut self, io: &mut dyn MuxIo) {
        let mut sec = [
            0x02u8, 0xB0, 0x12, 0x00, 0x01, 0xC1, 0x00, 0x00, 0xE1, 0x01, 0xF0, 0x00, 0x1B, 0xE1,
            0x01, 0xF0, 0x00, 0, 0, 0, 0,
        ];
        let [c0, c1, c2, c3] = ts_crc32(&self.crc_tab, &sec[..17]).to_be_bytes();
        sec[17] = c0;
        sec[18] = c1;
        sec[19] = c2;
        sec[20] = c3;
        let mut p = [0u8; 22];
        p[0] = 0x00;
        p[1..22].copy_from_slice(&sec);
        self.write_packet(io, PMT_PID, true, false, 0, &p);
    }

    /// One PES per access unit, PTS only, fragmented across TS packets.
    ///
    /// Under `PcrPlacement::PesStart` the FIRST fragment also carries the PCR
    /// (stamped from `pcr90`, final), which costs 8 bytes of adaptation field
    /// and shrinks its H.264 payload from 170 to 162 bytes -- gal-extender's
    /// measured shipping shape. Under `BlockHead` the PES start carries no PCR
    /// and the block-head packet is stamped at emission instead.
    pub fn send_video(&mut self, io: &mut dyn MuxIo, au: &[u8], pcr90: u64) {
        let pts90 = pcr90.wrapping_add(self.pts_lead_ms.wrapping_mul(90));
        let mut hdr = [0u8; 14];
        hdr[0] = 0;
        hdr[1] = 0;
        hdr[2] = 1;
        hdr[3] = 0xE0;
        hdr[4] = 0;
        hdr[5] = 0; // PES_packet_length 0 = unbounded video
        hdr[6] = 0x84;
        hdr[7] = 0x80;
        hdr[8] = 0x05;
        let pts = pts90 & 0x1_FFFF_FFFF;
        hdr[9] = 0x21 | (((pts >> 29) & 0x0E) as u8);
        hdr[10] = ((pts >> 22) & 0xFF) as u8;
        hdr[11] = 0x01 | (((pts >> 14) & 0xFE) as u8);
        hdr[12] = ((pts >> 7) & 0xFF) as u8;
        hdr[13] = 0x01 | (((pts << 1) & 0xFE) as u8);

        let first_has_pcr = self.pcr_placement == PcrPlacement::PesStart;
        // With a PCR aboard, the adaptation field eats 8 bytes of the first
        // packet's payload space. Continuations never carry PCR either way.
        let first_avail = if first_has_pcr {
            TS_PACKET_SIZE - 4 - 8
        } else {
            TS_PACKET_SIZE - 4
        };
        let h264_first = first_avail - 14;
        let mut first = [0u8; TS_PACKET_SIZE - 4];
        if let Some(d) = first.get_mut(..14) {
            d.copy_from_slice(&hdr);
        }

        if au.len() <= h264_first {
            let n = 14 + au.len();
            if let Some(dst) = first.get_mut(14..n) {
                dst.copy_from_slice(au);
            }
            if let Some(p) = first.get(..n) {
                self.write_packet(io, VIDEO_PID, true, first_has_pcr, pcr90, p);
            }
            return;
        }
        if let (Some(dst), Some(src)) = (first.get_mut(14..first_avail), au.get(..h264_first)) {
            dst.copy_from_slice(src);
        }
        if let Some(p) = first.get(..first_avail) {
            self.write_packet(io, VIDEO_PID, true, first_has_pcr, pcr90, p);
        }

        let mut offset = h264_first;
        while offset < au.len() {
            let chunk = (au.len() - offset).min(TS_PACKET_SIZE - 4);
            match au.get(offset..offset + chunk) {
                Some(c) => self.write_packet(io, VIDEO_PID, false, false, 0, c),
                None => break, // unreachable: chunk is clamped to the remainder
            }
            offset += chunk;
        }
    }
}

impl Default for Mux {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test consumer: a fixed clock and a Vec. This is the whole point of the
    /// seam -- the format tests need no fd, no tempfile and no platform.
    /// `now_calls` counts clock reads, because `PesStart`'s contract is that the
    /// transmission clock is NEVER consulted.
    struct TestIo {
        out: Vec<u8>,
        now: u64,
        now_calls: u32,
    }

    impl TestIo {
        fn new(now: u64) -> TestIo {
            TestIo {
                out: Vec::new(),
                now,
                now_calls: 0,
            }
        }
    }

    impl MuxIo for TestIo {
        fn now90(&mut self) -> u64 {
            self.now_calls += 1;
            self.now
        }
        fn emit(&mut self, block: &[u8]) {
            self.out.extend_from_slice(block);
        }
    }

    fn mux_to_bytes(f: impl FnOnce(&mut Mux, &mut TestIo)) -> Vec<u8> {
        let mut mux = Mux::new();
        let mut io = TestIo::new(0);
        f(&mut mux, &mut io);
        mux.flush_block(&mut io);
        io.out
    }

    fn packets(v: &[u8]) -> Vec<&[u8]> {
        v.chunks(TS_PACKET_SIZE)
            .filter(|p| p.len() == TS_PACKET_SIZE)
            .collect()
    }

    fn pid_of(p: &[u8]) -> u32 {
        (u32::from(p[1] & 0x1F) << 8) | u32::from(p[2])
    }

    /// Every block is exactly 64 packets of 188 bytes and every packet starts
    /// with the sync byte. The MOST driver takes whole 12032-byte blocks only,
    /// so a short or unaligned block is an EMSGSIZE rather than a picture.
    #[test]
    fn output_is_whole_blocks_of_synced_packets() {
        let v = mux_to_bytes(|m, io| {
            m.send_pat(io);
            m.send_pmt(io);
        });
        assert_eq!(
            v.len() % TS_BLOCK_SIZE,
            0,
            "must be whole 12032-byte blocks"
        );
        assert_eq!(v.len(), TS_BLOCK_SIZE, "one partial block, padded");
        for (i, p) in packets(&v).iter().enumerate() {
            assert_eq!(p[0], 0x47, "packet {i} lost sync");
        }
    }

    /// The tail of a partly-filled block is null packets (PID 0x1FFF), which a
    /// demuxer discards. Padding with anything else would be decoded.
    #[test]
    fn a_partial_block_is_padded_with_null_packets() {
        let v = mux_to_bytes(|m, io| {
            m.send_pat(io);
        });
        let ps = packets(&v);
        assert_eq!(ps.len(), TS_BLOCK_PACKETS);
        assert!(
            ps[2..].iter().all(|p| pid_of(p) == 0x1FFF),
            "tail is null packets"
        );
    }

    /// PSI lands on the right PIDs, and the block opens with a PCR-bearing
    /// packet so every block carries a timestamp.
    #[test]
    fn psi_uses_the_expected_pids_and_the_block_opens_with_a_pcr() {
        let v = mux_to_bytes(|m, io| {
            m.send_pat(io);
            m.send_pmt(io);
        });
        let ps = packets(&v);
        assert_eq!(
            pid_of(ps[0]),
            VIDEO_PID,
            "block_begin's PCR packet comes first"
        );
        assert_eq!(ps[0][3] & 0x30, 0x20, "adaptation field only, no payload");
        assert_eq!(ps[0][5] & 0x10, 0x10, "PCR flag set");
        let pids: Vec<u32> = ps.iter().map(|p| pid_of(p)).collect();
        assert!(pids.contains(&0x0000), "PAT on PID 0");
        assert!(pids.contains(&PMT_PID), "PMT on PID 0x0100");
    }

    /// The PCR stamped into the block is the io clock AT EMISSION, not anything
    /// captured earlier -- that is the "PCR is transmit time" property the
    /// whole seam preserves, and the consumer's virtual clock depends on it.
    #[test]
    fn the_stamped_pcr_is_the_emission_time_clock() {
        let mut mux = Mux::new();
        let mut io = TestIo::new(12_345 * 90);
        mux.send_pat(&mut io);
        mux.flush_block(&mut io);
        let v = io.out;
        let p0 = &packets(&v)[0];
        // adaptation-field PCR at offset 6: undo pcr_encode's packing
        let b = &p0[6..12];
        let pcr = u64::from(b[0]) << 25
            | u64::from(b[1]) << 17
            | u64::from(b[2]) << 9
            | u64::from(b[3]) << 1
            | u64::from(b[4]) >> 7;
        assert_eq!(pcr, 12_345 * 90, "PCR must be io.now90() at emit time");
    }

    /// An access unit larger than one packet is fragmented, and only the FIRST
    /// fragment carries the PES header and the payload-unit-start flag. Setting
    /// it on a continuation makes the demuxer start a new PES mid-frame.
    #[test]
    fn a_large_access_unit_fragments_with_one_pusi() {
        let au = vec![0xABu8; 1000];
        let v = mux_to_bytes(|m, io| {
            m.send_video(io, &au, 90_000);
        });
        let vid: Vec<&[u8]> = packets(&v)
            .into_iter()
            .filter(|p| pid_of(p) == VIDEO_PID && (p[3] & 0x10) != 0)
            .collect();
        assert!(vid.len() > 1, "1000 bytes must span several packets");
        let starts = vid.iter().filter(|p| p[1] & 0x40 != 0).count();
        assert_eq!(starts, 1, "exactly one payload_unit_start_indicator");
        let first = vid.iter().find(|p| p[1] & 0x40 != 0).unwrap();
        let body = 4 + if (first[3] >> 4) & 3 == 3 {
            1 + first[4] as usize
        } else {
            0
        };
        assert_eq!(
            &first[body..body + 4],
            &[0x00, 0x00, 0x01, 0xE0],
            "PES start code"
        );
    }

    /// Continuity counters must advance by one per payload packet on a PID. A
    /// demuxer treats a break as a discontinuity and drops the PES it was
    /// assembling -- which is exactly what a lost block looks like.
    #[test]
    fn continuity_counters_advance_without_gaps() {
        let au = vec![0x5Au8; 3000];
        let v = mux_to_bytes(|m, io| {
            m.send_pat(io);
            m.send_pmt(io);
            m.send_video(io, &au, 90_000);
        });
        let mut prev: Option<u8> = None;
        for p in packets(&v) {
            if pid_of(p) != VIDEO_PID || (p[3] & 0x10) == 0 {
                continue;
            }
            let cc = p[3] & 0x0F;
            if let Some(q) = prev {
                assert_eq!(cc, (q + 1) & 0x0F, "continuity break on the video PID");
            }
            prev = Some(cc);
        }
        assert!(prev.is_some(), "saw no video payload packets");
    }

    /// PTS is pcr90 + the configured lead, packed into the PES header. The
    /// probe's virtual-clock property (--fast) is now a consumer property built
    /// on this one: whoever supplies pcr90 supplies the timeline.
    #[test]
    fn pts_is_pcr90_plus_the_configured_lead() {
        let v = mux_to_bytes(|m, io| {
            m.pts_lead_ms = 0;
            m.send_video(io, &[0u8; 20], 1_000 * 90);
        });
        let first = packets(&v)
            .into_iter()
            .find(|p| pid_of(p) == VIDEO_PID && p[1] & 0x40 != 0)
            .unwrap();
        let body = 4 + if (first[3] >> 4) & 3 == 3 {
            1 + first[4] as usize
        } else {
            0
        };
        let b = &first[body + 9..body + 14];
        let pts = u64::from((b[0] >> 1) & 0x07) << 30
            | u64::from(b[1]) << 22
            | u64::from((b[2] >> 1) & 0x7F) << 15
            | u64::from(b[3]) << 7
            | u64::from((b[4] >> 1) & 0x7F);
        assert_eq!(pts, 1_000 * 90, "PTS must follow the caller's pcr90");
    }

    /// The PSI tables are fixed sections with a CRC; a wrong one is rejected
    /// outright by a demuxer and would look exactly like "the cluster ignored
    /// us".
    ///
    /// The expected CRC is fixed independently of this function. The section
    /// contains protocol fields only, with no device or person identifiers.
    #[test]
    fn psi_crc_matches_the_shipping_muxers_own_stream() {
        let tab = crc32_table();
        let pat = [
            0x00u8, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00, 0x00, 0x00, 0x01, 0xE1, 0x00,
        ];
        assert_eq!(ts_crc32(&tab, &pat), 0xE8F9_5E7D);
    }

    fn pcr_of(p: &[u8]) -> u64 {
        u64::from(p[6]) << 25
            | u64::from(p[7]) << 17
            | u64::from(p[8]) << 9
            | u64::from(p[9]) << 1
            | u64::from(p[10]) >> 7
    }

    /// The PES-start policy puts the build-time PCR in the first video
    /// packet. Its adaptation field reduces video data from 170 to 162 bytes.
    /// This policy makes no PCR-only packets.
    #[test]
    fn pes_start_placement_puts_the_pcr_on_the_pes_start_packet() {
        let au = vec![0xABu8; 1000];
        let mut mux = Mux::new();
        mux.pcr_placement = PcrPlacement::PesStart;
        mux.pts_lead_ms = 0;
        let mut io = TestIo::new(777);
        mux.send_video(&mut io, &au, 42_000);
        mux.flush_block(&mut io);
        let v = io.out;

        let ps = packets(&v);
        // no bare PCR packets: every video packet must carry payload
        assert!(
            ps.iter()
                .all(|p| pid_of(p) != VIDEO_PID || (p[3] & 0x10) != 0),
            "PesStart must not emit adaptation-only PCR packets"
        );
        let first = ps.iter().find(|p| p[1] & 0x40 != 0).expect("a PES start");
        assert_eq!(pid_of(first), VIDEO_PID);
        assert_eq!(first[3] & 0x30, 0x30, "adaptation field + payload");
        assert_eq!(first[5] & 0x10, 0x10, "PCR flag on the PES start");
        assert_eq!(pcr_of(first), 42_000, "build-time pcr90, not the io clock");
        // 188 - 4 header - 8 PCR adaptation - 14 PES header = 162 bytes of ES
        assert_eq!(
            first[4], 7,
            "adaptation_field_length 7 = flags + 6-byte PCR"
        );
        let es = 188 - 4 - 8 - 14;
        assert_eq!(es, 162);
        assert!(
            first[12 + 14..].iter().all(|&b| b == 0xAB),
            "ES fills the rest"
        );
    }

    /// The other half of `PesStart`'s contract: the transmission clock is never
    /// consulted -- restamping is a `BlockHead` behaviour.
    #[test]
    fn pes_start_placement_never_reads_the_io_clock() {
        let au = vec![0x5Au8; 3000];
        let mut mux = Mux::new();
        mux.pcr_placement = PcrPlacement::PesStart;
        let mut io = TestIo::new(999);
        mux.send_pat(&mut io);
        mux.send_pmt(&mut io);
        mux.send_video(&mut io, &au, 90_000);
        mux.flush_block(&mut io);
        assert_eq!(io.now_calls, 0, "PesStart must never consult now90()");
        // and a bare heartbeat produces nothing under this placement
        let before = io.out.len();
        mux.block_begin();
        mux.flush_block(&mut io);
        assert_eq!(io.out.len(), before, "no bare PCR heartbeat under PesStart");
    }

    /// A PCR is a 33-bit value split across 6 bytes with reserved bits set; a
    /// mis-packed one walks the decoder's clock recovery.
    #[test]
    fn pcr_packs_to_the_documented_layout() {
        let mut b = [0u8; 6];
        pcr_encode(&mut b, 0);
        assert_eq!(b, [0, 0, 0, 0, 0x7E, 0]);
        pcr_encode(&mut b, 0x1_FFFF_FFFF);
        assert_eq!(b, [0xFF, 0xFF, 0xFF, 0xFF, 0xFE, 0x00]);
    }
}
