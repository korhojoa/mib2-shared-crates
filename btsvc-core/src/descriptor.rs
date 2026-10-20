// SPDX-License-Identifier: MIT OR Apache-2.0
//! Read HID report descriptor items with bounded input access.
//! All descriptor helpers use the same item iterator.

/// One short HID item. `value` is the little-endian data bytes (bounded by the
/// buffer); `size` is the declared data size (0,1,2,4). Long items (`0xfe`) are
/// skipped, matching the C `i += 1 + d[i]` handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    /// bType: 0 main, 1 global, 2 local, 3 reserved.
    pub itype: u8,
    /// bTag (high nibble).
    pub tag: u8,
    pub value: u32,
    pub size: u8,
}

/// Iterate the short items of a HID report descriptor.
pub fn items(descriptor: &[u8]) -> impl Iterator<Item = Item> + '_ {
    let mut offset = 0usize;
    core::iter::from_fn(move || {
        while offset < descriptor.len() {
            let prefix = descriptor[offset];
            offset += 1;
            if prefix == 0xfe {
                // Long item: C reads descriptor[offset] as the size and skips 1 + size.
                if offset >= descriptor.len() {
                    return None;
                }
                let long_size = descriptor[offset] as usize;
                offset = offset.saturating_add(1 + long_size);
                continue;
            }
            let item_size = match prefix & 3 {
                3 => 4usize,
                n => n as usize,
            };
            let mut value = 0u32;
            let mut byte_index = 0;
            while byte_index < item_size && offset + byte_index < descriptor.len() {
                value |= u32::from(descriptor[offset + byte_index]) << (8 * byte_index);
                byte_index += 1;
            }
            offset += item_size; // may overshoot; the while-guard catches it next round
            return Some(Item {
                itype: (prefix >> 2) & 3,
                tag: prefix >> 4,
                value,
                size: u8::try_from(item_size).ok()?,
            });
        }
        None
    })
}

/// Does the descriptor declare any Report ID (global item, tag 8)?
#[must_use]
pub fn has_report_id(d: &[u8]) -> bool {
    items(d).any(|it| it.itype == 1 && it.tag == 8)
}

/// The first Report ID declared (0 if none). C: `desc_first_report_id`.
#[must_use]
pub fn first_report_id(d: &[u8]) -> u8 {
    for it in items(d) {
        if it.itype == 1 && it.tag == 8 {
            return it.value.to_le_bytes()[0];
        }
    }
    0
}

/// Does the descriptor declare Generic-Desktop X (0x30) and Y (0x31)?
/// C: `desc_has_xy`.
#[must_use]
pub fn has_xy(d: &[u8]) -> bool {
    let mut page = 0u32;
    let (mut x, mut y) = (false, false);
    for it in items(d) {
        if it.itype == 1 && it.tag == 0 {
            page = it.value;
        } else if it.itype == 2 && it.tag == 0 && page == 0x01 {
            if it.value == 0x30 {
                x = true;
            } else if it.value == 0x31 {
                y = true;
            }
        }
    }
    x && y
}

/// The Report ID whose INPUT item carries Generic-Desktop X/Y - the
/// mouse-motion report. 0 if none/no ids. C: `desc_report_id_with_xy`.
#[must_use]
pub fn report_id_with_xy(d: &[u8]) -> u8 {
    let mut page = 0u32;
    let mut rid = 0u8;
    let mut best = 0u8;
    let mut usages = [0u32; 8];
    let mut nu = 0usize;
    for it in items(d) {
        match it.itype {
            1 => {
                if it.tag == 0 {
                    page = it.value;
                } else if it.tag == 8 {
                    rid = it.value.to_le_bytes()[0];
                }
            }
            2 => {
                if it.tag == 0 && nu < 8 {
                    usages[nu] = if it.size == 4 {
                        it.value
                    } else {
                        (page << 16) | it.value
                    };
                    nu += 1;
                }
            }
            0 => {
                if it.tag == 8 {
                    // Input item: does any pending usage carry X or Y?
                    for &u in &usages[..nu] {
                        let pg = (u >> 16) as u16;
                        let us = (u & 0xffff) as u16;
                        if pg == 0x01 && (us == 0x30 || us == 0x31) {
                            best = rid;
                            break;
                        }
                    }
                }
                nu = 0; // any main item clears locals
            }
            _ => {}
        }
    }
    best
}

/// Application-collection usages present: keyboard, mouse, gamepad.
/// C: `desc_app_usages`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AppUsages {
    pub kbd: bool,
    pub mouse: bool,
    pub pad: bool,
}

/// Classify a descriptor by its Collection(Application) usages. C:
/// `desc_app_usages`.
#[must_use]
pub fn app_usages(d: &[u8]) -> AppUsages {
    let mut page = 0u32;
    let mut usages = [0u32; 4];
    let mut nu = 0usize;
    let mut out = AppUsages::default();
    for it in items(d) {
        match it.itype {
            1 => {
                if it.tag == 0 {
                    page = it.value;
                }
            }
            2 => {
                if it.tag == 0 && nu < 4 {
                    usages[nu] = if it.size == 4 {
                        it.value
                    } else {
                        (page << 16) | it.value
                    };
                    nu += 1;
                }
            }
            0 => {
                // Collection(Application): tag 0xA, value 0x01.
                if it.tag == 0xA && it.value == 0x01 && nu > 0 {
                    let u = usages[nu - 1];
                    let pg = (u >> 16) as u16;
                    let us = u16::from_le_bytes([u.to_le_bytes()[0], u.to_le_bytes()[1]]);
                    if pg == 0x01 && (us == 0x06 || us == 0x07) {
                        out.kbd = true;
                    } else if pg == 0x01 && (us == 0x02 || us == 0x01) {
                        out.mouse = true;
                    } else if pg == 0x01 && (us == 0x04 || us == 0x05) {
                        out.pad = true;
                    }
                }
                nu = 0; // main item resets locals
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // A minimal but real mouse report descriptor with a Report ID.
    const MOUSE: &[u8] = &[
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x02, // Usage (Mouse)
        0xA1, 0x01, // Collection (Application)
        0x85, 0x02, //   Report ID (2)
        0x09, 0x01, //   Usage (Pointer)
        0xA1, 0x00, //   Collection (Physical)
        0x09, 0x30, //     Usage (X)
        0x09, 0x31, //     Usage (Y)
        0x81, 0x06, //     Input (Data,Var,Rel)
        0xC0, //         End Collection
        0xC0, //       End Collection
    ];

    // A keyboard descriptor, no Report ID.
    const KEYBOARD: &[u8] = &[
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x06, // Usage (Keyboard)
        0xA1, 0x01, // Collection (Application)
        0x05, 0x07, //   Usage Page (Key Codes)
        0x19, 0x00, //   Usage Minimum (0)
        0x29, 0x65, //   Usage Maximum (101)
        0x81, 0x00, //   Input (Data,Array)
        0xC0, //         End Collection
    ];

    #[test]
    fn mouse_report_id() {
        assert!(has_report_id(MOUSE));
        assert_eq!(first_report_id(MOUSE), 2);
        assert_eq!(report_id_with_xy(MOUSE), 2);
        assert!(has_xy(MOUSE));
    }

    #[test]
    fn mouse_app_usage() {
        let a = app_usages(MOUSE);
        assert!(a.mouse);
        assert!(!a.kbd);
        assert!(!a.pad);
    }

    #[test]
    fn keyboard_no_report_id() {
        assert!(!has_report_id(KEYBOARD));
        assert_eq!(first_report_id(KEYBOARD), 0);
        assert_eq!(report_id_with_xy(KEYBOARD), 0); // no X/Y
        assert!(!has_xy(KEYBOARD));
        let a = app_usages(KEYBOARD);
        assert!(a.kbd);
        assert!(!a.mouse);
    }

    #[test]
    fn gamepad_app_usage() {
        // Usage Page(GD), Usage(Gamepad=0x05), Collection(Application)
        let d = &[0x05, 0x01, 0x09, 0x05, 0xA1, 0x01, 0xC0];
        let a = app_usages(d);
        assert!(a.pad);
        assert!(!a.kbd);
        assert!(!a.mouse);
    }

    #[test]
    fn truncated_never_panics() {
        // every prefix of both descriptors, plus garbage, must not panic
        for full in [MOUSE, KEYBOARD] {
            for n in 0..=full.len() {
                let d = &full[..n];
                let _ = has_report_id(d);
                let _ = first_report_id(d);
                let _ = has_xy(d);
                let _ = report_id_with_xy(d);
                let _ = app_usages(d);
                let _ = items(d).count();
            }
        }
        // an item that claims a 4-byte value at the very end (over-read bait)
        let bait = &[0x05, 0x01, 0x27, 0xff]; // 0x27 = global, size 3->4 bytes, only 1 present
        assert_eq!(report_id_with_xy(bait), 0);
        let _ = app_usages(bait);
        // a lone long-item prefix
        let _ = items(&[0xfe]).count();
        let _ = items(&[0xfe, 0x02, 0x00]).count();
    }

    #[test]
    fn long_item_is_skipped() {
        // 0xfe, size=2, 2 data bytes, then a real Report ID item
        let d = &[0xfe, 0x02, 0xaa, 0xbb, 0x85, 0x07];
        assert_eq!(first_report_id(d), 7);
    }
}
