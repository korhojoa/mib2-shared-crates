// SPDX-License-Identifier: MIT OR Apache-2.0
//! Parse controller HID interrupt reports into normalized pad state.
//! Reports include the leading transaction header and report identifier.
//! Reject unsupported identifiers and short input.

/// Axis count. Order: 0 LX, 1 LY, 2 RX, 3 RY, 4 L2, 5 R2.
/// Range -32768..32767; triggers use 0..32767 span mapped the same way.
pub const NAXES: usize = 6;
/// Button count (bits 0..17 of [`PadState::buttons`]).
pub const NBUTTONS: usize = 18;

// Button bit indices in the shared-memory report layout.
pub const BTN_SQUARE: u32 = 0;
pub const BTN_CROSS: u32 = 1;
pub const BTN_CIRCLE: u32 = 2;
pub const BTN_TRIANGLE: u32 = 3;
pub const BTN_L1: u32 = 4;
pub const BTN_R1: u32 = 5;
pub const BTN_L2: u32 = 6;
pub const BTN_R2: u32 = 7;
pub const BTN_SHARE: u32 = 8;
pub const BTN_OPTIONS: u32 = 9;
pub const BTN_L3: u32 = 10;
pub const BTN_R3: u32 = 11;
pub const BTN_PS: u32 = 12;
pub const BTN_TOUCHPAD: u32 = 13;
pub const BTN_DPAD_UP: u32 = 14;
pub const BTN_DPAD_DOWN: u32 = 15;
pub const BTN_DPAD_LEFT: u32 = 16;
pub const BTN_DPAD_RIGHT: u32 = 17;

/// Controller family (identity only; the normalized state is uniform).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadType {
    Ds4,
    Ds3,
    Xbox,
}

/// Xbox button-layout selector (`BTGP_XBOX_LAYOUT`): Auto and New both use the
/// Series-style codes. Old uses the earlier firmware layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XboxLayout {
    Auto,
    Old,
    New,
}

impl XboxLayout {
    /// Automatic and New select the newer report layout. Old selects the older layout.
    fn is_newfw(self) -> bool {
        !matches!(self, XboxLayout::Old)
    }
}

/// DS4 full-report (0x11) motion + touch. Raw DS4 units; the SDL side scales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sensors {
    pub gyro: [i16; 3],  // pitch, yaw, roll
    pub accel: [i16; 3], // x, y, z
    pub touch: [Finger; 2],
}

/// One touchpad contact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Finger {
    pub down: bool,
    pub id: u8,
    pub x: u16,
    pub y: u16,
}

/// DS4 battery, from the full report only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Battery {
    pub level: u8, // raw 0..~10
    pub charging: bool,
}

/// Normalized controller state - the pure-data result of parsing one report.
/// Mirrors `btgp_pad_t` content but as a value type (the `#[repr(C)]` shm
/// mirror + seqlock writer live in the `btsvc-shm` crate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PadState {
    pub pad_type: PadType,
    pub axis: [i16; NAXES],
    pub buttons: u32,
    /// DS4 only: this parse came from the 0x11 full report (analog triggers +
    /// battery + sensors). The state machine flips `full_report_active` on it.
    pub full_report: bool,
    pub sensors: Option<Sensors>,
    pub battery: Option<Battery>,
}

impl PadState {
    fn new(pad_type: PadType) -> Self {
        PadState {
            pad_type,
            axis: [0; NAXES],
            buttons: 0,
            full_report: false,
            sensors: None,
            battery: None,
        }
    }

    #[inline]
    #[must_use]
    pub fn button(&self, bit: u32) -> bool {
        self.buttons & (1 << bit) != 0
    }
}

/// Stick byte (0..255, center 0x80) -> Sint16. C: `(int16_t)(v*257 - 32768)`.
#[inline]
fn low_i16(value: i64) -> i16 {
    let bytes = value.to_le_bytes();
    i16::from_le_bytes([bytes[0], bytes[1]])
}

fn stick(v: u8) -> i16 {
    i16::from_le_bytes([v, v ^ 0x80])
}

/// D-pad 8-way hat (low nibble) -> the four DPAD button bits. Shared by DS4 and
/// Xbox. Values above 7 are neutral.
#[inline]
fn hat_bits(hat: u8) -> u32 {
    match hat {
        0 => 1 << BTN_DPAD_UP,                             // N
        1 => (1 << BTN_DPAD_UP) | (1 << BTN_DPAD_RIGHT),   // NE
        2 => 1 << BTN_DPAD_RIGHT,                          // E
        3 => (1 << BTN_DPAD_DOWN) | (1 << BTN_DPAD_RIGHT), // SE
        4 => 1 << BTN_DPAD_DOWN,                           // S
        5 => (1 << BTN_DPAD_DOWN) | (1 << BTN_DPAD_LEFT),  // SW
        6 => 1 << BTN_DPAD_LEFT,                           // W
        7 => (1 << BTN_DPAD_UP) | (1 << BTN_DPAD_LEFT),    // NW
        _ => 0,                                            // 8 = neutral
    }
}

/// Xbox uses a 1-based hat (0 none, 1=N .. 8=NW), a different indexing from the
/// DS4 low-nibble hat. C `hatbits[9]` table.
#[inline]
fn xbox_hat_bits(hat: u8) -> u32 {
    match hat {
        1 => 1 << BTN_DPAD_UP,
        2 => (1 << BTN_DPAD_UP) | (1 << BTN_DPAD_RIGHT),
        3 => 1 << BTN_DPAD_RIGHT,
        4 => (1 << BTN_DPAD_DOWN) | (1 << BTN_DPAD_RIGHT),
        5 => 1 << BTN_DPAD_DOWN,
        6 => (1 << BTN_DPAD_DOWN) | (1 << BTN_DPAD_LEFT),
        7 => 1 << BTN_DPAD_LEFT,
        8 => (1 << BTN_DPAD_UP) | (1 << BTN_DPAD_LEFT),
        _ => 0,
    }
}

/// DS4 simple (0x01) or full (0x11) report. Port of `publish_ds4`.
#[must_use]
pub fn parse_ds4(r: &[u8]) -> Option<PadState> {
    if r.len() < 4 {
        return None;
    }
    let (boff, full) = match r[1] {
        0x01 => (2usize, false),
        0x11 => (4usize, true),
        _ => return None,
    };
    // C: body_len = len - boff; require >= 7 (sticks + 3 button bytes).
    if r.len() < boff + 7 {
        return None;
    }
    let body = &r[boff..];
    let body_len = body.len();

    let mut st = PadState::new(PadType::Ds4);
    st.full_report = full;

    st.axis[0] = stick(body[0]);
    st.axis[1] = stick(body[1]);
    st.axis[2] = stick(body[2]);
    st.axis[3] = stick(body[3]);
    if full && body_len >= 9 {
        st.axis[4] = stick(body[7]);
        st.axis[5] = stick(body[8]);
    } else {
        st.axis[4] = if body[5] & 0x04 != 0 { 32767 } else { -32768 };
        st.axis[5] = if body[5] & 0x08 != 0 { 32767 } else { -32768 };
    }

    let (face, sh, ps) = (body[4], body[5], body[6]);
    let mut b = 0u32;
    if face & 0x10 != 0 {
        b |= 1 << BTN_SQUARE;
    }
    if face & 0x20 != 0 {
        b |= 1 << BTN_CROSS;
    }
    if face & 0x40 != 0 {
        b |= 1 << BTN_CIRCLE;
    }
    if face & 0x80 != 0 {
        b |= 1 << BTN_TRIANGLE;
    }
    if sh & 0x01 != 0 {
        b |= 1 << BTN_L1;
    }
    if sh & 0x02 != 0 {
        b |= 1 << BTN_R1;
    }
    if sh & 0x04 != 0 {
        b |= 1 << BTN_L2;
    }
    if sh & 0x08 != 0 {
        b |= 1 << BTN_R2;
    }
    if sh & 0x10 != 0 {
        b |= 1 << BTN_SHARE;
    }
    if sh & 0x20 != 0 {
        b |= 1 << BTN_OPTIONS;
    }
    if sh & 0x40 != 0 {
        b |= 1 << BTN_L3;
    }
    if sh & 0x80 != 0 {
        b |= 1 << BTN_R3;
    }
    if ps & 0x01 != 0 {
        b |= 1 << BTN_PS;
    }
    if ps & 0x02 != 0 {
        b |= 1 << BTN_TOUCHPAD;
    }
    b |= hat_bits(face & 0x0f);
    st.buttons = b;

    // Full-report extras.
    if full && body_len >= 24 {
        let s16 = |lo: usize, hi: usize| i16::from_le_bytes([body[lo], body[hi]]);
        let mut sensors = Sensors {
            gyro: [s16(12, 13), s16(14, 15), s16(16, 17)],
            accel: [s16(18, 19), s16(20, 21), s16(22, 23)],
            ..Sensors::default()
        };
        if body_len >= 33 + 9 {
            let pkt = &body[33..];
            for (fgr, finger) in sensors.touch.iter_mut().enumerate() {
                let ft = &pkt[1 + fgr * 4..];
                finger.down = ft[0] & 0x80 == 0; // bit7 clear = finger down
                finger.id = ft[0] & 0x7f;
                finger.x = u16::from(ft[1]) | (u16::from(ft[2] & 0x0f) << 8);
                finger.y = u16::from(ft[2] >> 4) | (u16::from(ft[3]) << 4);
            }
        }
        st.sensors = Some(sensors);
    }
    if full && body_len >= 30 {
        let bat = body[29];
        st.battery = Some(Battery {
            level: bat & 0x0f,
            charging: bat & 0x10 != 0,
        });
    }

    Some(st)
}

/// DS3 / Sixaxis report (0x01, USB-style 49-byte layout over BT). Port of
/// `publish_ds3`. `d = r+1` aligns to the documented USB report bytes.
#[must_use]
pub fn parse_ds3(r: &[u8]) -> Option<PadState> {
    if r.len() < 11 {
        return None;
    }
    let d = &r[1..];

    let mut st = PadState::new(PadType::Ds3);
    st.axis[0] = stick(d[6]);
    st.axis[1] = stick(d[7]);
    st.axis[2] = stick(d[8]);
    st.axis[3] = stick(d[9]);
    if r.len() >= 21 {
        st.axis[4] = stick(d[18]);
        st.axis[5] = stick(d[19]);
    } else {
        st.axis[4] = if d[3] & 0x01 != 0 { 32767 } else { -32768 };
        st.axis[5] = if d[3] & 0x02 != 0 { 32767 } else { -32768 };
    }

    let (b1, b2, ps) = (d[2], d[3], d[4]);
    let mut b = 0u32;
    if b2 & 0x80 != 0 {
        b |= 1 << BTN_SQUARE;
    }
    if b2 & 0x40 != 0 {
        b |= 1 << BTN_CROSS;
    }
    if b2 & 0x20 != 0 {
        b |= 1 << BTN_CIRCLE;
    }
    if b2 & 0x10 != 0 {
        b |= 1 << BTN_TRIANGLE;
    }
    if b2 & 0x04 != 0 {
        b |= 1 << BTN_L1;
    }
    if b2 & 0x08 != 0 {
        b |= 1 << BTN_R1;
    }
    if b2 & 0x01 != 0 {
        b |= 1 << BTN_L2;
    }
    if b2 & 0x02 != 0 {
        b |= 1 << BTN_R2;
    }
    if b1 & 0x01 != 0 {
        b |= 1 << BTN_SHARE;
    } // Select
    if b1 & 0x08 != 0 {
        b |= 1 << BTN_OPTIONS;
    } // Start
    if b1 & 0x02 != 0 {
        b |= 1 << BTN_L3;
    }
    if b1 & 0x04 != 0 {
        b |= 1 << BTN_R3;
    }
    if ps & 0x01 != 0 {
        b |= 1 << BTN_PS;
    }
    if b1 & 0x10 != 0 {
        b |= 1 << BTN_DPAD_UP;
    }
    if b1 & 0x40 != 0 {
        b |= 1 << BTN_DPAD_DOWN;
    }
    if b1 & 0x80 != 0 {
        b |= 1 << BTN_DPAD_LEFT;
    }
    if b1 & 0x20 != 0 {
        b |= 1 << BTN_DPAD_RIGHT;
    }
    st.buttons = b;

    Some(st)
}

/// Button bytes of the Xbox report, as (mask in the byte, output bit). The
/// comments name the pad's own button for each entry.
const XBOX_NEWFW_M1: [(u8, u32); 6] = [
    (0x01, BTN_CROSS),    // A
    (0x02, BTN_CIRCLE),   // B
    (0x08, BTN_SQUARE),   // X
    (0x10, BTN_TRIANGLE), // Y
    (0x40, BTN_L1),       // LB
    (0x80, BTN_R1),       // RB
];
const XBOX_NEWFW_M2: [(u8, u32); 5] = [
    (0x04, BTN_SHARE),   // View
    (0x08, BTN_OPTIONS), // Menu
    (0x10, BTN_PS),      // Xbox
    (0x20, BTN_L3),      // LS
    (0x40, BTN_R3),      // RS
];
const XBOX_OLDFW_M1: [(u8, u32); 8] = [
    (0x01, BTN_CROSS),    // A
    (0x02, BTN_CIRCLE),   // B
    (0x04, BTN_SQUARE),   // X
    (0x08, BTN_TRIANGLE), // Y
    (0x10, BTN_L1),       // LB
    (0x20, BTN_R1),       // RB
    (0x40, BTN_SHARE),    // View
    (0x80, BTN_OPTIONS),  // Menu
];
const XBOX_OLDFW_M2: [(u8, u32); 2] = [
    (0x01, BTN_L3), // LS
    (0x02, BTN_R3), // RS
];

/// The output bits whose mask is set in `byte`.
fn mapped_bits(byte: u8, table: &[(u8, u32)]) -> u32 {
    table
        .iter()
        .filter(|(mask, _)| byte & mask != 0)
        .fold(0, |acc, (_, bit)| acc | (1 << bit))
}

/// Xbox One S / Series report (0x01). Port of `publish_xbox`'s main path.
/// `guide_held` carries the old-fw guide state the state machine tracks from
/// report 0x02 (see [`parse_xbox_guide`]); it is ignored on new-fw pads, which
/// carry the Xbox button inline.
#[must_use]
pub fn parse_xbox(r: &[u8], layout: XboxLayout, guide_held: bool) -> Option<PadState> {
    if r.get(1) != Some(&0x01) || r.len() < 16 {
        return None;
    }
    let b = &r[2..];
    let plen = r.len() - 2;
    let newfw = layout.is_newfw();

    let mut st = PadState::new(PadType::Xbox);
    let axis16 = |lo: usize, hi: usize| i16::from_le_bytes([b[lo], b[hi] ^ 0x80]);
    st.axis[0] = axis16(0, 1);
    st.axis[1] = axis16(2, 3);
    st.axis[2] = axis16(4, 5);
    st.axis[3] = axis16(6, 7);
    let lt = u16::from(b[8]) | (u16::from(b[9]) << 8); // 0..1023 on real hardware
    let rt = u16::from(b[10]) | (u16::from(b[11]) << 8);
    // Use a wide intermediate to scale the trigger without overflow.
    st.axis[4] = low_i16(i64::from(lt) * 65535 / 1023 - 32768);
    st.axis[5] = low_i16(i64::from(rt) * 65535 / 1023 - 32768);

    let mut btn = 0u32;
    if b[12] <= 8 {
        btn |= xbox_hat_bits(b[12]);
    }
    let m1 = b[13];
    let m2 = if plen >= 15 { b[14] } else { 0 };
    if newfw {
        btn |= mapped_bits(m1, &XBOX_NEWFW_M1) | mapped_bits(m2, &XBOX_NEWFW_M2);
        if plen >= 16 && b[15] & 0x01 != 0 {
            btn |= 1 << BTN_TOUCHPAD; // Share
        }
    } else {
        btn |= mapped_bits(m1, &XBOX_OLDFW_M1) | mapped_bits(m2, &XBOX_OLDFW_M2);
        if guide_held {
            btn |= 1 << BTN_PS; // guide from report 0x02
        }
    }
    if lt > 512 {
        btn |= 1 << BTN_L2;
    }
    if rt > 512 {
        btn |= 1 << BTN_R2;
    }
    st.buttons = btn;

    Some(st)
}

/// Classify a controller from its first input report's id + length. Port of
/// `classify_report`'s decision (the side effects - connect, LED, DID query -
/// belong to the state machine). Order is significant: the DS3 `len >= 40`
/// test precedes the DS4 `len <= 13` and Xbox `len <= 20` tests.
///
/// `r` is the full report including the `0xa1` header. `None` = unclassified
/// Unsupported reports do not select a controller type.
#[must_use]
pub fn classify(r: &[u8]) -> Option<PadType> {
    let id = *r.get(1)?;
    let len = r.len();
    if id == 0x11 {
        Some(PadType::Ds4)
    } else if id == 0x01 && len >= 40 {
        Some(PadType::Ds3)
    } else if id == 0x01 && len <= 13 {
        Some(PadType::Ds4)
    } else if (id == 0x01 || id == 0x02) && len <= 20 {
        Some(PadType::Xbox)
    } else {
        None
    }
}

/// Dispatch a report to the parser for an already-classified pad type. Port of
/// `publish_report`'s switch. `layout`/`guide_held` are the state machine's
/// per-pad Xbox knobs (ignored for DS4/DS3).
#[must_use]
pub fn parse_for_type(
    r: &[u8],
    pad_type: PadType,
    layout: XboxLayout,
    guide_held: bool,
) -> Option<PadState> {
    match pad_type {
        PadType::Ds4 => parse_ds4(r),
        PadType::Ds3 => parse_ds3(r),
        PadType::Xbox => parse_xbox(r, layout, guide_held),
    }
}

/// Old-fw Xbox guide-button report (id 0x02). Returns the guide-held flag the
/// state machine keeps between input reports. C: `p->xbox_guide = (len>=3 &&
/// (r[2]&0x01))`.
#[must_use]
pub fn parse_xbox_guide(r: &[u8]) -> Option<bool> {
    if r.get(1) != Some(&0x02) {
        return None;
    }
    Some(r.len() >= 3 && r[2] & 0x01 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- helpers -----------------------------------------------------------
    fn ds4_simple(lx: u8, ly: u8, rx: u8, ry: u8, face: u8, sh: u8, ps: u8) -> [u8; 9] {
        [0xa1, 0x01, lx, ly, rx, ry, face, sh, ps]
    }

    // ---- DS4 simple --------------------------------------------------------
    #[test]
    fn ds4_neutral() {
        // sticks centered, dpad neutral (low nibble 8), nothing pressed
        let st = parse_ds4(&ds4_simple(0x80, 0x80, 0x80, 0x80, 0x08, 0x00, 0x00)).unwrap();
        assert_eq!(st.axis, [128, 128, 128, 128, -32768, -32768]);
        assert_eq!(st.buttons, 0);
        assert!(!st.full_report);
        assert!(st.battery.is_none());
    }

    #[test]
    fn ds4_stick_extremes() {
        let st = parse_ds4(&ds4_simple(0x00, 0xff, 0x00, 0xff, 0x08, 0x00, 0x00)).unwrap();
        assert_eq!(st.axis[0], -32768);
        assert_eq!(st.axis[1], 32767);
        assert_eq!(st.axis[2], -32768);
        assert_eq!(st.axis[3], 32767);
    }

    #[test]
    fn ds4_face_buttons() {
        // \u{25a1} = face bit4, keep dpad neutral (0x08)
        let sq = parse_ds4(&ds4_simple(0x80, 0x80, 0x80, 0x80, 0x18, 0, 0)).unwrap();
        assert_eq!(sq.buttons, 1 << BTN_SQUARE);
        // \u{2715}\u{25cb}\u{25b3} together = bits 5,6,7 -> face 0xE0 | 0x08 neutral
        let three = parse_ds4(&ds4_simple(0x80, 0x80, 0x80, 0x80, 0xE8, 0, 0)).unwrap();
        assert_eq!(
            three.buttons,
            (1 << BTN_CROSS) | (1 << BTN_CIRCLE) | (1 << BTN_TRIANGLE)
        );
    }

    #[test]
    fn ds4_dpad_directions() {
        // low nibble is the hat; keep high nibble 0 (no face)
        let up = parse_ds4(&ds4_simple(0x80, 0x80, 0x80, 0x80, 0x00, 0, 0)).unwrap();
        assert_eq!(up.buttons, 1 << BTN_DPAD_UP);
        let se = parse_ds4(&ds4_simple(0x80, 0x80, 0x80, 0x80, 0x03, 0, 0)).unwrap();
        assert_eq!(se.buttons, (1 << BTN_DPAD_DOWN) | (1 << BTN_DPAD_RIGHT));
        let west = parse_ds4(&ds4_simple(0x80, 0x80, 0x80, 0x80, 0x06, 0, 0)).unwrap();
        assert_eq!(west.buttons, 1 << BTN_DPAD_LEFT);
    }

    #[test]
    fn ds4_shoulders_and_digital_triggers() {
        // sh: L1(0x01) R2(0x08) -> both L1 button and the digital R2 axis high
        let st = parse_ds4(&ds4_simple(0x80, 0x80, 0x80, 0x80, 0x08, 0x09, 0)).unwrap();
        assert!(st.button(BTN_L1));
        assert!(st.button(BTN_R2));
        assert_eq!(st.axis[4], -32768); // L2 digital not set
        assert_eq!(st.axis[5], 32767); // R2 digital set
    }

    #[test]
    fn ds4_ps_and_touchpad() {
        let st = parse_ds4(&ds4_simple(0x80, 0x80, 0x80, 0x80, 0x08, 0, 0x03)).unwrap();
        assert!(st.button(BTN_PS));
        assert!(st.button(BTN_TOUCHPAD));
    }

    #[test]
    fn ds4_too_short() {
        assert!(parse_ds4(&[0xa1, 0x01, 0x80]).is_none());
        assert!(parse_ds4(&[0xa1, 0x01, 0x80, 0x80, 0x80, 0x80, 0x08, 0x00]).is_none());
        // 8 < 9
    }

    #[test]
    fn ds4_wrong_id() {
        assert!(parse_ds4(&[0xa1, 0x05, 0, 0, 0, 0, 0, 0, 0]).is_none());
    }

    // ---- DS4 full (0x11) ---------------------------------------------------
    #[test]
    fn ds4_full_report_analog_battery_sensors() {
        // r = a1 11 C0 00 <body...>; body starts at index 4.
        let mut r = vec![0u8; 4 + 46];
        r[0] = 0xa1;
        r[1] = 0x11;
        r[2] = 0xc0;
        r[3] = 0x00;
        let body = &mut r[4..];
        body[0] = 0x80; // LX center
        body[1] = 0x80;
        body[2] = 0x80;
        body[3] = 0x80;
        body[4] = 0x08; // dpad neutral, no face
        body[5] = 0x00;
        body[6] = 0x00;
        body[7] = 0xff; // L2 analog full
        body[8] = 0x00; // R2 analog zero
                        // gyro[0] = body[12..14] = 0x0100 LE = 256
        body[12] = 0x00;
        body[13] = 0x01;
        // accel[2] = body[22..24] = 0xffff LE = -1
        body[22] = 0xff;
        body[23] = 0xff;
        // battery body[29] = level 5, charging
        body[29] = 0x15;
        // touch: pkt=body+33; finger0 ft=pkt+1=body[34..38]
        body[34] = 0x07; // id 7, bit7 clear => down
        body[35] = 0x21; // x low
        body[36] = 0x43; // x = 0x21 | (0x3<<8)=0x321; y low nibble = 0x4
        body[37] = 0x65; // y = 0x4 | (0x65<<4) = 0x654
        let st = parse_ds4(&r).unwrap();
        assert!(st.full_report);
        assert_eq!(st.axis[4], 32767); // L2 analog full
        assert_eq!(st.axis[5], -32768); // R2 analog zero
        let bat = st.battery.unwrap();
        assert_eq!(bat.level, 5);
        assert!(bat.charging);
        let s = st.sensors.unwrap();
        assert_eq!(s.gyro[0], 256);
        assert_eq!(s.accel[2], -1);
        assert!(s.touch[0].down);
        assert_eq!(s.touch[0].id, 7);
        assert_eq!(s.touch[0].x, 0x321);
        assert_eq!(s.touch[0].y, 0x654);
    }

    // ---- DS3 ---------------------------------------------------------------
    #[test]
    fn ds3_neutral_and_buttons() {
        // r = a1 01 <d...> with d=r+1; need len>=11. Build a 21-byte report so
        // pressure path (len>=21) is exercised too.
        let mut r = vec![0u8; 21];
        r[0] = 0xa1;
        r[1] = 0x01; // d[0] = report id
        let d = &mut r[1..];
        d[6] = 0x80; // LX
        d[7] = 0x80;
        d[8] = 0x80;
        d[9] = 0x80;
        d[18] = 0xff; // L2 pressure full
        d[19] = 0x00; // R2 pressure zero
        d[3] = 0x80; // Square (bit7)
        d[2] = 0x10; // D-pad up (bit4)
        let st = parse_ds3(&r).unwrap();
        assert_eq!(st.pad_type, PadType::Ds3);
        assert_eq!(st.axis, [128, 128, 128, 128, 32767, -32768]);
        assert!(st.button(BTN_SQUARE));
        assert!(st.button(BTN_DPAD_UP));
    }

    #[test]
    fn ds3_digital_triggers_when_short() {
        // len 11..20 -> no pressure bytes, L2/R2 from d[3] bits 0/1
        let mut r = vec![0u8; 11];
        r[0] = 0xa1;
        r[1] = 0x01;
        r[1 + 3] = 0x03; // d[3] bits0,1 = L2,R2 digital
        let st = parse_ds3(&r).unwrap();
        assert_eq!(st.axis[4], 32767);
        assert_eq!(st.axis[5], 32767);
        assert!(st.button(BTN_L2));
        assert!(st.button(BTN_R2));
    }

    #[test]
    fn ds3_too_short() {
        assert!(parse_ds3(&[0xa1, 0x01, 0, 0, 0, 0, 0, 0, 0, 0]).is_none());
    }

    // ---- Xbox --------------------------------------------------------------
    // Fixture arguments follow the physical report fields in wire order.
    #[allow(clippy::too_many_arguments)]
    fn xbox_report(
        lx: u16,
        ly: u16,
        rx: u16,
        ry: u16,
        lt: u16,
        rt: u16,
        hat: u8,
        m1: u8,
        m2: u8,
    ) -> Vec<u8> {
        let mut r = vec![0u8; 17]; // header + 15-byte payload (b[0..14])
        r[0] = 0xa1;
        r[1] = 0x01;
        let b = &mut r[2..];
        b[0..2].copy_from_slice(&lx.to_le_bytes());
        b[2..4].copy_from_slice(&ly.to_le_bytes());
        b[4..6].copy_from_slice(&rx.to_le_bytes());
        b[6..8].copy_from_slice(&ry.to_le_bytes());
        b[8..10].copy_from_slice(&lt.to_le_bytes());
        b[10..12].copy_from_slice(&rt.to_le_bytes());
        b[12] = hat;
        b[13] = m1;
        b[14] = m2;
        r
    }

    #[test]
    fn xbox_newfw_sticks_and_buttons() {
        // center sticks 0x8000, no triggers, hat none, A pressed (m1 bit0)
        let r = xbox_report(0x8000, 0x8000, 0x8000, 0x8000, 0, 0, 0, 0x01, 0x00);
        let st = parse_xbox(&r, XboxLayout::New, false).unwrap();
        assert_eq!(st.axis[0], 0);
        assert_eq!(st.axis[1], 0);
        assert_eq!(st.axis[4], -32768); // no trigger
        assert!(st.button(BTN_CROSS)); // A -> CROSS
        assert!(!st.button(BTN_SQUARE));
    }

    #[test]
    fn xbox_newfw_xbox_button_and_menu() {
        let r = xbox_report(0x8000, 0x8000, 0x8000, 0x8000, 0, 0, 0, 0, 0x10 | 0x08);
        let st = parse_xbox(&r, XboxLayout::Auto, false).unwrap();
        assert!(st.button(BTN_PS)); // Xbox (m2 bit4)
        assert!(st.button(BTN_OPTIONS)); // Menu (m2 bit3)
    }

    #[test]
    fn xbox_triggers_digital_bits() {
        // lt full (1023) -> analog max + digital L2 bit (>512)
        let r = xbox_report(0x8000, 0x8000, 0x8000, 0x8000, 1023, 0, 0, 0, 0);
        let st = parse_xbox(&r, XboxLayout::New, false).unwrap();
        assert_eq!(st.axis[4], 32767);
        assert!(st.button(BTN_L2));
        assert!(!st.button(BTN_R2));
    }

    #[test]
    fn xbox_oldfw_x_button_and_guide() {
        // old layout: X = m1 bit2 (0x04); guide comes from guide_held
        let r = xbox_report(0x8000, 0x8000, 0x8000, 0x8000, 0, 0, 0, 0x04, 0);
        let st = parse_xbox(&r, XboxLayout::Old, true).unwrap();
        assert!(st.button(BTN_SQUARE)); // X -> SQUARE
        assert!(st.button(BTN_PS)); // guide_held
    }

    #[test]
    fn xbox_hat_east() {
        let r = xbox_report(0x8000, 0x8000, 0x8000, 0x8000, 0, 0, 3, 0, 0);
        let st = parse_xbox(&r, XboxLayout::New, false).unwrap();
        assert_eq!(st.buttons & (0xf << BTN_DPAD_UP), 1 << BTN_DPAD_RIGHT);
    }

    #[test]
    fn xbox_guide_report() {
        assert_eq!(parse_xbox_guide(&[0xa1, 0x02, 0x01]), Some(true));
        assert_eq!(parse_xbox_guide(&[0xa1, 0x02, 0x00]), Some(false));
        assert_eq!(parse_xbox_guide(&[0xa1, 0x02]), Some(false)); // len<3
        assert_eq!(parse_xbox_guide(&[0xa1, 0x01, 0x01]), None); // wrong id
    }

    #[test]
    fn xbox_too_short() {
        assert!(parse_xbox(&[0xa1, 0x01, 0, 0, 0, 0], XboxLayout::New, false).is_none());
    }

    // ---- classification ----------------------------------------------------
    fn rep(id: u8, len: usize) -> Vec<u8> {
        let mut r = vec![0u8; len.max(2)];
        r[0] = 0xa1;
        r[1] = id;
        r.truncate(len);
        r
    }

    #[test]
    fn classify_boundaries() {
        // 0x11 is always DS4 regardless of length
        assert_eq!(classify(&rep(0x11, 5)), Some(PadType::Ds4));
        assert_eq!(classify(&rep(0x11, 78)), Some(PadType::Ds4));
        // 0x01: >=40 DS3, <=13 DS4, 14..=20 Xbox, 21..=39 unclassified
        assert_eq!(classify(&rep(0x01, 40)), Some(PadType::Ds3));
        assert_eq!(classify(&rep(0x01, 49)), Some(PadType::Ds3));
        assert_eq!(classify(&rep(0x01, 39)), None); // gap between Xbox and DS3
        assert_eq!(classify(&rep(0x01, 21)), None); // just past Xbox
        assert_eq!(classify(&rep(0x01, 20)), Some(PadType::Xbox));
        assert_eq!(classify(&rep(0x01, 14)), Some(PadType::Xbox));
        assert_eq!(classify(&rep(0x01, 13)), Some(PadType::Ds4));
        assert_eq!(classify(&rep(0x01, 10)), Some(PadType::Ds4));
        // 0x02 only ever classifies as Xbox, and only when <=20
        assert_eq!(classify(&rep(0x02, 20)), Some(PadType::Xbox));
        assert_eq!(classify(&rep(0x02, 21)), None);
        assert_eq!(classify(&rep(0x02, 3)), Some(PadType::Xbox));
        // unknown ids
        assert_eq!(classify(&rep(0x05, 12)), None);
        assert_eq!(classify(&[0xa1]), None); // no id byte
    }

    #[test]
    fn parse_for_type_dispatch() {
        // a 13-byte 0x01 report classifies DS4 and parses as DS4
        let mut r = ds4_simple(0x80, 0x80, 0x80, 0x80, 0x08, 0, 0).to_vec();
        r.resize(13, 0);
        let t = classify(&r).unwrap();
        assert_eq!(t, PadType::Ds4);
        let st = parse_for_type(&r, t, XboxLayout::Auto, false).unwrap();
        assert_eq!(st.pad_type, PadType::Ds4);
    }

    // ---- property: parsers never panic on arbitrary input ------------------
    #[test]
    fn parsers_never_panic_on_short_or_garbage() {
        // exhaustive over small lengths and a few id/byte patterns
        for id in [0x00u8, 0x01, 0x02, 0x11, 0xff] {
            for len in 0..64usize {
                let mut r = vec![0xa1u8; len];
                if len > 1 {
                    r[1] = id;
                }
                // fill with a walking pattern
                for (i, x) in r.iter_mut().enumerate() {
                    if i >= 2 {
                        *x = i.to_le_bytes()[0].wrapping_mul(37);
                    }
                }
                let _ = parse_ds4(&r);
                let _ = parse_ds3(&r);
                let _ = parse_xbox(&r, XboxLayout::New, false);
                let _ = parse_xbox(&r, XboxLayout::Old, true);
                let _ = parse_xbox_guide(&r);
            }
        }
    }
}
