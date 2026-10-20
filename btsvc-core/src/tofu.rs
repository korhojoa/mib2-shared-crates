// SPDX-License-Identifier: MIT OR Apache-2.0
//! Trust-on-first-use key records.
//!
//! This module maps a peer to a public-key fingerprint. It does not perform
//! authentication or cryptography. The caller must authenticate the peer with
//! TLS before it uses this store.
//!
//! The caller supplies file access, hashing and TLS. This module has no I/O.

/// Render a fingerprint digest as the lowercase hex the store holds.
pub fn hex64(out: &mut [u8; 64], digest: &[u8; 32]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (i, b) in digest.iter().enumerate() {
        out[i * 2] = HEX[(b >> 4) as usize];
        out[i * 2 + 1] = HEX[(b & 0x0f) as usize];
    }
}

/// What the store says about a key we have just been shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinCheck<'a> {
    /// No line for this peer. Trust it, record it - this is the first use.
    FirstUse,
    /// The stored fingerprint is the one in front of us.
    Match,
    /// A line exists and names a DIFFERENT key. Refuse; do not overwrite.
    Changed(&'a str),
}

/// Look `name` up in the store and compare.
///
/// Store format - `known_hosts`-shaped, because it can be read and edited over
/// a serial console, which is sometimes all there is:
///
/// ```text
/// <name> sha256:<64 lowercase hex>      # comments allowed
/// ```
///
/// The fingerprint is over the peer's `SubjectPublicKeyInfo`, so it survives the
/// certificate being reissued around the same key.
///
/// The first line naming a peer decides, so a duplicate appended later cannot
/// quietly override a pin. A malformed line is skipped rather than trusted: it
/// can never produce a match, and treating it as absent would turn a typo into
/// re-adoption of whatever answers next.
#[must_use]
pub fn check_pin<'a>(store: &'a str, name: &str, fp_hex: &str) -> PinCheck<'a> {
    for (who, hex) in store_lines(store) {
        if who == name {
            return if hex == fp_hex {
                PinCheck::Match
            } else {
                PinCheck::Changed(hex)
            };
        }
    }
    PinCheck::FirstUse
}

/// Does the store hold any pin for `name`?
#[must_use]
pub fn is_pinned(store: &str, name: &str) -> bool {
    store_lines(store).any(|(who, _)| who == name)
}

fn store_lines(store: &str) -> impl Iterator<Item = (&str, &str)> {
    store.lines().filter_map(|line| {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            return None;
        }
        let mut parts = line.split_whitespace();
        let (who, fp) = (parts.next()?, parts.next()?);
        let hex = fp.strip_prefix("sha256:")?;
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return None;
        }
        Some((who, hex))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_use_match_and_change() {
        let fp = "a".repeat(64);
        let other = "b".repeat(64);
        let store = format!("# note\n\nheadunit sha256:{fp}   # trailing\n");
        assert_eq!(check_pin(&store, "headunit", &fp), PinCheck::Match);
        assert_eq!(
            check_pin(&store, "headunit", &other),
            PinCheck::Changed(&fp)
        );
        assert_eq!(check_pin(&store, "phone", &fp), PinCheck::FirstUse);
        assert_eq!(check_pin("", "headunit", &fp), PinCheck::FirstUse);
        assert!(is_pinned(&store, "headunit"));
        assert!(!is_pinned(&store, "phone"));
    }

    /// A malformed line must not read as "no pin": that would re-adopt whatever
    /// answered next, which is the one thing the store exists to prevent.
    #[test]
    fn malformed_lines_never_match_and_never_pin() {
        let fp = "c".repeat(64);
        for bad in [
            "headunit",
            "headunit sha256:",
            "headunit md5:0123",
            "headunit sha256:short",
            &format!("headunit sha256:{}", fp.to_uppercase()),
        ] {
            assert_ne!(
                check_pin(bad, "headunit", &fp),
                PinCheck::Match,
                "matched {bad:?}"
            );
            assert!(!is_pinned(bad, "headunit"), "pinned from {bad:?}");
        }
    }

    /// The first line wins, so a duplicate appended later cannot override a pin.
    #[test]
    fn a_later_duplicate_cannot_override() {
        let first = "d".repeat(64);
        let second = "e".repeat(64);
        let store = format!("hu sha256:{first}\nhu sha256:{second}\n");
        assert_eq!(check_pin(&store, "hu", &second), PinCheck::Changed(&first));
    }

    #[test]
    fn hex_is_lowercase_and_full_width() {
        let mut digest = [0u8; 32];
        digest[0] = 0xde;
        digest[31] = 0x0f;
        let mut out = [0u8; 64];
        hex64(&mut out, &digest);
        let s = core::str::from_utf8(&out).unwrap();
        assert!(s.starts_with("de00") && s.ends_with("000f"), "{s}");
    }
}
