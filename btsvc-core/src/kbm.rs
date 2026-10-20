// SPDX-License-Identifier: MIT OR Apache-2.0
//! Keyboard and mouse report processing with press and release detection.
//!
//! The shell drives `BTstack`'s descriptor HID parser to turn a raw
//! report into a list of [`HidField`] `(usage_page, usage, value)` tuples; this
//! module turns those into keyboard/mouse EVENTS (press/release edges, relative
//! motion, wheel), which the shell pushes into the `/hidkbm-*` shm ring. Keeping
//! the edge detection here makes it `cargo test`-able without a device.

/// One parsed HID field from a report (`BTstack` `btstack_hid_parser_get_field`).
#[derive(Debug, Clone, Copy)]
pub struct HidField {
    pub usage_page: u16,
    pub usage: u16,
    pub value: i32,
}

/// A keyboard/mouse event - maps 1:1 onto `hidinput_ev_t` kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbmEvent {
    /// Keyboard key edge. `code` = HID keyboard usage (== SDL scancode).
    Key { code: u8, down: bool },
    /// Mouse button edge. `code` = 1..5 (L,R,M,X1,X2).
    MButton { code: u8, down: bool },
    /// Relative mouse motion.
    MMotion { dx: i16, dy: i16 },
    /// Wheel: `dy` vertical detents, `dx` horizontal (AC Pan).
    MWheel { dx: i16, dy: i16 },
}

/// Per-connection kbm state (previous report, for edge detection).
#[derive(Clone, Copy, Default)]
pub struct KbmConn {
    mods: u8,
    keys: [u8; 16],
    nkeys: usize,
    btns: u8,
    pub is_kbd: bool,
    pub is_mouse: bool,
}

impl KbmConn {
    #[must_use]
    pub const fn new() -> Self {
        KbmConn {
            mods: 0,
            keys: [0; 16],
            nkeys: 0,
            btns: 0,
            is_kbd: false,
            is_mouse: false,
        }
    }

    /// Process one input report's parsed fields, emitting the resulting
    /// keyboard/mouse events. Port of `kbm_handle_report` (minus the
    /// Home-hold launcher hook, which is a separate concern).
    pub fn process(&mut self, fields: &[HidField], out: &mut impl FnMut(KbmEvent)) {
        let mut mods = 0u8;
        let mut keys = [0u8; 16];
        let mut nkeys = 0usize;
        let mut btns = 0u8;
        let (mut dx, mut dy, mut wh, mut pan) = (0i32, 0i32, 0i32, 0i32);
        let (mut saw_kbd, mut saw_xy, mut saw_btn) = (false, false, false);

        for f in fields {
            match f.usage_page {
                0x07 => {
                    // keyboard / keypad
                    saw_kbd = true;
                    if (0xE0..=0xE7).contains(&f.usage) {
                        if f.value != 0 {
                            mods |= 1u8 << (f.usage - 0xE0);
                        }
                    } else if f.usage > 0x03 && f.usage <= 0xE7 && f.value != 0 && nkeys < 16 {
                        keys[nkeys] = f.usage.to_le_bytes()[0]; // skip 0-3 (rollover codes)
                        nkeys += 1;
                    }
                }
                0x01 => {
                    // generic desktop
                    match f.usage {
                        0x30 => {
                            dx += f.value;
                            saw_xy = true;
                        }
                        0x31 => {
                            dy += f.value;
                            saw_xy = true;
                        }
                        0x38 => wh += f.value, // wheel
                        _ => {}
                    }
                }
                0x0C => {
                    // consumer page - AC Pan (horizontal scroll)
                    if f.usage == 0x238 {
                        pan += f.value;
                    }
                }
                0x09 => {
                    // buttons
                    saw_btn = true;
                    if (1..=5).contains(&f.usage) && f.value != 0 {
                        btns |= 1u8 << (f.usage - 1);
                    }
                }
                _ => {}
            }
        }

        // Self-correct identity from what the report actually carries.
        if saw_kbd {
            self.is_kbd = true;
        }
        if saw_xy || saw_btn {
            self.is_mouse = true;
        }

        if saw_kbd {
            self.keyboard_edges(mods, &keys[..nkeys], out);
        }

        // Mouse part: only when this report carries pointer fields (a combo
        // device's keyboard report must not release held mouse buttons).
        if saw_xy || (saw_btn && !saw_kbd) {
            for i in 0..5u8 {
                let bit = 1u8 << i;
                if (btns ^ self.btns) & bit != 0 {
                    out(KbmEvent::MButton {
                        code: i + 1,
                        down: btns & bit != 0,
                    });
                }
            }
            self.btns = btns;
            if dx != 0 || dy != 0 {
                out(KbmEvent::MMotion {
                    dx: i16::from_le_bytes([dx.to_le_bytes()[0], dx.to_le_bytes()[1]]),
                    dy: i16::from_le_bytes([dy.to_le_bytes()[0], dy.to_le_bytes()[1]]),
                });
            }
        }
        if wh != 0 || pan != 0 {
            out(KbmEvent::MWheel {
                dx: i16::from_le_bytes([pan.to_le_bytes()[0], pan.to_le_bytes()[1]]),
                dy: i16::from_le_bytes([wh.to_le_bytes()[0], wh.to_le_bytes()[1]]),
            });
        }
    }
    fn keyboard_edges(&mut self, mods: u8, keys: &[u8], out: &mut impl FnMut(KbmEvent)) {
        let nkeys = keys.len();

        for i in 0..8u8 {
            let bit = 1u8 << i;
            if (mods ^ self.mods) & bit != 0 {
                out(KbmEvent::Key {
                    code: 0xE0 + i,
                    down: mods & bit != 0,
                });
            }
        }
        self.mods = mods;
        // releases
        for i in 0..self.nkeys {
            if !keys[..nkeys].contains(&self.keys[i]) {
                out(KbmEvent::Key {
                    code: self.keys[i],
                    down: false,
                });
            }
        }
        // presses
        for &key in &keys[..nkeys] {
            if !self.keys[..self.nkeys].contains(&key) {
                out(KbmEvent::Key {
                    code: key,
                    down: true,
                });
            }
        }
        self.keys[..nkeys].copy_from_slice(&keys[..nkeys]);
        self.nkeys = nkeys;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(page: u16, usage: u16, value: i32) -> HidField {
        HidField {
            usage_page: page,
            usage,
            value,
        }
    }

    fn run(c: &mut KbmConn, fields: &[HidField]) -> Vec<KbmEvent> {
        let mut ev = Vec::new();
        c.process(fields, &mut |e| ev.push(e));
        ev
    }

    #[test]
    fn mouse_motion_and_button() {
        let mut c = KbmConn::new();
        let ev = run(
            &mut c,
            &[f(0x01, 0x30, 5), f(0x01, 0x31, -3), f(0x09, 1, 1)],
        );
        assert!(ev.contains(&KbmEvent::MButton {
            code: 1,
            down: true
        }));
        assert!(ev.contains(&KbmEvent::MMotion { dx: 5, dy: -3 }));
        assert!(c.is_mouse);
        // release the button on the next report
        let ev = run(&mut c, &[f(0x09, 1, 0)]);
        assert!(ev.contains(&KbmEvent::MButton {
            code: 1,
            down: false
        }));
    }

    #[test]
    fn mouse_wheel_and_pan() {
        let mut c = KbmConn::new();
        let ev = run(&mut c, &[f(0x01, 0x38, 1), f(0x0C, 0x238, -1)]);
        assert!(ev.contains(&KbmEvent::MWheel { dx: -1, dy: 1 }));
    }

    #[test]
    fn keyboard_press_release_and_modifiers() {
        let mut c = KbmConn::new();
        // press 'a' (0x04) with left-shift (0xE1) held
        let ev = run(&mut c, &[f(0x07, 0xE1, 1), f(0x07, 0x04, 1)]);
        assert!(ev.contains(&KbmEvent::Key {
            code: 0xE1,
            down: true
        }));
        assert!(ev.contains(&KbmEvent::Key {
            code: 0x04,
            down: true
        }));
        assert!(c.is_kbd);
        // next report: only shift still held, 'a' released
        let ev = run(&mut c, &[f(0x07, 0xE1, 1)]);
        assert!(ev.contains(&KbmEvent::Key {
            code: 0x04,
            down: false
        }));
        assert!(!ev.contains(&KbmEvent::Key {
            code: 0xE1,
            down: false
        }));
        // empty keyboard report: shift released
        let ev = run(&mut c, &[f(0x07, 0x00, 0)]);
        assert!(ev.contains(&KbmEvent::Key {
            code: 0xE1,
            down: false
        }));
    }

    #[test]
    fn combo_keyboard_report_does_not_touch_mouse_buttons() {
        let mut c = KbmConn::new();
        // hold mouse button 1
        run(&mut c, &[f(0x09, 1, 1), f(0x01, 0x30, 1)]);
        // a keyboard-only report (saw_kbd, no pointer fields) must NOT release it
        let ev = run(&mut c, &[f(0x07, 0x04, 1)]);
        assert!(!ev.iter().any(|e| matches!(e, KbmEvent::MButton { .. })));
    }

    #[test]
    fn no_fields_no_events() {
        let mut c = KbmConn::new();
        assert_eq!(run(&mut c, &[]), vec![]);
    }

    #[test]
    fn rollover_codes_ignored() {
        let mut c = KbmConn::new();
        // usage 0x01 = ErrorRollOver - must not be treated as a key
        let ev = run(&mut c, &[f(0x07, 0x01, 1)]);
        assert!(!ev.iter().any(|e| matches!(e, KbmEvent::Key { .. })));
    }
}
