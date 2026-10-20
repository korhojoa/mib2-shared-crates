// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rumble: the host->pad half of the shm contract (v4 "BTG4", v5 "BTG5" -
//! same per-slot command struct, 8 slots instead of 4) - command
//! dedupe/expiry state and the per-pad-type output-report frame builders.
//! Pure/sans-IO: the shell reads commands out of the shm, runs them through
//! [`RumbleState::poll`], and sends the returned frame over the right
//! transport (Classic interrupt channel / HOGP write).
//!
//! Frame references (verified against the sources named - NOT from memory):
//!
//! * **DS4 (BT Classic, output report 0x11)** - SDL's vendored hidapi driver
//!   `SDL/src/joystick/hidapi/SDL_hidapi_ps4.c`
//!   (`HIDAPI_DriverPS4_InternalSendJoystickEffect` + `DS4EffectsState_t`):
//!   78-byte report `{0x11, 0xC0|interval, 0x00, mask, 0x00, 0x00,
//!   rumble_right, rumble_left, led_r, led_g, led_b, led_delay_on,
//!   led_delay_off, ...zeros...}` with a CRC-32 (zlib/IEEE) over the HIDP
//!   header byte `0xA2` + report bytes [0..74), little-endian at [74..78).
//!   `rumble_left = low_frequency >> 8` (strong), `rumble_right =
//!   high_frequency >> 8` (weak); mask bit0 = rumble. `BTstack`'s
//!   `hid_host_send_report` prepends `0xa2` + the report id itself
//!   (`src/classic/hid_host.c`, `HID_HOST_W2_SEND_REPORT`), so the builder
//!   returns the 77-byte payload *after* the id and the CRC seed covers
//!   `0xa2 0x11 payload[0..73]`.
//! * **DS3 (output report 0x01)** - Linux `hid-sony.c`
//!   `struct sixaxis_output_report` (report body after the id: padding byte,
//!   `right_duration`, `right_motor_on`, `left_duration`, `left_motor_force`,
//!   padding[4], `leds_bitmap` at body[9], 4x5-byte LED timing blocks). The right
//!   (weak) motor is BINARY on/off; the left (strong) takes 0..255.
//!   SDL's `SDL_hidapi_ps3.c` `HIDAPI_DriverPS3_UpdateEffects` confirms the
//!   scaling: `right ? 1 : 0`, `left = low_frequency >> 8`.
//! * **Xbox One/Series (BT/BLE, output report 0x03)** - SDL's
//!   `SDL/src/joystick/hidapi/SDL_hidapi_xboxone.c` bluetooth branch:
//!   `{0x03, 0x0F, left_trigger, right_trigger, low, high, 0xFF, 0x00,
//!   0xEB}` - [1] enable-actuators mask, magnitudes 0..100
//!   (`low_frequency / 655`), [6] duration (deciseconds-ish ticks, 0xFF =
//!   max), [7] start delay, [8] loop count. The same report id/layout rides
//!   BT Classic and BLE/HOGP (the pad exposes one HID descriptor).

use crate::report::PadType;

/// One decoded command out of the shm `rumble[]` slot (mirrored by
/// `btsvc_shm::gamepad::Rumble`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RumbleCmd {
    pub seq: u32,
    pub low: u16,
    pub high: u16,
    pub duration_ms: u32,
}

/// Safety cap: a `duration_ms == 0` ("until next command") rumble is cut off
/// after this long, so a writer that died mid-rumble can't leave the motors
/// running.
pub const RUMBLE_CAP_MS: u32 = 5000;

/// What the shell must do this poll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RumbleAction {
    /// Send the motors these magnitudes.
    Apply { low: u16, high: u16 },
    /// Send motors-off (duration expired / explicit stop).
    Stop,
}

/// Per-slot rumble bookkeeping: dedupe by `cmd_seq`, time out by deadline.
/// Time is injected (`now_ms`), never read - host-testable.
#[derive(Clone, Copy)]
pub struct RumbleState {
    last_seq: u32,
    active: bool,
    deadline_ms: u64,
}

impl Default for RumbleState {
    fn default() -> Self {
        Self::new()
    }
}

impl RumbleState {
    #[must_use]
    pub const fn new() -> Self {
        RumbleState {
            last_seq: 0,
            active: false,
            deadline_ms: 0,
        }
    }

    /// Re-arm at pad connect: swallow whatever `cmd_seq` is already in the
    /// slot so commands issued before this connection never buzz the pad.
    pub fn reset(&mut self, current_seq: u32) {
        self.last_seq = current_seq;
        self.active = false;
        self.deadline_ms = 0;
    }

    /// Is a rumble currently applied (deadline pending)?
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Feed one poll: the freshest stable command from the shm (None if the
    /// slot read was torn) + the current time. Returns the action to take.
    pub fn poll(&mut self, now_ms: u64, cmd: Option<RumbleCmd>) -> Option<RumbleAction> {
        if let Some(c) = cmd {
            if c.seq != self.last_seq {
                self.last_seq = c.seq;
                if c.low == 0 && c.high == 0 {
                    // explicit stop - only worth sending if motors are on
                    if self.active {
                        self.active = false;
                        return Some(RumbleAction::Stop);
                    }
                    return None;
                }
                let dur = if c.duration_ms == 0 || c.duration_ms > RUMBLE_CAP_MS {
                    RUMBLE_CAP_MS
                } else {
                    c.duration_ms
                };
                self.active = true;
                self.deadline_ms = now_ms + u64::from(dur);
                return Some(RumbleAction::Apply {
                    low: c.low,
                    high: c.high,
                });
            }
        }
        if self.active && now_ms >= self.deadline_ms {
            self.active = false;
            return Some(RumbleAction::Stop);
        }
        None
    }
}

// --- CRC-32 (IEEE 802.3 / zlib), bitwise - tiny, no_std, cold path ----------

fn crc32(seed: u32, data: &[u8]) -> u32 {
    let mut crc = !seed;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

// --- frame builders ---------------------------------------------------------

/// DS4 BT output report 0x11: the 77-byte payload AFTER the report id (which
/// `BTstack`'s `hid_host_send_report` inserts). CRC over `0xa2 0x11 payload`.
#[must_use]
pub fn ds4_frame(low: u16, high: u16) -> [u8; 77] {
    let mut p = [0u8; 77];
    p[0] = 0xC0 | 0x04; // HID + CRC flags | 4 ms interval (SDL's default)
    p[2] = 0x01; // valid-flag mask: rumble only (LED left alone)
    p[5] = (high >> 8) as u8; // ucRumbleRight - weak / high-frequency motor
    p[6] = (low >> 8) as u8; // ucRumbleLeft - strong / low-frequency motor
                             // CRC over the full BT frame so far: HIDP DATA|OUTPUT header + report id
                             // + payload[0..73]. Little-endian into the last 4 bytes.
    let mut crc = crc32(0, &[0xa2, 0x11]);
    crc = crc32(crc, &p[..73]);
    p[73..77].copy_from_slice(&crc.to_le_bytes());
    p
}

/// DS3 output report 0x01: the 35-byte payload after the id - the proven
/// `ds3_set_led` frame with the rumble bytes live. Keeps the slot LED lit
/// (LED N = slot+1), because this frame REPLACES the whole output state.
#[must_use]
pub fn ds3_frame(low: u16, high: u16, slot: u8) -> [u8; 35] {
    let mut p: [u8; 35] = [
        0x00, // padding (hid-sony sixaxis_output_report: byte after the id)
        0xfe, 0x00, // right motor: duration, on/off (binary, weak)
        0xfe, 0x00, // left motor: duration, force 0..255 (strong)
        0x00, 0x00, 0x00, 0x00, // padding
        0x02, // LED bitmap - patched below
        0xff, 0x27, 0x10, 0x00, 0x32, // LED4..LED1 timing (solid on)
        0xff, 0x27, 0x10, 0x00, 0x32, //
        0xff, 0x27, 0x10, 0x00, 0x32, //
        0xff, 0x27, 0x10, 0x00, 0x32, //
        0x00, 0x00, 0x00, 0x00, 0x00, // reserved
    ];
    p[2] = u8::from(high > 0);
    p[4] = (low >> 8) as u8;
    p[9] = ds3_led_bitmap(slot);
    p
}

/// DS3 player-LED bitmap for a slot, in the DS3's own encoding (LED N sits at
/// bit N, so LED1 = 0x02 ... LED4 = 0x10).
///
/// A DS3 has only four LEDs but the PS3 supported SEVEN pads, and Sony solved
/// it by showing players 5+ as COMBINATIONS rather than wrapping - player 5 is
/// LEDs 1+4, and so on. Same table as Linux `hid-sony` (`sixaxis_leds`), which
/// runs to player 10, so it covers all 8 v5 slots with a UNIQUE pattern each.
/// Wrapping (`slot & 3`) would show two pads the same number; this does not.
#[must_use]
pub fn ds3_led_bitmap(slot: u8) -> u8 {
    // Rows are LED1..LED4 as bits 1..4, matching the wire encoding directly.
    const LEDS: [u8; 10] = [
        0b0_0010, // 1: LED1
        0b0_0100, // 2: LED2
        0b0_1000, // 3: LED3
        0b1_0000, // 4: LED4
        0b1_0010, // 5: LED1+4
        0b1_0100, // 6: LED2+4
        0b1_1000, // 7: LED3+4
        0b1_1010, // 8: LED1+3+4
        0b1_1100, // 9: LED2+3+4
        0b1_1110, // 10: all four
    ];
    LEDS[(slot as usize).min(LEDS.len() - 1)]
}

/// Xbox One S / Series output report 0x03 (BT Classic and BLE/HOGP alike):
/// the 8-byte payload after the id. Magnitudes are 0..100.
#[must_use]
pub fn xbox_frame(low: u16, high: u16) -> [u8; 8] {
    [
        0x0F,               // enable-actuators mask (both motors + triggers)
        0x00,               // left trigger magnitude (not driven)
        0x00,               // right trigger magnitude (not driven)
        (low / 655) as u8,  // low-frequency / left / strong, 0..100
        (high / 655) as u8, // high-frequency / right / weak, 0..100
        0xFF,               // duration ticks (max; we stop explicitly)
        0x00,               // start delay
        0xEB,               // loop count (SDL's constant)
    ]
}

/// The output-report id per pad family (what goes next to the payload above).
#[must_use]
pub fn report_id(t: PadType) -> u8 {
    match t {
        PadType::Ds4 => 0x11,
        PadType::Ds3 => 0x01,
        PadType::Xbox => 0x03,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // CRC-32 sanity: the standard IEEE check value.
    #[test]
    fn crc32_check_value() {
        assert_eq!(crc32(0, b"123456789"), 0xCBF4_3926);
    }

    // Golden bytes: full-strength both motors. CRC verified against python3
    // zlib.crc32(b"\xa2\x11" + payload[:73]) = 0x924d65dc.
    #[test]
    fn ds4_frame_golden_full() {
        let p = ds4_frame(0xffff, 0xffff);
        assert_eq!(p.len(), 77);
        assert_eq!(p[0], 0xC4); // 0xC0 | 4ms interval
        assert_eq!(p[1], 0x00);
        assert_eq!(p[2], 0x01); // rumble-only mask
        assert_eq!(p[5], 0xff); // weak (high)
        assert_eq!(p[6], 0xff); // strong (low)
        assert!(p[7..73].iter().all(|&b| b == 0)); // LED area untouched
        assert_eq!(&p[73..77], &0x924d_65dcu32.to_le_bytes());
    }

    // Golden bytes: asymmetric magnitudes land in the right motor bytes.
    // CRC verified against python3 zlib: 0x6fa571b9.
    #[test]
    fn ds4_frame_golden_asymmetric() {
        let p = ds4_frame(0x8000, 0x1200);
        assert_eq!(p[5], 0x12); // high >> 8 -> weak motor byte
        assert_eq!(p[6], 0x80); // low >> 8 -> strong motor byte
        assert_eq!(&p[73..77], &0x6fa5_71b9u32.to_le_bytes());
    }

    // Stop frame still carries the rumble-valid mask (that's what stops it).
    #[test]
    fn ds4_frame_stop() {
        let p = ds4_frame(0, 0);
        assert_eq!(p[2], 0x01);
        assert_eq!(p[5], 0);
        assert_eq!(p[6], 0);
    }

    #[test]
    fn ds3_frame_golden() {
        let p = ds3_frame(0xff00, 0x0001, 0);
        assert_eq!(p.len(), 35);
        assert_eq!(p[0], 0x00); // hid-sony leading padding byte
        assert_eq!(p[1], 0xfe); // right duration
        assert_eq!(p[2], 0x01); // weak motor: binary ON (high > 0)
        assert_eq!(p[3], 0xfe); // left duration
        assert_eq!(p[4], 0xff); // strong motor force = low >> 8
        assert_eq!(p[9], 0x02); // LED 1 (slot 0) stays lit
        assert_eq!(&p[10..15], &[0xff, 0x27, 0x10, 0x00, 0x32]); // solid on
        assert!(p[30..].iter().all(|&b| b == 0));
    }

    /// Sony's own answer to "4 LEDs, more than 4 players": combinations, not
    /// wrapping. Every one of the 8 v5 slots must be distinguishable.
    #[test]
    fn ds3_led_patterns_are_unique_across_all_slots() {
        let mut seen = [0u8; 8];
        for slot in 0..8u8 {
            let b = ds3_led_bitmap(slot);
            assert_eq!(b & !0b1_1110, 0, "slot {slot}: bits outside LED1..LED4");
            assert_ne!(b, 0, "slot {slot}: no LED lit");
            seen[slot as usize] = b;
        }
        for i in 0..8 {
            for j in (i + 1)..8 {
                assert_ne!(seen[i], seen[j], "slots {i} and {j} show the same pattern");
            }
        }
        // Players 1..4 stay single LEDs (unchanged behaviour for the common case)
        assert_eq!(seen[0], 0x02);
        assert_eq!(seen[3], 0x10);
        // Player 5 = LED1+LED4, the PS3's own encoding
        assert_eq!(seen[4], 0x12);
        // Out-of-range slots clamp instead of panicking
        assert_eq!(ds3_led_bitmap(200), 0b1_1110);
    }

    #[test]
    fn ds3_frame_off_keeps_led() {
        let p = ds3_frame(0, 0, 2);
        assert_eq!(p[2], 0);
        assert_eq!(p[4], 0);
        assert_eq!(p[9], 0x08); // LED 3 (slot 2)
                                // identical to the daemon's proven LED frame except the durations
        let mut led_only = p;
        led_only[1] = 0x00;
        led_only[3] = 0x00;
        let daemon_led: [u8; 35] = [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0xff, 0x27, 0x10, 0x00,
            0x32, 0xff, 0x27, 0x10, 0x00, 0x32, 0xff, 0x27, 0x10, 0x00, 0x32, 0xff, 0x27, 0x10,
            0x00, 0x32, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(led_only, daemon_led);
    }

    #[test]
    fn xbox_frame_golden() {
        // SDL scales /655 to 0..100; full-scale must land exactly on 100.
        let p = xbox_frame(0xffff, 0x8000);
        assert_eq!(p, [0x0F, 0x00, 0x00, 100, 50, 0xFF, 0x00, 0xEB]);
        let stop = xbox_frame(0, 0);
        assert_eq!(stop, [0x0F, 0x00, 0x00, 0, 0, 0xFF, 0x00, 0xEB]);
    }

    #[test]
    fn report_ids() {
        assert_eq!(report_id(PadType::Ds4), 0x11);
        assert_eq!(report_id(PadType::Ds3), 0x01);
        assert_eq!(report_id(PadType::Xbox), 0x03);
    }

    // --- RumbleState ---------------------------------------------------------

    // Mirror the optional command input used by the state machine.
    #[allow(clippy::unnecessary_wraps)]
    fn cmd(seq: u32, low: u16, high: u16, dur: u32) -> Option<RumbleCmd> {
        Some(RumbleCmd {
            seq,
            low,
            high,
            duration_ms: dur,
        })
    }

    #[test]
    fn new_command_applies_then_dedupes() {
        let mut st = RumbleState::new();
        assert_eq!(
            st.poll(1000, cmd(1, 100, 200, 0)),
            Some(RumbleAction::Apply {
                low: 100,
                high: 200
            })
        );
        // same seq again: no resend at publish cadence
        assert_eq!(st.poll(1001, cmd(1, 100, 200, 0)), None);
        assert_eq!(st.poll(1002, cmd(1, 100, 200, 0)), None);
    }

    #[test]
    fn zero_duration_capped() {
        let mut st = RumbleState::new();
        st.poll(0, cmd(1, 500, 0, 0));
        assert!(st.is_active());
        assert_eq!(st.poll(u64::from(RUMBLE_CAP_MS) - 1, None), None);
        assert_eq!(
            st.poll(u64::from(RUMBLE_CAP_MS), None),
            Some(RumbleAction::Stop)
        );
        assert!(!st.is_active());
        // after the stop, nothing more
        assert_eq!(st.poll(u64::from(RUMBLE_CAP_MS) + 1000, None), None);
    }

    #[test]
    fn explicit_duration_expires() {
        let mut st = RumbleState::new();
        st.poll(100, cmd(1, 1, 1, 250));
        assert_eq!(st.poll(349, None), None);
        assert_eq!(st.poll(350, None), Some(RumbleAction::Stop));
    }

    #[test]
    fn oversized_duration_clamped_to_cap() {
        let mut st = RumbleState::new();
        st.poll(0, cmd(1, 1, 1, 60_000));
        assert_eq!(
            st.poll(u64::from(RUMBLE_CAP_MS), None),
            Some(RumbleAction::Stop)
        );
    }

    #[test]
    fn explicit_stop_sent_once() {
        let mut st = RumbleState::new();
        st.poll(0, cmd(1, 9, 9, 0));
        assert_eq!(st.poll(10, cmd(2, 0, 0, 0)), Some(RumbleAction::Stop));
        // repeated/further stop commands while idle: swallowed
        assert_eq!(st.poll(20, cmd(3, 0, 0, 0)), None);
    }

    #[test]
    fn refresh_extends_deadline() {
        let mut st = RumbleState::new();
        st.poll(0, cmd(1, 5, 5, 1000));
        st.poll(900, cmd(2, 5, 5, 1000)); // re-issued before expiry
        assert_eq!(st.poll(1000, None), None); // old deadline no longer fires
        assert_eq!(st.poll(1900, None), Some(RumbleAction::Stop));
    }

    #[test]
    fn reset_swallows_stale_commands() {
        let mut st = RumbleState::new();
        st.reset(7); // pad connected with cmd_seq already at 7
        assert_eq!(st.poll(0, cmd(7, 999, 999, 0)), None); // stale - ignored
        assert_eq!(
            st.poll(1, cmd(8, 3, 4, 0)),
            Some(RumbleAction::Apply { low: 3, high: 4 })
        );
    }

    #[test]
    fn torn_reads_do_not_disturb_state() {
        let mut st = RumbleState::new();
        st.poll(0, cmd(1, 5, 5, 1000));
        assert_eq!(st.poll(500, None), None); // torn read mid-rumble: nothing
        assert_eq!(st.poll(1000, None), Some(RumbleAction::Stop)); // expiry still lands
    }
}
