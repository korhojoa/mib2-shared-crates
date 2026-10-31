// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 korhojoa

//! H.264 bit reader, parameter-set parser and CABAC skip-slice generator.
//!
//! The generator makes non-reference P slices. It uses fixed buffers and has
//! no clock or I/O. Tests check parsed fields and encoded output.

/// H.264 Table 9-44, rangeTabLPS.
const RANGE_LPS: [[u8; 4]; 64] = [
    [128, 176, 208, 240],
    [128, 167, 197, 227],
    [128, 158, 187, 216],
    [123, 150, 178, 205],
    [116, 142, 169, 195],
    [111, 135, 160, 185],
    [105, 128, 152, 175],
    [100, 122, 144, 166],
    [95, 116, 137, 158],
    [90, 110, 130, 150],
    [85, 104, 123, 142],
    [81, 99, 117, 135],
    [77, 94, 111, 128],
    [73, 89, 105, 122],
    [69, 85, 100, 116],
    [66, 80, 95, 110],
    [62, 76, 90, 104],
    [59, 72, 86, 99],
    [56, 69, 81, 94],
    [53, 65, 77, 89],
    [51, 62, 73, 85],
    [48, 59, 69, 80],
    [46, 56, 66, 76],
    [43, 53, 63, 72],
    [41, 50, 59, 69],
    [39, 48, 56, 65],
    [37, 45, 54, 62],
    [35, 43, 51, 59],
    [33, 41, 48, 56],
    [32, 39, 46, 53],
    [30, 37, 43, 50],
    [28, 35, 41, 48],
    [27, 33, 39, 45],
    [26, 31, 37, 43],
    [24, 30, 35, 41],
    [23, 28, 33, 39],
    [22, 27, 32, 37],
    [21, 26, 30, 35],
    [20, 24, 29, 33],
    [19, 23, 27, 31],
    [18, 22, 26, 30],
    [17, 21, 25, 28],
    [16, 20, 23, 27],
    [15, 19, 22, 25],
    [14, 18, 21, 24],
    [14, 17, 20, 23],
    [13, 16, 19, 22],
    [12, 15, 18, 21],
    [12, 14, 17, 20],
    [11, 14, 16, 19],
    [11, 13, 15, 18],
    [10, 12, 15, 17],
    [10, 12, 14, 16],
    [9, 11, 13, 15],
    [9, 11, 12, 14],
    [8, 10, 12, 14],
    [8, 9, 11, 13],
    [7, 9, 11, 12],
    [7, 9, 10, 12],
    [7, 8, 10, 11],
    [6, 8, 9, 11],
    [6, 7, 9, 10],
    [6, 7, 8, 9],
    [2, 2, 2, 2],
];
/// H.264 Table 9-45, transIdxLPS.
const TRANS_LPS: [u8; 64] = [
    0, 0, 1, 2, 2, 4, 4, 5, 6, 7, 8, 9, 9, 11, 11, 12, 13, 13, 15, 15, 16, 16, 18, 18, 19, 19, 21,
    21, 23, 22, 23, 24, 24, 25, 26, 26, 27, 27, 28, 29, 29, 30, 30, 30, 31, 32, 32, 33, 33, 33, 34,
    34, 35, 35, 35, 36, 36, 36, 37, 37, 37, 38, 38, 63,
];
/// H.264 Table 9-45, transIdxMPS.
const TRANS_MPS: [u8; 64] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26,
    27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50,
    51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 62, 63,
];

/* ---- Exp-Golomb bit reader over a de-emulated RBSP ---- */

struct BitR<'a> {
    b: &'a [u8],
    nbits: usize,
    pos: usize,
    failed: bool,
}

impl<'a> BitR<'a> {
    fn new(b: &'a [u8]) -> BitR<'a> {
        BitR {
            nbits: b.len() * 8,
            b,
            pos: 0,
            failed: false,
        }
    }
    /// Refuse a field beyond the input or the integer width.
    fn u(&mut self, k: u32) -> u32 {
        if k > 32 || k as usize > self.nbits.saturating_sub(self.pos) {
            self.failed = true;
            return 0;
        }
        let mut v: u32 = 0;
        for _ in 0..k {
            let bit = self
                .b
                .get(self.pos >> 3)
                .map_or(0, |&byte| (byte >> (7 - (self.pos & 7))) & 1);
            v = (v << 1) | u32::from(bit);
            self.pos += 1;
        }
        v
    }
    fn ue(&mut self) -> u32 {
        let mut zeros = 0;
        loop {
            if self.pos >= self.nbits || zeros == 32 {
                self.failed = true;
                return u32::MAX;
            }
            if self.u(1) != 0 {
                break;
            }
            zeros += 1;
        }
        ((1u32 << zeros) - 1).wrapping_add(self.u(zeros))
    }
    fn se(&mut self) -> i32 {
        let k = self.ue();
        if k & 1 != 0 {
            (k.wrapping_add(1) / 2).cast_signed()
        } else {
            -((k / 2).cast_signed())
        }
    }
}

/// Strip emulation-prevention bytes into `out` and return the byte count.
/// Stop at output capacity. Parse an oversized NAL from the truncated RBSP.
fn deemul(input: &[u8], out: &mut [u8]) -> usize {
    let mut o = 0usize;
    let mut z = 0u32;
    for &x in input {
        if z >= 2 && x == 0x03 {
            z = 0;
            continue;
        }
        match out.get_mut(o) {
            Some(slot) => *slot = x,
            None => break,
        }
        o += 1;
        z = if x == 0 { z + 1 } else { 0 };
    }
    o
}

/// Everything the skip generator needs, parsed once from the SPS/PPS.
/// Defaults are inactive until a supported SPS and PPS have been parsed.
#[allow(
    clippy::struct_excessive_bools,
    reason = "these ARE the SPS/PPS flag bits; the slice-header writer reads each one independently"
)]
pub struct H264 {
    pub ready: bool,
    pub log2_fn: u32,
    pub log2_poc: u32,
    pub pic_mbs: i32,
    pub pic_qp: i32,
    pub deblock_pres: bool,
    /// PPS `bottom_field_pic_order_in_frame_present_flag`. When set, a slice
    /// header carries `delta_pic_order_cnt_bottom` right after `pic_order_cnt_lsb`;
    /// omitting it misaligns everything that follows, and the decoder reports a
    /// invalid `cabac_init_idc`. The bit must match the active PPS.
    pub pic_order_present: bool,
    /// SPS `pic_order_cnt_type`: 0 uses an explicit picture-order count. Type 2 derives it.
    /// Decides whether a slice header carries `pic_order_cnt_lsb` at all.
    pub poc_type: u32,
    have_sps: bool,
    have_pps: bool,
    /// Last real slice's `frame_num`, and the POC the filler continues from.
    pub last_fn: u32,
    pub fill_poc: u32,
}

fn has_chroma_fields(profile: u32) -> bool {
    matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    )
}

impl H264 {
    /// Make initial state that a caller can keep in a static variable.
    /// No mux initialization is required.
    #[must_use]
    pub const fn new() -> Self {
        H264 {
            ready: false,
            log2_fn: 12,
            log2_poc: 13,
            pic_mbs: 50 * 30,
            pic_qp: 26,
            deblock_pres: true,
            pic_order_present: false,
            poc_type: 0,
            have_sps: false,
            have_pps: false,
            last_fn: 0,
            fill_poc: 0,
        }
    }
}

impl Default for H264 {
    fn default() -> Self {
        H264::new()
    }
}

/// Maximum supported picture size in macroblocks. This bounds filler work.
pub const MAX_PIC_MBS: i32 = 65_536;

impl H264 {
    fn parse_sps(&mut self, nal: &[u8]) {
        self.have_sps = false;
        let mut rb = [0u8; 80];
        let m = deemul(nal, &mut rb);
        // `deemul` bounds m to the buffer length. Use a checked slice so
        // this call cannot introduce a bounds panic.
        let mut r = BitR::new(rb.get(..m).unwrap_or(&[]));
        let prof = r.u(8);
        r.u(8);
        r.u(8);
        if r.ue() != 0 {
            return;
        } // Only SPS zero is supported.
        if has_chroma_fields(prof) {
            if r.ue() != 1 || r.ue() != 0 || r.ue() != 0 {
                return;
            }
            r.u(1);
            if r.u(1) != 0 {
                self.have_sps = false; // scaling matrix: bail
                return;
            }
        }
        // A corrupt stream can return 0xffffffff. Use wrapping arithmetic;
        // callers treat an invalid parse as not ready.
        self.log2_fn = r.ue().wrapping_add(4);
        // pic_order_cnt_type. Two of the three are usable and they need
        // different slice headers:
        //
        //   0 - pic_order_cnt_lsb is present in the slice header.
        //   2 - picture order derives from frame_num. No POC field is present.
        //   1 - would need delta_pic_order_cnt[]; nothing here produces it.
        //
        // Types 0 and 2 have supported skip-slice headers.
        self.poc_type = r.ue();
        if self.poc_type == 0 {
            self.log2_poc = r.ue().wrapping_add(4);
        } else if self.poc_type != 2 {
            self.have_sps = false;
            return;
        }
        r.ue(); // max_num_ref_frames
        r.u(1); // gaps_allowed
        let wmbs = r.ue().wrapping_add(1);
        let hmap = r.ue().wrapping_add(1);
        if r.u(1) == 0 {
            self.have_sps = false; // frame_mbs_only only
            return;
        }
        let Some(mbs) = wmbs.checked_mul(hmap).and_then(|n| i32::try_from(n).ok()) else {
            return;
        };
        if r.failed
            || !(4..=16).contains(&self.log2_fn)
            || (self.poc_type == 0 && !(4..=16).contains(&self.log2_poc))
            || !(1..=MAX_PIC_MBS).contains(&mbs)
        {
            return;
        }
        self.pic_mbs = mbs;
        self.have_sps = true;
    }

    fn parse_pps(&mut self, nal: &[u8]) {
        self.have_pps = false;
        let mut rb = [0u8; 64];
        let m = deemul(nal, &mut rb);
        let mut r = BitR::new(rb.get(..m).unwrap_or(&[])); // see parse_sps
        if r.ue() != 0 || r.ue() != 0 {
            return;
        } // PPS zero refers to SPS zero.
        let ent = r.u(1);
        self.pic_order_present = r.u(1) != 0;
        if r.ue().wrapping_add(1) != 1 {
            self.have_pps = false; // num_slice_groups
            return;
        }
        // The generated P header has one default reference and no weight table.
        if r.ue() != 0 || r.ue() != 0 || r.u(1) != 0 || r.u(2) != 0 {
            return;
        }
        self.pic_qp = r.se().wrapping_add(26); // pic_init_qp
        r.se();
        r.se(); // qs, chroma offset
        self.deblock_pres = r.u(1) != 0;
        r.u(1); // constrained_intra_pred_flag does not change a P skip slice.
        let redundant = r.u(1);
        if ent != 1 || redundant != 0 || r.failed || !(0..=51).contains(&self.pic_qp) {
            self.have_pps = false; // CABAC required
            return;
        }
        self.have_pps = true;
    }

    fn track_slice(&mut self, nal: &[u8], nal_type: u8) {
        let mut rb = [0u8; 24];
        let m = deemul(nal, &mut rb);
        let mut r = BitR::new(rb.get(..m).unwrap_or(&[])); // see parse_sps
        r.ue();
        r.ue();
        if r.ue() != 0 {
            self.have_pps = false;
            return;
        }
        let frame_num = r.u(self.log2_fn);
        if nal_type == 5 {
            r.ue(); // idr_pic_id
        }
        // Only poc_type 0 puts a POC in the slice header. Under type 2 it is
        // derived from frame_num, so there is nothing here to resync to and
        // reading log2_poc bits would consume whatever followed instead.
        if self.poc_type == 0 {
            let poc = r.u(self.log2_poc);
            if !r.failed {
                self.fill_poc = poc;
            }
        }
        if r.failed {
            self.have_pps = false;
        } else {
            self.last_fn = frame_num;
        }
    }

    /// Scan an Annex-B access unit and observe SPS/PPS/slices.
    pub fn observe(&mut self, au: &[u8]) {
        let len = au.len();
        let mut i = 0usize;
        while i + 4 < len {
            // Inside the loop bound every get() is Some; the match spells the
            // same start-code tests the indexed version did, with the
            // impossible None folding into "no start code here".
            let sc = match au.get(i..i + 4) {
                Some(&[0, 0, 0, 1]) => 4,
                Some(&[0, 0, 1, _]) => 3,
                _ => 0,
            };
            if sc == 0 {
                i += 1;
                continue;
            }
            let body = i + sc;
            let mut j = body + 1;
            while j + 3 < len {
                let next_sc = matches!(au.get(j..j + 4), Some(&[0, 0, 1, _] | &[0, 0, 0, 1]));
                if next_sc {
                    break;
                }
                j += 1;
            }
            let end = if j + 3 < len { j } else { len };
            let t = au.get(body).map_or(0, |&b| b & 0x1F);
            let payload = au.get(body + 1..end).unwrap_or(&[]);
            match t {
                7 => self.parse_sps(payload),
                8 => self.parse_pps(payload),
                1 | 5 => self.track_slice(payload, t),
                _ => {}
            }
            i = end;
        }
        self.ready = self.have_sps && self.have_pps;
    }
}

/* ---- bit writer + CABAC encoder for the skip slice ---- */

/// Count all requested bits to preserve CABAC arithmetic. Discard bytes beyond
/// the 4096-byte output buffer and set the overflow flag.
struct BitW {
    buf: [u8; 4096],
    nbits: usize,
    ovf: bool,
}

impl BitW {
    fn new() -> BitW {
        BitW {
            buf: [0; 4096],
            nbits: 0,
            ovf: false,
        }
    }
    fn bit(&mut self, b: u32) {
        let bitpos = self.nbits & 7;
        match self.buf.get_mut(self.nbits >> 3) {
            Some(slot) => {
                if bitpos == 0 {
                    *slot = 0;
                }
                if b != 0 {
                    *slot |= 1 << (7 - bitpos);
                }
            }
            None => self.ovf = true,
        }
        self.nbits += 1;
    }
    fn u(&mut self, v: u32, k: u32) {
        if k > 32 {
            self.ovf = true;
            return;
        }
        for i in (0..k).rev() {
            self.bit((v >> i) & 1);
        }
    }
    fn ue(&mut self, v: u32) {
        if v == u32::MAX {
            self.ovf = true;
            return;
        }
        let x = v.wrapping_add(1);
        // Limit the shift count to less than the integer width.
        let mut n = 0u32;
        while n < 32 && (x >> n) != 0 {
            n += 1;
        }
        // n >= 1 always (x >= 1); saturating spells out that 0 - 1 cannot occur
        for _ in 0..n.saturating_sub(1) {
            self.bit(0);
        }
        self.u(x, n);
    }
    fn se(&mut self, v: i32) {
        // wrapping_neg: identical to -v for every reachable value (callers pass
        // 0) and spells out that i32::MIN cannot turn into an overflow check.
        let k = match v.cmp(&0) {
            core::cmp::Ordering::Equal => 0u32,
            core::cmp::Ordering::Greater => 2u32.wrapping_mul(v.cast_unsigned()).wrapping_sub(1),
            core::cmp::Ordering::Less => 2u32.wrapping_mul(v.wrapping_neg().cast_unsigned()),
        };
        self.ue(k);
    }
}

struct Cabac {
    range: u32,
    low: u32,
    first: bool,
    outst: u32,
    state: usize,
    mps: i32,
}

impl Cabac {
    #[allow(
        clippy::cast_sign_loss,
        reason = "pre is clamped to 1..=126 two lines below, so 63 - pre (taken when pre <= 63) and pre - 64 (taken when pre >= 64) are both in 0..=62"
    )]
    fn new(qp: i32) -> Cabac {
        let q = qp.clamp(0, 51);
        let pre = (((23 * q) >> 4) + 33).clamp(1, 126);
        if pre <= 63 {
            Cabac {
                range: 510,
                low: 0,
                first: true,
                outst: 0,
                state: (63 - pre) as usize,
                mps: 0,
            }
        } else {
            Cabac {
                range: 510,
                low: 0,
                first: true,
                outst: 0,
                state: (pre - 64) as usize,
                mps: 1,
            }
        }
    }
    fn putbit(&mut self, w: &mut BitW, b: u32) {
        if self.first {
            self.first = false;
        } else {
            w.bit(b);
        }
        while self.outst > 0 {
            w.bit(1 - b);
            self.outst -= 1;
        }
    }
    fn renorm(&mut self, w: &mut BitW) {
        while self.range < 256 {
            if self.low < 256 {
                self.putbit(w, 0);
            } else if self.low >= 512 {
                self.low = self.low.wrapping_sub(512);
                self.putbit(w, 1);
            } else {
                self.low = self.low.wrapping_sub(256);
                self.outst += 1;
            }
            self.range <<= 1;
            self.low <<= 1;
        }
    }
    /// `mb_skip_flag` = 1, context 11.
    ///
    /// `state` is provably in 0..64 (it only ever comes from `Cabac::new`'s
    /// clamp or the 64-entry `TRANS` tables), so the `get()` fallbacks below are
    /// unreachable; they exist so that no input, however corrupt, can turn
    /// this into a panic. The fallback values are the tables' own terminal
    /// entries. Use wrapping arithmetic for integer state updates.
    fn skip(&mut self, w: &mut BitW) {
        let q_idx = ((self.range >> 6) & 3) as usize;
        let lps = u32::from(
            RANGE_LPS
                .get(self.state)
                .and_then(|row| row.get(q_idx))
                .copied()
                .unwrap_or(2),
        );
        self.range = self.range.wrapping_sub(lps);
        if self.mps == 1 {
            self.state = TRANS_MPS.get(self.state).copied().unwrap_or(63) as usize;
        } else {
            self.low = self.low.wrapping_add(self.range);
            self.range = lps;
            if self.state == 0 {
                self.mps = 1 - self.mps;
            }
            self.state = TRANS_LPS.get(self.state).copied().unwrap_or(63) as usize;
        }
        self.renorm(w);
    }
    fn term(&mut self, w: &mut BitW, bin_val: bool) {
        self.range = self.range.wrapping_sub(2);
        if bin_val {
            self.low = self.low.wrapping_add(self.range);
            self.range = 2;
            self.renorm(w);
            let b = (self.low >> 9) & 1;
            self.putbit(w, b);
            w.bit((self.low >> 8) & 1);
            w.bit(1);
        } else {
            self.renorm(w);
        }
    }
}

/// Build a non-reference all-skip P access unit (Annex-B) into `out`.
/// Returns the byte count, or 0 on failure (buffer too small, or the slice
/// outgrew the bit writer, or parameter sets are not supported and ready).
///
/// Generate a non-reference frame with picture-order count type zero.
pub fn build_skip_au(out: &mut [u8], frame_num: u32, poc: u32, h: &H264) -> usize {
    build_skip_au_ref(out, frame_num, poc, h, 0)
}

/// As above, but the caller chooses `nal_ref_idc`.
///
/// With picture-order count type 0, non-reference filler preserves the source reference chain.
/// With type 2, picture order derives from `frame_num`.
/// Distinct reference pictures require advancing frame numbers in that mode.
/// The caller must own the reference chain before it inserts reference filler.
/// `ref_idc` zero selects non-reference filler. Values 1 through 3 select reference filler.
/// Other values return zero and leave the output unchanged.
#[allow(
    clippy::many_single_char_names,
    reason = "short local names describe bit coding state and image dimensions"
)]
pub fn build_skip_au_ref(out: &mut [u8], frame_num: u32, poc: u32, h: &H264, ref_idc: u8) -> usize {
    if ref_idc > 3
        || !(4..=16).contains(&h.log2_fn)
        || (h.poc_type == 0 && !(4..=16).contains(&h.log2_poc))
        || !matches!(h.poc_type, 0 | 2)
        || !(1..=MAX_PIC_MBS).contains(&h.pic_mbs)
        || !(0..=51).contains(&h.pic_qp)
    {
        return 0;
    }
    if !h.ready || !h.have_sps || !h.have_pps {
        return 0;
    }
    let mut w = BitW::new();
    w.ue(0); // first_mb_in_slice
    w.ue(5); // slice_type = P
    w.ue(0); // pic_parameter_set_id
    w.u(frame_num, h.log2_fn);
    if h.poc_type == 0 {
        w.u(poc, h.log2_poc);
        if h.pic_order_present {
            // delta_pic_order_cnt_bottom lives inside the poc_type==0 branch of
            // the slice header, so it is absent under type 2 regardless of what
            // the PPS flag says.
            w.se(0);
        }
    }
    w.u(0, 1); // num_ref_idx_active_override_flag
    w.u(0, 1); // ref_pic_list_modification_flag_l0
    if ref_idc != 0 {
        // dec_ref_pic_marking(), and its position is load-bearing: the slice
        // header puts it AFTER ref_pic_list_modification(), not next to the POC
        // fields. Non-IDR form is the single
        // adaptive_ref_pic_marking_mode_flag = 0 (sliding window). Present only
        // when nal_ref_idc != 0; one bit in the wrong place misaligns
        // cabac_init_idc and everything after it.
        w.u(0, 1);
    }
    w.ue(0); // cabac_init_idc = 0
    w.se(0); // slice_qp_delta
    if h.deblock_pres {
        w.ue(1); // disable_deblocking_filter_idc = 1
    }
    w.bit(1); // cabac_alignment_one_bit
    while w.nbits & 7 != 0 {
        w.bit(0);
    }
    let mut c = Cabac::new(h.pic_qp);
    for i in 0..h.pic_mbs {
        c.skip(&mut w);
        c.term(&mut w, i == h.pic_mbs.wrapping_sub(1));
        if w.ovf {
            return 0;
        }
    }
    while w.nbits & 7 != 0 {
        w.bit(0);
    }
    if w.ovf {
        return 0; // slice outgrew buf: refuse, never read past it
    }
    let rbsp = w.nbits >> 3;

    // start code, then the NAL header: nal_ref_idc in bits 6:5, type = 1
    // (non-IDR slice). ref_idc zero selects a non-reference picture.
    // A nonzero value selects a reference picture.
    let head: &mut [u8; 5] = match out.get_mut(..5).and_then(|s| s.try_into().ok()) {
        Some(h) => h,
        None => return 0, // caller buffer too small for even the header
    };
    *head = [0, 0, 0, 1, (ref_idc << 5) | 0x01];
    escape(&w.buf[..rbsp], out, 5)
}

/// Write `rbsp` into `out` from offset `o` with emulation-prevention bytes
/// inserted. Returns the end offset, or 0 when `out` is too small (refuse,
/// never clip).
fn escape(rbsp: &[u8], out: &mut [u8], mut o: usize) -> usize {
    let mut z = 0u32;
    for &x in rbsp {
        if z >= 2 && x <= 3 {
            match out.get_mut(o) {
                Some(slot) => *slot = 3,
                None => return 0,
            }
            o += 1;
            z = 0;
        }
        match out.get_mut(o) {
            Some(slot) => *slot = x,
            None => return 0,
        }
        o += 1;
        z = if x == 0 { z + 1 } else { 0 };
    }
    o
}

/// A frame cropping window in luma pixels of the coded frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// What `sps_set_crop` wrote: the NAL length, the coded frame size and the
/// window it actually set (the request rounded down to the crop unit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CropDone {
    pub len: usize,
    pub coded_w: u32,
    pub coded_h: u32,
    pub crop: Crop,
}

/// Why `sps_set_crop` left an SPS alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CropError {
    /// The NAL is not an SPS (type 7).
    NotSps,
    /// The SPS is longer than the parse buffer.
    TooLong,
    /// `seq_scaling_matrix_present_flag` is set (not parsed here).
    ScalingMatrix,
    /// The window does not fit in the coded frame, or is empty.
    OutOfFrame,
    /// `out` is too small.
    OutTooSmall,
}

/// Read chroma format and skip frame number and picture order fields.
fn crop_chroma_and_order(r: &mut BitR<'_>) -> Result<(u32, bool), CropError> {
    let profile = r.u(8);
    r.u(16); // constraint flags, level_idc
    r.ue(); // seq_parameter_set_id
    let mut chroma = 1u32;
    let mut separate = false;
    if has_chroma_fields(profile) {
        chroma = r.ue();
        if chroma == 3 {
            separate = r.u(1) != 0;
        }
        r.ue(); // bit_depth_luma_minus8
        r.ue(); // bit_depth_chroma_minus8
        r.u(1); // qpprime_y_zero_transform_bypass_flag
        if r.u(1) != 0 {
            return Err(CropError::ScalingMatrix);
        }
    }
    r.ue(); // log2_max_frame_num_minus4
    match r.ue() {
        0 => {
            r.ue(); // log2_max_pic_order_cnt_lsb_minus4
        }
        1 => {
            r.u(1);
            r.se();
            r.se();
            let n = r.ue();
            for _ in 0..n.min(255) {
                r.se();
            }
        }
        _ => {}
    }
    Ok((chroma, separate))
}

/// Set the `frame_cropping` fields of one SPS so that a decoder outputs only
/// `crop` of the coded frame. `nal` is the SPS without its start code (NAL
/// header byte first); the rewritten NAL goes to `out`. Every other field
/// and the VUI are copied bit for bit. The window is rounded down to the
/// crop unit (2 luma pixels for 4:2:0 progressive).
///
/// # Errors
/// Returns `NotSps` for a non-SPS NAL, `TooLong` for an oversized parse input,
/// `ScalingMatrix` for unsupported scaling matrices, `OutOfFrame` for an empty
/// or out-of-frame window or an unusable RBSP tail, and `OutTooSmall` when the
/// rewritten NAL cannot fit in the internal writer or the output buffer.
pub fn sps_set_crop(nal: &[u8], crop: Crop, out: &mut [u8]) -> Result<CropDone, CropError> {
    let (&hdr, body) = nal.split_first().ok_or(CropError::NotSps)?;
    if hdr & 0x1F != 7 {
        return Err(CropError::NotSps);
    }
    let mut rb = [0u8; 256];
    if body.len() > rb.len() {
        return Err(CropError::TooLong);
    }
    let m = deemul(body, &mut rb);
    let rb = rb.get(..m).unwrap_or(&[]);
    let mut r = BitR::new(rb);
    let (chroma, separate) = crop_chroma_and_order(&mut r)?;
    r.ue(); // max_num_ref_frames
    r.u(1); // gaps_in_frame_num_value_allowed_flag
    let wmbs = r.ue().wrapping_add(1);
    let hmap = r.ue().wrapping_add(1);
    let frame_mbs_only = r.u(1);
    if frame_mbs_only == 0 {
        r.u(1); // mb_adaptive_frame_field_flag
    }
    r.u(1); // direct_8x8_inference_flag
    let head_bits = r.pos;
    if r.u(1) != 0 {
        r.ue();
        r.ue();
        r.ue();
        r.ue();
    }
    let tail_from = r.pos;

    let coded_w = wmbs.wrapping_mul(16);
    let coded_h = hmap.wrapping_mul(16).wrapping_mul(2 - frame_mbs_only);
    let (sub_w, sub_h) = match (chroma, separate) {
        (1, _) => (2, 2),
        (2, _) => (2, 1),
        _ => (1, 1),
    };
    let unit_x = sub_w;
    let unit_y = if chroma == 0 || separate { 1 } else { sub_h } * (2 - frame_mbs_only);
    let (left, width) = (crop.x / unit_x, crop.w / unit_x);
    let (top, height) = (crop.y / unit_y, crop.h / unit_y);
    let right = (coded_w / unit_x)
        .checked_sub(left)
        .and_then(|v| v.checked_sub(width));
    let bottom = (coded_h / unit_y)
        .checked_sub(top)
        .and_then(|v| v.checked_sub(height));
    let (Some(right), Some(bottom)) = (right, bottom) else {
        return Err(CropError::OutOfFrame);
    };
    if width == 0 || height == 0 {
        return Err(CropError::OutOfFrame);
    }

    // The rbsp_stop_one_bit: the last set bit of the RBSP.
    let stop = rb
        .iter()
        .rposition(|&b| b != 0)
        .map(|i| i * 8 + 7 - rb[i].trailing_zeros() as usize);
    let Some(stop) = stop.filter(|&s| s >= tail_from) else {
        return Err(CropError::OutOfFrame);
    };

    let mut w = BitW::new();
    let mut src = BitR::new(rb);
    for _ in 0..head_bits {
        w.bit(src.u(1));
    }
    w.bit(1); // frame_cropping_flag
    w.ue(left);
    w.ue(right);
    w.ue(top);
    w.ue(bottom);
    src.pos = tail_from;
    while src.pos <= stop {
        w.bit(src.u(1));
    }
    while w.nbits & 7 != 0 {
        w.bit(0);
    }
    if w.ovf {
        return Err(CropError::OutTooSmall);
    }
    let first = out.first_mut().ok_or(CropError::OutTooSmall)?;
    *first = hdr;
    let len = escape(&w.buf[..w.nbits >> 3], out, 1);
    if len == 0 {
        return Err(CropError::OutTooSmall);
    }
    Ok(CropDone {
        len,
        coded_w,
        coded_h,
        crop: Crop {
            x: left * unit_x,
            y: top * unit_y,
            w: width * unit_x,
            h: height * unit_y,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// H.264 parameter set from a decoded CarPlay stream.
    /// It contains codec fields only, with no device or person identifiers.
    pub(super) const CARPLAY_SPS: &[u8] = &[
        0x27, 0x64, 0x00, 0x1f, 0xac, 0x13, 0x14, 0x50, 0x32, 0x0f, 0x69, 0xb8, 0x08, 0x68, 0x30,
        0x36, 0x82, 0x21, 0x19, 0x60,
    ];

    fn h_with(poc_type: u32) -> H264 {
        H264 {
            ready: true,
            have_sps: true,
            have_pps: true,
            poc_type,
            log2_fn: 12,
            log2_poc: 13,
            pic_mbs: 1500,
            pic_qp: 26,
            deblock_pres: true,
            pic_order_present: false,
            ..H264::default()
        }
    }

    /// A generated skip slice must parse back to its input fields and bit offsets.
    #[test]
    fn skip_slice_header_parses_back() {
        let h = h_with(0);
        let mut out = [0u8; 4096];
        let n = build_skip_au(&mut out, 641, 642, &h);
        assert!(n > 0, "generator refused to build");

        // start code + NAL header
        assert_eq!(&out[..4], &[0, 0, 0, 1]);
        assert_eq!(out[4] & 0x1F, 1, "nal_unit_type must be 1 (non-IDR slice)");
        assert_eq!(
            (out[4] >> 5) & 3,
            0,
            "nal_ref_idc must be 0 (non-reference)"
        );

        // Re-parse the slice header out of the RBSP.
        let mut rb = [0u8; 64];
        let m = deemul(&out[5..n.min(5 + 40)], &mut rb);
        let mut r = BitR::new(&rb[..m]);
        assert_eq!(r.ue(), 0, "first_mb_in_slice");
        assert_eq!(r.ue(), 5, "slice_type = P (all slices)");
        assert_eq!(r.ue(), 0, "pic_parameter_set_id");
        assert_eq!(r.u(h.log2_fn), 641, "frame_num survives the round trip");
        assert_eq!(r.u(h.log2_poc), 642, "pic_order_cnt_lsb survives");
    }

    /// Under `poc_type` 2 the slice header carries NO POC field at all. Writing
    /// one anyway shifts every field after it, which is the subtle way to make a
    /// slice that still "decodes" into garbage.
    #[test]
    fn poc_type_2_omits_the_poc_field() {
        let h = h_with(2);
        let mut out = [0u8; 4096];
        let n = build_skip_au(&mut out, 7, 999, &h);
        assert!(n > 0);
        let mut rb = [0u8; 64];
        let m = deemul(&out[5..n.min(5 + 40)], &mut rb);
        let mut r = BitR::new(&rb[..m]);
        r.ue();
        r.ue();
        r.ue();
        assert_eq!(r.u(h.log2_fn), 7, "frame_num");
        // Next field must be num_ref_idx_active_override_flag (1 bit, 0), NOT a
        // 13-bit POC. Reading the POC that is not there would consume it.
        assert_eq!(
            r.u(1),
            0,
            "num_ref_idx_active_override_flag follows directly"
        );
    }

    /// `nal_ref_idc` != 0 adds `dec_ref_pic_marking` AFTER the ref-list flag. One
    /// bit in the wrong place misaligns `cabac_init_idc` and everything after.
    #[test]
    fn reference_skip_adds_dec_ref_pic_marking_in_the_right_place() {
        let h = h_with(2);
        let mut out = [0u8; 4096];
        let n = build_skip_au_ref(&mut out, 3, 0, &h, 2);
        assert!(n > 0);
        assert_eq!((out[4] >> 5) & 3, 2, "nal_ref_idc reaches the NAL header");
        let mut rb = [0u8; 64];
        let m = deemul(&out[5..n.min(5 + 40)], &mut rb);
        let mut r = BitR::new(&rb[..m]);
        r.ue();
        r.ue();
        r.ue();
        assert_eq!(r.u(h.log2_fn), 3);
        assert_eq!(r.u(1), 0, "num_ref_idx_active_override_flag");
        assert_eq!(r.u(1), 0, "ref_pic_list_modification_flag_l0");
        assert_eq!(
            r.u(1),
            0,
            "adaptive_ref_pic_marking_mode_flag - present only here"
        );
    }

    #[test]
    fn reference_identifier_matches_the_header_or_is_refused() {
        for poc_type in [0, 2] {
            let h = h_with(poc_type);
            for ref_idc in 0..=3 {
                let mut out = [0u8; 4096];
                let n = build_skip_au_ref(&mut out, 3, 7, &h, ref_idc);
                assert!(n > 0);
                assert_eq!(out[4], (ref_idc << 5) | 1);
                let mut rb = [0u8; 64];
                let m = deemul(&out[5..n.min(45)], &mut rb);
                let mut r = BitR::new(&rb[..m]);
                assert_eq!(r.ue(), 0);
                assert_eq!(r.ue(), 5);
                assert_eq!(r.ue(), 0);
                assert_eq!(r.u(h.log2_fn), 3);
                if poc_type == 0 {
                    assert_eq!(r.u(h.log2_poc), 7);
                }
                assert_eq!(r.u(1), 0);
                assert_eq!(r.u(1), 0);
                if ref_idc != 0 {
                    assert_eq!(r.u(1), 0);
                }
                assert_eq!(r.ue(), 0, "cabac_init_idc");
                assert_eq!(r.se(), 0, "slice_qp_delta");
                assert_eq!(r.ue(), 1, "disable_deblocking_filter_idc");
            }
            for ref_idc in 4..=u8::MAX {
                let mut out = [0xa5u8; 4096];
                assert_eq!(
                    build_skip_au_ref(&mut out, 3, 7, &h, ref_idc),
                    0,
                    "poc_type={poc_type}, ref_idc={ref_idc}"
                );
                assert_eq!(out, [0xa5u8; 4096]);
            }
        }
    }

    /// Emulation prevention: no 00 00 0x sequence may survive in the payload, or
    /// a demuxer resynchronises mid-slice on a false start code.
    #[test]
    fn payload_carries_no_false_start_codes() {
        let h = h_with(0);
        let mut out = [0u8; 4096];
        let n = build_skip_au(&mut out, 0, 0, &h);
        // 00 00 03 is an escape. Only unescaped 00 00 00, 01 and 02 are refused.
        for i in 5..n.saturating_sub(2) {
            let bad = out[i] == 0 && out[i + 1] == 0 && out[i + 2] < 3;
            assert!(!bad, "unescaped 00 00 {:02x} at byte {}", out[i + 2], i);
        }
    }

    /// For every macroblock count, return zero or a complete slice that fits.
    /// The generator must not truncate, overrun or panic.
    #[test]
    fn an_oversized_slice_never_overruns() {
        for mbs in [1i32, 1500, 65_536, 600_000] {
            let mut h = h_with(0);
            h.pic_mbs = mbs;
            let mut out = [0u8; 4096];
            let n = build_skip_au(&mut out, 0, 0, &h);
            assert!(n <= out.len(), "pic_mbs={mbs} returned {n} > buffer");
        }
    }

    /// A picture beyond the work limit must be refused before encoding.
    #[test]
    fn a_picture_above_the_work_limit_is_refused() {
        let mut h = h_with(0);
        h.pic_mbs = MAX_PIC_MBS + 1;
        let mut out = [0u8; 4096];
        assert_eq!(
            build_skip_au(&mut out, 0, 0, &h),
            0,
            "must refuse, not truncate"
        );
    }

    /// And a caller buffer too small for even the header is refused too.
    #[test]
    fn a_tiny_output_buffer_is_refused() {
        let h = h_with(0);
        let mut out = [0u8; 4];
        assert_eq!(build_skip_au(&mut out, 0, 0, &h), 0);
    }

    /// exp-Golomb must round-trip, since every slice-header field rides on it.
    #[test]
    fn exp_golomb_round_trips() {
        for v in [0u32, 1, 2, 3, 7, 8, 255, 256, 4095, 65535] {
            let mut w = BitW::new();
            w.ue(v);
            w.bit(1);
            while w.nbits & 7 != 0 {
                w.bit(0);
            }
            let bytes = w.nbits >> 3;
            let mut r = BitR::new(&w.buf[..bytes]);
            assert_eq!(r.ue(), v, "ue({v}) did not survive");
        }
    }

    /// The real `CarPlay` SPS must parse to the expected geometry, so a
    /// regression in the parser shows up here rather than as a dead cluster.
    #[test]
    fn the_real_carplay_sps_parses_to_the_measured_values() {
        let mut h = H264::default();
        h.observe(&[&[0u8, 0, 0, 1][..], CARPLAY_SPS].concat());
        assert_eq!(h.poc_type, 0, "CarPlay uses pic_order_cnt_type 0");
        assert_eq!(
            h.log2_fn, 12,
            "log2_max_frame_num, from the parameter-set fixture"
        );
        assert_eq!(h.log2_poc, 13, "log2_max_pic_order_cnt_lsb");
        assert_eq!(h.pic_mbs, 1500, "800x480 = 50x30 macroblocks");
    }

    /// An Android Auto SPS as a phone sent it (Baseline, 800x480, with VUI).
    const AA_SPS: &[u8] = &[
        0x67, 0x42, 0x80, 0x1f, 0xda, 0x03, 0x20, 0xf6, 0x9a, 0x80, 0x80, 0x80, 0x83, 0x68, 0x50,
        0x9a, 0x80,
    ];

    /// The four `frame_crop_*_offset` values of an SPS (non-high profile).
    fn crop_offsets(nal: &[u8]) -> Option<[u32; 4]> {
        let mut rb = [0u8; 256];
        let m = deemul(&nal[1..], &mut rb);
        let mut r = BitR::new(&rb[..m]);
        r.u(24);
        r.ue();
        r.ue();
        if r.ue() == 0 {
            r.ue();
        }
        r.ue();
        r.u(1);
        r.ue();
        r.ue();
        if r.u(1) == 0 {
            r.u(1);
        }
        r.u(1);
        (r.u(1) != 0).then(|| [r.ue(), r.ue(), r.ue(), r.ue()])
    }

    #[test]
    fn sps_crop_sets_the_window_and_keeps_the_rest() {
        assert_eq!(crop_offsets(AA_SPS), None, "the phone's SPS has no crop");
        let mut out = [0u8; 64];
        let want = Crop {
            x: 134,
            y: 172,
            w: 1010,
            h: 376,
        };
        // 800x480 cannot hold a 1010 wide window
        assert_eq!(
            sps_set_crop(AA_SPS, want, &mut out),
            Err(CropError::OutOfFrame)
        );
        let small = Crop {
            x: 48,
            y: 60,
            w: 530,
            h: 136,
        };
        let done = sps_set_crop(AA_SPS, small, &mut out).unwrap();
        assert_eq!((done.coded_w, done.coded_h), (800, 480));
        assert_eq!(done.crop, small);
        let s1 = out[..done.len].to_vec();
        // offsets in 2-pixel units: left, right, top, bottom
        assert_eq!(crop_offsets(&s1), Some([24, 111, 30, 142]));
        // the other fields parse as before
        let (mut a, mut b) = (H264::default(), H264::default());
        a.observe(&[&[0u8, 0, 0, 1][..], AA_SPS].concat());
        b.observe(&[&[0u8, 0, 0, 1][..], &s1[..]].concat());
        assert_eq!(
            (a.log2_fn, a.poc_type, a.log2_poc, a.pic_mbs),
            (b.log2_fn, b.poc_type, b.log2_poc, b.pic_mbs)
        );
        // rewriting again is a no-op, so the VUI tail was copied intact
        let mut out2 = [0u8; 64];
        let d2 = sps_set_crop(&s1, small, &mut out2).unwrap();
        assert_eq!(&out2[..d2.len], &s1[..]);
        // an odd request rounds down to the 2-pixel unit
        let d3 = sps_set_crop(
            AA_SPS,
            Crop {
                x: 135,
                y: 173,
                w: 501,
                h: 101,
            },
            &mut out2,
        )
        .unwrap();
        assert_eq!(
            d3.crop,
            Crop {
                x: 134,
                y: 172,
                w: 500,
                h: 100
            }
        );
        assert_eq!(
            sps_set_crop(&[0x68, 0xce], small, &mut out2),
            Err(CropError::NotSps)
        );
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;

    fn pps(pps_id: u32, sps_id: u32, refs: u32, weighted: bool, redundant: bool) -> BitW {
        let mut w = BitW::new();
        w.ue(pps_id);
        w.ue(sps_id);
        w.bit(1);
        w.bit(0);
        w.ue(0);
        w.ue(refs);
        w.ue(0);
        w.bit(u32::from(weighted));
        w.u(0, 2);
        w.se(0);
        w.se(0);
        w.se(0);
        w.bit(1);
        w.bit(0);
        w.bit(u32::from(redundant));
        w.bit(1);
        while w.nbits & 7 != 0 {
            w.bit(0);
        }
        w
    }
    #[test]
    fn filler_requires_the_exact_parameter_syntax_it_emits() {
        let mut h = H264::new();
        h.parse_sps(&super::tests::CARPLAY_SPS[1..]);
        assert!(h.have_sps);
        for args in [
            (0, 0, 0, false, false),
            (1, 0, 0, false, false),
            (0, 1, 0, false, false),
            (0, 0, 1, false, false),
            (0, 0, 0, true, false),
            (0, 0, 0, false, true),
        ] {
            let w = pps(args.0, args.1, args.2, args.3, args.4);
            h.parse_pps(&w.buf[..w.nbits / 8]);
            h.ready = h.have_sps && h.have_pps;
            let valid = args == (0, 0, 0, false, false);
            assert_eq!(h.ready, valid, "{args:?}");
            assert_eq!(build_skip_au(&mut [0u8; 4096], 1, 1, &h) > 0, valid);
        }
        let valid = pps(0, 0, 0, false, false);
        h.parse_pps(&valid.buf[..valid.nbits / 8]);
        assert!(h.have_pps);
        let mut slice = BitW::new();
        slice.ue(0);
        slice.ue(0);
        slice.ue(1);
        slice.u(0, h.log2_fn);
        h.track_slice(&slice.buf[..slice.nbits.div_ceil(8)], 1);
        assert!(!h.have_pps);
        h.parse_pps(&[]);
        assert!(!h.have_pps);
    }
    #[test]
    fn invalid_widths_and_unbounded_work_are_refused_before_output() {
        let mut out = [0x55; 64];
        for width in [0, 3, 17, 33, u32::MAX] {
            let mut h = H264::new();
            h.ready = true;
            h.have_sps = true;
            h.have_pps = true;
            h.log2_fn = width;
            assert_eq!(build_skip_au(&mut out, 0, 0, &h), 0);
            assert_eq!(out, [0x55; 64]);
        }
        for count in [i32::MIN, -1, 0, MAX_PIC_MBS + 1, i32::MAX] {
            let mut h = H264::new();
            h.ready = true;
            h.have_sps = true;
            h.have_pps = true;
            h.pic_mbs = count;
            assert_eq!(build_skip_au(&mut out, 0, 0, &h), 0);
            assert_eq!(out, [0x55; 64]);
        }
        let mut w = BitW::new();
        w.u(0, 33);
        assert!(w.ovf);
        let mut r = BitR::new(&[0; 80]);
        assert_eq!(r.ue(), u32::MAX);
        assert!(r.failed);
        assert!(r.pos <= 32);
        for fn_width in [13, u32::MAX] {
            let mut w = BitW::new();
            w.u(66, 8);
            w.u(0, 8);
            w.u(30, 8);
            w.ue(0);
            w.ue(fn_width);
            w.ue(2);
            w.ue(1);
            w.bit(0);
            w.ue(49);
            w.ue(29);
            w.bit(1);
            let mut h = H264::new();
            h.ready = true;
            h.have_sps = true;
            h.have_pps = true;
            h.parse_sps(&w.buf[..w.nbits.div_ceil(8)]);
            assert!(!h.have_sps);
        }
    }
    #[test]
    fn sps_profile_fields_preserve_baseline_and_high_profile_geometry() {
        for profile in [
            66, 77, 100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135,
        ] {
            let mut bits = BitW::new();
            bits.u(profile, 8);
            bits.u(0, 8);
            bits.u(30, 8);
            bits.ue(0);
            if ![66, 77].contains(&profile) {
                bits.ue(1);
                bits.ue(0);
                bits.ue(0);
                bits.bit(0);
                bits.bit(0);
            }
            bits.ue(8);
            bits.ue(0);
            bits.ue(9);
            bits.ue(1);
            bits.bit(0);
            bits.ue(49);
            bits.ue(29);
            bits.bit(1);
            bits.bit(1);
            bits.bit(0);
            bits.bit(0);
            bits.bit(1);
            while bits.nbits & 7 != 0 {
                bits.bit(0);
            }
            let payload = &bits.buf[..bits.nbits / 8];
            let mut parsed = H264::default();
            parsed.parse_sps(payload);
            assert!(parsed.have_sps, "profile {profile}");
            assert_eq!(
                (parsed.log2_fn, parsed.log2_poc, parsed.pic_mbs),
                (12, 13, 1500),
                "profile {profile}"
            );
            assert_eq!(
                crop_chroma_and_order(&mut BitR::new(payload)),
                Ok((1, false))
            );
        }
    }
}
