// SPDX-License-Identifier: MIT OR Apache-2.0
//! Auto-pairing policy based on the device class, vendor address prefix,
//! and inquiry result. Unknown devices cannot pair automatically.
//!
//! Auto-pairing is just-works (no button, no confirmation), so an unattended
//! scan must only ever bond devices that are actually PS4/Xbox controllers.
//! The gate is: gamepad Class-of-Device AND a known controller vendor OUI.
//! `pair_any`/`force_pair` only make an unknown device LISTABLE for a human to
//! pick - they never auto-pair it.

use crate::sm::BdAddr;

/// Class-of-Device: major "peripheral" (0x05) with minor joystick (1) or
/// gamepad (2). Matches DS4/Xbox (0x2508) and DS3 (0x0508).
#[inline]
#[must_use]
pub fn cod_is_gamepad(cod: u32) -> bool {
    ((cod >> 8) & 0x1f) == 0x05 && (((cod >> 2) & 0x0f) == 0x01 || ((cod >> 2) & 0x0f) == 0x02)
}

/// Class-of-Device: major "peripheral" with the keyboard/mouse bits set.
/// C: `COD_IS_KBM`.
#[inline]
#[must_use]
pub fn cod_is_kbm(cod: u32) -> bool {
    ((cod >> 8) & 0x1f) == 0x05 && (cod & 0xc0) != 0
}

/// Controller vendor OUIs (Microsoft / Foxconn-for-genuine-DS4 / Sony),
/// used to classify advertised device addresses.
pub static CONTROLLER_OUIS: [[u8; 3]; 169] = [
    [0x00, 0x03, 0xFF],
    [0x00, 0x0D, 0x3A],
    [0x00, 0x12, 0x5A],
    [0x00, 0x15, 0x5D],
    [0x00, 0x17, 0xFA],
    [0x00, 0x1D, 0xD8],
    [0x00, 0x22, 0x48],
    [0x00, 0x25, 0xAE],
    [0xC0, 0xD6, 0xD5],
    [0x00, 0x50, 0xF2],
    [0x04, 0x27, 0x28],
    [0x0C, 0x35, 0x26],
    [0x0C, 0x41, 0x3E],
    [0x0C, 0xE7, 0x25],
    [0x10, 0x2F, 0x6B],
    [0x14, 0x9A, 0x10],
    [0x14, 0xCB, 0x65],
    [0x1C, 0x1A, 0xDF],
    [0x20, 0x16, 0x42],
    [0x20, 0x62, 0x74],
    [0x20, 0xA9, 0x9B],
    [0x30, 0x59, 0xB7],
    [0x28, 0x16, 0xA8],
    [0x28, 0x18, 0x78],
    [0x28, 0xEA, 0x0B],
    [0x2C, 0x29, 0x97],
    [0x2C, 0x54, 0x91],
    [0x30, 0x0D, 0x43],
    [0x38, 0xF2, 0x3E],
    [0x38, 0x25, 0x6B],
    [0x38, 0x56, 0x3D],
    [0x3C, 0x83, 0x75],
    [0x3C, 0xFA, 0x06],
    [0x40, 0x8E, 0x2C],
    [0x44, 0x16, 0x22],
    [0x48, 0x50, 0x73],
    [0x48, 0x86, 0xE8],
    [0x4C, 0x0B, 0xBE],
    [0x4C, 0x3B, 0xDF],
    [0x50, 0x1A, 0xC5],
    [0x54, 0x4C, 0x8A],
    [0x58, 0x82, 0xA8],
    [0x5C, 0xBA, 0x37],
    [0x5C, 0xCA, 0x1A],
    [0x60, 0x45, 0xBD],
    [0x60, 0x7E, 0xDD],
    [0x68, 0x6C, 0xE6],
    [0x6C, 0x15, 0x44],
    [0x6C, 0x24, 0x83],
    [0x6C, 0x27, 0x79],
    [0x6C, 0x5D, 0x3A],
    [0x6C, 0x8F, 0xB5],
    [0x7C, 0x1E, 0x52],
    [0x70, 0xBC, 0x10],
    [0x70, 0xF8, 0xAE],
    [0x74, 0xE2, 0x8C],
    [0x7C, 0xC0, 0xAA],
    [0x7C, 0xED, 0x8D],
    [0x80, 0xC5, 0xE6],
    [0x84, 0x57, 0x33],
    [0x84, 0x63, 0xD6],
    [0x84, 0xB1, 0xE2],
    [0x90, 0x6A, 0xEB],
    [0x94, 0x9A, 0xA9],
    [0x98, 0x5F, 0xD3],
    [0x98, 0x7A, 0x14],
    [0x9C, 0x6C, 0x15],
    [0x9C, 0xAA, 0x1B],
    [0xA0, 0x4A, 0x5E],
    [0xA0, 0x85, 0xFC],
    [0xA4, 0x51, 0x6F],
    [0xA8, 0x8C, 0x3E],
    [0xCC, 0x60, 0xC8],
    [0xB4, 0xAE, 0x2B],
    [0xB4, 0xE1, 0xC4],
    [0xB8, 0x31, 0xB5],
    [0xB8, 0x4F, 0xD5],
    [0xBC, 0x83, 0x85],
    [0xC0, 0x33, 0x5E],
    [0xC4, 0x61, 0xC7],
    [0xC4, 0x9D, 0xED],
    [0xC4, 0xCB, 0x76],
    [0xD0, 0x92, 0x9E],
    [0xC8, 0x3F, 0x26],
    [0xC8, 0x96, 0x65],
    [0xCA, 0x12, 0x5C],
    [0xD4, 0x8F, 0x33],
    [0xD8, 0xE2, 0xDF],
    [0xDC, 0x98, 0x40],
    [0xE4, 0x2A, 0xAC],
    [0xB8, 0x5C, 0x5C],
    [0xAC, 0x8E, 0xBD],
    [0xCC, 0xB0, 0xB3],
    [0xE8, 0xF6, 0x73],
    [0xE4, 0x98, 0xD1],
    [0xEC, 0x83, 0x50],
    [0xE8, 0xA7, 0x2F],
    [0xEC, 0x59, 0xE7],
    [0xF0, 0x1D, 0xBC],
    [0xF0, 0x6E, 0x0B],
    [0xF4, 0x6A, 0xD7],
    [0xFC, 0x8C, 0x11],
    [0x10, 0xC7, 0x35],
    [0x74, 0x76, 0x1F],
    [0x78, 0x86, 0x2E],
    [0x48, 0x7B, 0x2F],
    [0x74, 0xC4, 0x12],
    [0x58, 0x79, 0x61],
    [0x68, 0xF7, 0xD8],
    [0xCC, 0x0D, 0xCB],
    [0xEC, 0x46, 0x84],
    [0x38, 0x33, 0xC5],
    [0x70, 0xA8, 0xA5],
    [0xCC, 0x76, 0x45],
    [0x7C, 0x6D, 0x12],
    [0x90, 0x20, 0xD7],
    [0xB8, 0xBA, 0x66],
    [0x08, 0x35, 0x7D],
    [0xE4, 0xFB, 0x1E],
    [0x40, 0x12, 0x77],
    [0xE4, 0xFA, 0xDE],
    [0x10, 0x7A, 0x2A],
    [0x74, 0xD8, 0x09],
    [0xC4, 0xC0, 0x0A],
    [0x0C, 0x1B, 0x7B],
    [0x00, 0x22, 0x68],
    [0x00, 0x04, 0x1F],
    [0x00, 0x13, 0x15],
    [0x00, 0x15, 0xC1],
    [0x00, 0x19, 0xC5],
    [0x00, 0x1D, 0x0D],
    [0x00, 0x1F, 0xA7],
    [0x00, 0x24, 0x8D],
    [0x00, 0xD9, 0xD1],
    [0x00, 0xE4, 0x21],
    [0x04, 0xF7, 0x78],
    [0x0C, 0x70, 0x43],
    [0x0C, 0xFE, 0x45],
    [0x28, 0x0D, 0xFC],
    [0x28, 0x40, 0xDD],
    [0x2C, 0x9E, 0x00],
    [0x2C, 0xCC, 0x44],
    [0x50, 0xB0, 0x3B],
    [0x54, 0xE6, 0xFD],
    [0x5C, 0x84, 0x3C],
    [0x5C, 0x96, 0x66],
    [0x68, 0x28, 0x6C],
    [0x70, 0x66, 0x2A],
    [0x70, 0x9E, 0x29],
    [0x78, 0xC8, 0x81],
    [0x84, 0xE6, 0x57],
    [0x90, 0x47, 0x48],
    [0x98, 0xFA, 0x2E],
    [0x9C, 0x37, 0xCB],
    [0xA8, 0xE3, 0xEE],
    [0xB4, 0x0A, 0xD8],
    [0xB4, 0x1F, 0x4D],
    [0xBC, 0x33, 0x29],
    [0xBC, 0x60, 0xA7],
    [0xC0, 0x15, 0x1B],
    [0xC8, 0x4A, 0xA0],
    [0xC8, 0x63, 0xF1],
    [0xD4, 0xF7, 0xD5],
    [0xE8, 0x6E, 0x3A],
    [0xEC, 0x74, 0x8C],
    [0xF4, 0x64, 0x12],
    [0xF8, 0x46, 0x1C],
    [0xF8, 0xD0, 0xAC],
    [0xFC, 0x0F, 0xE6],
];

/// True if the address's 24-bit OUI is a known controller vendor.
#[must_use]
pub fn oui_is_controller(addr: &BdAddr) -> bool {
    let head = [addr[0], addr[1], addr[2]];
    CONTROLLER_OUIS.contains(&head)
}

/// Pairing policy supplied by the caller.
#[derive(Debug, Clone, Copy)]
pub struct PairPolicy {
    pub kbm_enabled: bool,
    pub force_pair: bool,
    pub pair_any: bool,
}

impl Default for PairPolicy {
    fn default() -> Self {
        PairPolicy {
            kbm_enabled: true,
            force_pair: false,
            pair_any: false,
        }
    }
}

/// What to do with a Classic inquiry result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InquiryAction {
    /// Known controller: auto-connect (just-works pair).
    AutoConnect,
    /// Show it in the interactive discovery list for a human to pick.
    ListInteractive,
    /// Not a pairing candidate; drop it.
    Ignore,
}

/// Decide what to do with a discovered Classic device. Port of the
/// `GAP_EVENT_INQUIRY_RESULT` gate: auto-connect ONLY a gamepad-CoD device with
/// a known controller OUI; `pair_any`/`force_pair` make an unknown device
/// LISTABLE but never auto-paired (the safety that once stopped a bystander
/// SHIELD remote being grabbed).
#[must_use]
pub fn classic_inquiry_action(cod: u32, addr: &BdAddr, p: PairPolicy) -> InquiryAction {
    if cod_is_gamepad(cod) {
        if oui_is_controller(addr) {
            InquiryAction::AutoConnect
        } else if p.force_pair || p.pair_any {
            InquiryAction::ListInteractive
        } else {
            InquiryAction::Ignore
        }
    } else if p.kbm_enabled && cod_is_kbm(cod) {
        if p.force_pair || p.pair_any {
            InquiryAction::ListInteractive
        } else {
            InquiryAction::Ignore
        }
    } else {
        InquiryAction::Ignore
    }
}

// --- LE advertising filters (HOGP) ------------------------------------------
// AD structure: repeated [len][type][data...], len counts the type byte.

/// Iterate `(ad_type, data)` fields of an advertising payload.
fn ad_fields(ad: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut i = 0usize;
    core::iter::from_fn(move || {
        if i < ad.len() {
            let l = ad[i] as usize;
            if l == 0 {
                return None; // early terminator
            }
            let ty = *ad.get(i + 1)?;
            let start = i + 2;
            let end = (i + 1 + l).min(ad.len());
            let data = if start <= end {
                &ad[start..end]
            } else {
                &[][..]
            };
            i += 1 + l;
            return Some((ty, data));
        }
        None
    })
}

/// The Appearance AD field (type 0x19), if present.
#[must_use]
pub fn adv_appearance(ad: &[u8]) -> Option<u16> {
    for (ty, data) in ad_fields(ad) {
        if ty == 0x19 && data.len() >= 2 {
            return Some(u16::from(data[0]) | (u16::from(data[1]) << 8));
        }
    }
    None
}

/// Does the advertisement carry the HID service (0x1812) in a 16-bit UUID list
/// (type 0x02/0x03), or a HID-category appearance (0x03Cx)? C: `adv_is_hid`.
#[must_use]
pub fn adv_is_hid(ad: &[u8]) -> bool {
    for (ty, data) in ad_fields(ad) {
        if (ty == 0x02 || ty == 0x03) && data.len() >= 2 {
            for u in data.as_chunks::<2>().0 {
                if (u16::from(u[0]) | (u16::from(u[1]) << 8)) == 0x1812 {
                    return true;
                }
            }
        }
        if ty == 0x19 && data.len() >= 2 {
            let app = u16::from(data[0]) | (u16::from(data[1]) << 8);
            if app & 0xfff0 == 0x03c0 {
                return true;
            }
        }
    }
    false
}

/// May we auto-just-works-pair with this LE advertiser? Port of
/// `le_pair_candidate_ok`: public address only, a known controller OUI, and if
/// it names a HID appearance it must be GAMEPAD (0x03C4) - a Microsoft BLE
/// mouse (0x03C2) / keyboard (0x03C1) is rejected even with a controller OUI.
/// `atype` 0 = public. `pair_any` never bypasses this (it only widens the
/// interactive list); auto-pair = OUI-verified controllers only, ever.
#[must_use]
pub fn le_pair_candidate_ok(addr: &BdAddr, atype: u8, ad: &[u8]) -> bool {
    if atype != 0 {
        return false; // random address hides the vendor
    }
    if !oui_is_controller(addr) {
        return false;
    }
    if let Some(app) = adv_appearance(ad) {
        if app & 0xfff0 == 0x03c0 && app != 0x03c4 {
            return false;
        }
    }
    true
}

/// What to do with one LE advertiser - the unified pad/kbm gate (was split
/// between `le_pair_candidate_ok` in core and `le_kbm_candidate` in the
/// shell). Precedence: a bonded device reconnects on sight (pad before kbm);
/// a NEW pad auto-pairs only through the public-address controller-OUI gate;
/// a NEW keyboard/mouse only under an explicit pairing session (`force_pair`,
/// i.e. `BTGP_PAIR=1` - no bystander-mouse hijack, ever).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeAdvAction {
    ConnectPad,
    ConnectKbm,
    Ignore,
}

#[must_use]
// Preserve the established shell API for independent pad and keyboard bonds.
#[allow(clippy::fn_params_excessive_bools, clippy::too_many_arguments)]
pub fn le_adv_action(
    addr: &BdAddr,
    atype: u8,
    ad: &[u8],
    bonded_pad: bool,
    pad_connected: bool,
    bonded_kbm: bool,
    kbm_connected: bool,
    p: PairPolicy,
) -> LeAdvAction {
    if bonded_pad {
        return if pad_connected {
            LeAdvAction::Ignore // already up but still advertising
        } else {
            LeAdvAction::ConnectPad
        };
    }
    if bonded_kbm {
        return if kbm_connected {
            LeAdvAction::Ignore
        } else {
            LeAdvAction::ConnectKbm
        };
    }
    if adv_is_hid(ad) && le_pair_candidate_ok(addr, atype, ad) {
        return LeAdvAction::ConnectPad;
    }
    if p.force_pair && p.kbm_enabled {
        match adv_appearance(ad) {
            Some(app) if app == 0x03c1 || app == 0x03c2 => return LeAdvAction::ConnectKbm,
            _ if adv_is_hid(ad) => return LeAdvAction::ConnectKbm,
            _ => {}
        }
    }
    LeAdvAction::Ignore
}

/// Does this LE re-encryption failure status mean the STORED BOND IS STALE?
/// AUTHENTICATION_
/// FAILURE (0x05) / `PIN_OR_KEY_MISSING` (0x06) = the peer rejected (or no
/// longer holds) the LTK we re-encrypted with - retrying with that key can
/// NEVER succeed, so the recovery is delete-bond + fresh pairing, once per
/// connection. Anything else (`CONNECTION_TIMEOUT` 0x08, `REMOTE_USER_TERMINATED`
/// 0x13, ...) is the link dying before security finished - the bond may be
/// fine; keep it and let the disconnect path retry the connection.
#[must_use]
pub fn reencryption_failure_is_stale_bond(status: u8) -> bool {
    status == 0x05 || status == 0x06
}

/// Describe a failed LE pairing attempt.
#[must_use]
pub fn pairing_failure_reason(status: u8) -> &'static str {
    match status {
        0x13 => "the device dropped the link (not in pairing mode?)",
        0x08 => "no pairing response",
        _ => "pairing rejected",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COD_DS4: u32 = 0x2508;
    const COD_DS3: u32 = 0x0508;
    const COD_KEYBOARD: u32 = 0x2540; // peripheral + keyboard bit
    const COD_PHONE: u32 = 0x5a_020c;

    #[test]
    fn cod_gates() {
        assert!(cod_is_gamepad(COD_DS4));
        assert!(cod_is_gamepad(COD_DS3));
        assert!(!cod_is_gamepad(COD_KEYBOARD));
        assert!(!cod_is_gamepad(COD_PHONE));
        assert!(cod_is_kbm(COD_KEYBOARD));
        assert!(!cod_is_kbm(COD_DS4));
        assert!(!cod_is_kbm(COD_PHONE));
    }

    #[test]
    fn oui_table_count_and_membership() {
        assert_eq!(CONTROLLER_OUIS.len(), 169);
        // Sony (first Sony entry), Foxconn genuine-DS4, Microsoft (first entry)
        assert!(oui_is_controller(&[0x00, 0x04, 0x1F, 1, 2, 3]));
        assert!(oui_is_controller(&[0x00, 0x22, 0x68, 0xCA, 0xEB, 0xD4]));
        assert!(oui_is_controller(&[0x00, 0x03, 0xFF, 0, 0, 0]));
        // a random / clone radio is not a known controller OUI
        assert!(!oui_is_controller(&[0xDE, 0xAD, 0xBE, 0, 0, 0]));
        // a CSR-compatible USB adapter prefix is not a controller prefix
        assert!(!oui_is_controller(&[0x00, 0x1A, 0x7D, 0xDA, 0x71, 0x14]));
    }

    #[test]
    fn inquiry_policy_matrix() {
        let none = PairPolicy::default();
        let any = PairPolicy {
            pair_any: true,
            ..PairPolicy::default()
        };
        let no_kbm = PairPolicy {
            kbm_enabled: false,
            ..PairPolicy::default()
        };

        let sony_pad = [0x00, 0x04, 0x1F, 1, 2, 3];
        let clone_pad = [0xDE, 0xAD, 0xBE, 1, 2, 3];
        let kbd = [0x11, 0x22, 0x33, 4, 5, 6];

        // known controller: auto-connect regardless of flags (the safe case)
        assert_eq!(
            classic_inquiry_action(COD_DS4, &sony_pad, none),
            InquiryAction::AutoConnect
        );
        assert_eq!(
            classic_inquiry_action(COD_DS4, &sony_pad, any),
            InquiryAction::AutoConnect
        );

        // gamepad CoD but unknown OUI: NEVER auto-pair; only listable under flags
        assert_eq!(
            classic_inquiry_action(COD_DS4, &clone_pad, none),
            InquiryAction::Ignore
        );
        assert_eq!(
            classic_inquiry_action(COD_DS4, &clone_pad, any),
            InquiryAction::ListInteractive
        );

        // keyboard: interactive only, off by default; nothing when kbm disabled
        assert_eq!(
            classic_inquiry_action(COD_KEYBOARD, &kbd, none),
            InquiryAction::Ignore
        );
        assert_eq!(
            classic_inquiry_action(COD_KEYBOARD, &kbd, any),
            InquiryAction::ListInteractive
        );
        assert_eq!(
            classic_inquiry_action(COD_KEYBOARD, &kbd, no_kbm),
            InquiryAction::Ignore
        );

        // non-peripheral (a phone): always ignored
        assert_eq!(
            classic_inquiry_action(COD_PHONE, &sony_pad, any),
            InquiryAction::Ignore
        );
    }

    #[test]
    fn adv_hid_detection() {
        // flags(0x01) + 16-bit complete UUID list(0x03) containing 0x1812
        let hid_svc = [0x02u8, 0x01, 0x06, 0x03, 0x03, 0x12, 0x18];
        assert!(adv_is_hid(&hid_svc));
        // appearance gamepad 0x03C4
        let app_pad = [0x03u8, 0x19, 0xc4, 0x03];
        assert!(adv_is_hid(&app_pad));
        assert_eq!(adv_appearance(&app_pad), Some(0x03c4));
        // a non-HID advert (just flags + a name)
        let plain = [0x02u8, 0x01, 0x06, 0x03, 0x09, b'h', b'i'];
        assert!(!adv_is_hid(&plain));
        // truncated / garbage never panics
        for n in 0..hid_svc.len() {
            let _ = adv_is_hid(&hid_svc[..n]);
            let _ = adv_appearance(&hid_svc[..n]);
        }
    }

    #[test]
    fn le_candidate_gate() {
        let ms = [0x00u8, 0x03, 0xFF, 1, 2, 3]; // Microsoft OUI
        let clone = [0xDE, 0xAD, 0xBE, 1, 2, 3];
        let pad_app = [0x03u8, 0x19, 0xc4, 0x03]; // gamepad appearance
        let mouse_app = [0x03u8, 0x19, 0xc2, 0x03]; // BLE mouse appearance

        // public controller OUI, gamepad appearance -> ok
        assert!(le_pair_candidate_ok(&ms, 0, &pad_app));
        // public controller OUI but appearance says mouse -> rejected
        assert!(!le_pair_candidate_ok(&ms, 0, &mouse_app));
        // random address -> never
        assert!(!le_pair_candidate_ok(&ms, 1, &pad_app));
        // unknown OUI -> never
        assert!(!le_pair_candidate_ok(&clone, 0, &pad_app));
        // controller OUI, no appearance field -> ok
        assert!(le_pair_candidate_ok(&ms, 0, &[]));
    }

    #[test]
    fn le_adv_action_gates_and_precedence() {
        use LeAdvAction::*;
        let ms = [0x00u8, 0x03, 0xFF, 1, 2, 3]; // Microsoft OUI (public)
        let rnd = [0xE6u8, 0x91, 0x31, 0x4A, 0x17, 0xCD]; // static random
        let pad_app = [0x03u8, 0x19, 0xc4, 0x03]; // gamepad appearance
        let mouse_app = [0x03u8, 0x19, 0xc2, 0x03]; // mouse appearance
        let hid_svc = [0x03u8, 0x03, 0x12, 0x18]; // 0x1812 in the UUID16 list
        let closed = PairPolicy {
            kbm_enabled: true,
            force_pair: false,
            pair_any: false,
        };
        let pairing = PairPolicy {
            force_pair: true,
            ..closed
        };
        let no_kbm = PairPolicy {
            kbm_enabled: false,
            ..pairing
        };

        // NEW pad through the OUI gate - no pairing session needed
        assert_eq!(
            le_adv_action(&ms, 0, &pad_app, false, false, false, false, closed),
            ConnectPad
        );
        // NEW mouse: gate closed -> never; gate open -> kbm (appearance or HID)
        assert_eq!(
            le_adv_action(&rnd, 1, &mouse_app, false, false, false, false, closed),
            Ignore
        );
        assert_eq!(
            le_adv_action(&rnd, 1, &mouse_app, false, false, false, false, pairing),
            ConnectKbm
        );
        assert_eq!(
            le_adv_action(&rnd, 1, &hid_svc, false, false, false, false, pairing),
            ConnectKbm
        );
        // kbm disabled -> even a pairing session ignores mice
        assert_eq!(
            le_adv_action(&rnd, 1, &mouse_app, false, false, false, false, no_kbm),
            Ignore
        );
        // a random-address HID advertiser NEVER auto-pairs as a pad
        assert_eq!(
            le_adv_action(&rnd, 1, &pad_app, false, false, false, false, closed),
            Ignore
        );

        // bonded devices reconnect on sight, gate closed, regardless of AD
        assert_eq!(
            le_adv_action(&rnd, 1, &[], true, false, false, false, closed),
            ConnectPad
        );
        assert_eq!(
            le_adv_action(&rnd, 1, &[], false, false, true, false, closed),
            ConnectKbm
        );
        // ... but not while already connected
        assert_eq!(
            le_adv_action(&rnd, 1, &[], true, true, false, false, closed),
            Ignore
        );
        assert_eq!(
            le_adv_action(&rnd, 1, &[], false, false, true, true, closed),
            Ignore
        );
        // pad bond outranks kbm bond for one address
        assert_eq!(
            le_adv_action(&rnd, 1, &[], true, false, true, false, closed),
            ConnectPad
        );
    }

    #[test]
    fn reencryption_recovery_split() {
        // key rejected -> stale bond, delete + re-pair
        assert!(reencryption_failure_is_stale_bond(0x05)); // AUTHENTICATION_FAILURE
        assert!(reencryption_failure_is_stale_bond(0x06)); // PIN_OR_KEY_MISSING
                                                           // link died before security finished -> bond may be fine, keep it
        assert!(!reencryption_failure_is_stale_bond(0x08)); // CONNECTION_TIMEOUT
        assert!(!reencryption_failure_is_stale_bond(0x13)); // REMOTE_USER_TERMINATED
        assert!(!reencryption_failure_is_stale_bond(0x00));
    }
}

// --- Meshtastic advertiser identification -----------------------------------
// The test plan requires selecting the radio by NAME, then confirming that the
// name's four-hex-digit suffix equals the MAC suffix - never by address alone
// and never from an unnamed advertisement.

/// The Local Name AD field: complete (0x09) wins, shortened (0x08) is taken
/// only when no complete one is present. `None` for absent or non-UTF-8 names.
///
/// A Meshtastic radio usually puts its name in the SCAN RESPONSE, which is a
/// separate report from the same MAC as the advertisement - so callers MUST
/// run this on every report, not only the first one seen per address.
#[must_use]
pub fn adv_local_name(ad: &[u8]) -> Option<&str> {
    let mut short = None;
    for (ty, data) in ad_fields(ad) {
        // Strip trailing NUL padding some stacks append.
        let data = match data.iter().position(|b| *b == 0) {
            Some(z) => &data[..z],
            None => data,
        };
        if data.is_empty() {
            continue;
        }
        match ty {
            0x09 => return core::str::from_utf8(data).ok(),
            0x08 if short.is_none() => short = core::str::from_utf8(data).ok(),
            _ => {}
        }
    }
    short
}

/// The four hex characters of a `Meshtastic XXXX` local name, lowercased. Any
/// other shape (wrong prefix, wrong length, non-hex) yields `None`.
///
/// An underscore or a space can separate the prefix from the suffix.
/// A missing separator is not accepted.
#[must_use]
pub fn mesh_name_suffix(name: &str) -> Option<[u8; 4]> {
    let rest = name.trim().strip_prefix("Meshtastic")?;
    let rest = rest
        .strip_prefix('_')
        .or_else(|| rest.strip_prefix(' '))?
        .trim();
    let b = rest.as_bytes();
    if b.len() != 4 || !b.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    Some([
        b[0].to_ascii_lowercase(),
        b[1].to_ascii_lowercase(),
        b[2].to_ascii_lowercase(),
        b[3].to_ascii_lowercase(),
    ])
}

/// Plan step 1.4: do the last four hex characters of the MAC, ignoring
/// separators and case, equal the advertised `XXXX`?
#[must_use]
pub fn mesh_suffix_matches_addr(suffix: &[u8; 4], addr: &[u8; 6]) -> bool {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mac = [
        HEX[(addr[4] >> 4) as usize],
        HEX[(addr[4] & 0xf) as usize],
        HEX[(addr[5] >> 4) as usize],
        HEX[(addr[5] & 0xf) as usize],
    ];
    suffix.eq_ignore_ascii_case(&mac)
}

/// Is this advertisement a Meshtastic radio, and does its name suffix agree
/// with its address? Returns the lowercased suffix so callers can log it.
#[must_use]
pub fn mesh_adv_identity(ad: &[u8], addr: &[u8; 6]) -> Option<([u8; 4], bool)> {
    let suffix = mesh_name_suffix(adv_local_name(ad)?)?;
    let ok = mesh_suffix_matches_addr(&suffix, addr);
    Some((suffix, ok))
}

/// The configured mesh name: either the whole advertised name or just
/// its four hex digits, so they can paste what the radio shows or what the
/// scan logged.
#[must_use]
pub fn mesh_name_spec(spec: &str) -> Option<[u8; 4]> {
    let spec = spec.trim();
    mesh_name_suffix(spec).or_else(|| {
        let b = spec.as_bytes();
        (b.len() == 4 && b.iter().all(u8::is_ascii_hexdigit)).then(|| {
            [
                b[0].to_ascii_lowercase(),
                b[1].to_ascii_lowercase(),
                b[2].to_ascii_lowercase(),
                b[3].to_ascii_lowercase(),
            ]
        })
    })
}

/// A BLE passkey as the radio displays it: up to six decimal digits. Leading
/// zeros are significant to the display but not to the value, and surrounding
/// whitespace is tolerated because the file is written by hand.
#[must_use]
pub fn mesh_parse_pin(raw: &[u8]) -> Option<u32> {
    let s = core::str::from_utf8(raw).ok()?.trim();
    if s.is_empty() || s.len() > 6 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u32>().ok()
}

#[cfg(test)]
mod mesh_name_tests {
    use super::*;

    fn ad_name(ty: u8, name: &str) -> Vec<u8> {
        let mut v = vec![u8::try_from(name.len() + 1).unwrap(), ty];
        v.extend_from_slice(name.as_bytes());
        v
    }

    #[test]
    fn complete_name_wins_over_shortened() {
        let mut ad = ad_name(0x08, "Mesh");
        ad.extend(ad_name(0x09, "Meshtastic 1a2b"));
        assert_eq!(adv_local_name(&ad), Some("Meshtastic 1a2b"));
        // A shortened name is still used when that is all the radio sent.
        assert_eq!(adv_local_name(&ad_name(0x08, "Mesh")), Some("Mesh"));
        assert_eq!(adv_local_name(&[]), None);
    }

    #[test]
    fn name_survives_flags_and_nul_padding() {
        let mut ad = vec![0x02, 0x01, 0x06]; // Flags
        ad.extend_from_slice(&[0x08, 0x09, b'M', b'e', b's', b'h', 0, 0, 0]);
        assert_eq!(adv_local_name(&ad), Some("Mesh"));
    }

    #[test]
    fn suffix_parses_the_real_underscore_name() {
        // Locally administered example address with a matching name suffix.
        let addr = [0x02, 0x00, 0x00, 0x00, 0x45, 0x3a];
        assert_eq!(mesh_name_suffix("Meshtastic_453a"), Some(*b"453a"));
        let ad = ad_name(0x09, "Meshtastic_453a");
        assert_eq!(mesh_adv_identity(&ad, &addr), Some((*b"453a", true)));
        assert_eq!(mesh_name_spec("Meshtastic_453a"), Some(*b"453a"));
        // A separator is still required - this is a different name.
        assert_eq!(mesh_name_suffix("Meshtastic453a"), None);
    }

    #[test]
    fn suffix_parses_only_the_meshtastic_shape() {
        assert_eq!(mesh_name_suffix("Meshtastic 1A2b"), Some(*b"1a2b"));
        assert_eq!(mesh_name_suffix("Meshtastic 1a2"), None);
        assert_eq!(mesh_name_suffix("Meshtastic 1a2bc"), None);
        assert_eq!(mesh_name_suffix("Meshtastic zzzz"), None);
        assert_eq!(mesh_name_suffix("Meshtastic"), None);
        assert_eq!(mesh_name_suffix("Not Meshtastic 1a2b"), None);
    }

    #[test]
    fn suffix_is_checked_against_the_mac_tail() {
        let addr = [0xd4, 0xd4, 0xda, 0x00, 0x1a, 0x2b];
        assert!(mesh_suffix_matches_addr(b"1a2b", &addr));
        assert!(mesh_suffix_matches_addr(b"1A2B", &addr)); // case-insensitive
        assert!(!mesh_suffix_matches_addr(b"1a2c", &addr));
        let named = ad_name(0x09, "Meshtastic 1a2b");
        assert_eq!(mesh_adv_identity(&named, &addr), Some((*b"1a2b", true)));
        // A name/address disagreement is REPORTED (false), never hidden.
        let other = [0, 0, 0, 0, 0xff, 0xff];
        assert_eq!(mesh_adv_identity(&named, &other), Some((*b"1a2b", false)));
        // An unnamed or non-Meshtastic advertiser is not an identity at all.
        let pad = ad_name(0x09, "Wireless Controller");
        assert_eq!(mesh_adv_identity(&pad, &addr), None);
    }

    #[test]
    fn name_spec_accepts_full_name_or_bare_suffix() {
        assert_eq!(mesh_name_spec("Meshtastic 1A2b"), Some(*b"1a2b"));
        assert_eq!(mesh_name_spec(" 1a2b "), Some(*b"1a2b"));
        assert_eq!(mesh_name_spec("1a2"), None);
        assert_eq!(mesh_name_spec("Meshtastic zzzz"), None);
    }

    #[test]
    fn pin_parser_takes_only_displayed_passkeys() {
        assert_eq!(mesh_parse_pin(b"123456\n"), Some(123_456));
        assert_eq!(mesh_parse_pin(b"  7  "), Some(7));
        assert_eq!(mesh_parse_pin(b"000042\n"), Some(42));
        assert_eq!(mesh_parse_pin(b"1234567"), None); // longer than a passkey
        assert_eq!(mesh_parse_pin(b""), None);
        assert_eq!(mesh_parse_pin(b"12 34"), None);
        assert_eq!(mesh_parse_pin(b"abc"), None);
        assert_eq!(mesh_parse_pin(&[0xff, 0xfe]), None);
    }
}
