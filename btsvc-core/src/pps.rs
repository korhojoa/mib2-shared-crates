// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bounded PPS adapter for wireless policy and session contracts.
const KEYS: &[&str] = &[
    "version",
    "revision",
    "epoch",
    "updated_ms",
    "enabled",
    "selected",
    "allow",
    "peer",
    "session",
    "stream",
    "held",
    "accepted",
    "nosession",
    "badpeer",
    "lost",
    "restarts",
    "end",
];

const SELECTED: usize = 5;
const ALLOW: usize = 6;

pub struct Text {
    bytes: [u8; 4096],
    len: usize,
}
impl Default for Text {
    fn default() -> Self {
        Self {
            bytes: [0; 4096],
            len: 0,
        }
    }
}
impl Text {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
    #[must_use]
    /// # Panics
    /// Panics if the private ASCII storage invariant is violated.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(self.as_bytes()).expect("ASCII PPS")
    }
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    fn push(&mut self, text: &str) -> Option<()> {
        let end = self.len.checked_add(text.len())?;
        self.bytes
            .get_mut(self.len..end)?
            .copy_from_slice(text.as_bytes());
        self.len = end;
        Some(())
    }
}

/// Remove PPS object and attribute qualifiers without changing field values.
#[must_use]
pub fn content(mut line: &str) -> Option<&str> {
    while let Some(flags) = line.strip_prefix('[') {
        let (_, rest) = flags.split_once(']')?;
        line = rest;
    }
    Some(line)
}

#[must_use]
pub fn decode(text: &str) -> Option<Text> {
    if text.len() > 4096 || !text.is_ascii() {
        return None;
    }
    let mut fields = [None; KEYS.len()];
    for line in text.lines() {
        let line = content(line)?;
        if line.starts_with('@') || line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once("::")?;
        let i = KEYS.iter().position(|key| *key == name)?;
        if fields[i].replace(value).is_some() {
            return None;
        }
    }
    let mut flat = Text::default();
    for (i, name) in KEYS.iter().enumerate() {
        if let Some(value) = fields[i] {
            if *name == "allow" {
                for item in value.split(',').filter(|item| !item.is_empty()) {
                    flat.push("allow=")?;
                    flat.push(item)?;
                    flat.push("\n")?;
                }
            } else {
                flat.push(name)?;
                flat.push("=")?;
                flat.push(value)?;
                flat.push("\n")?;
            }
        }
    }
    Some(flat)
}

/// Preserve explicit allow entries without a selected peer.
/// A selected peer without allow entries writes an empty allow list.
#[must_use]
pub fn encode(flat: &str) -> Option<Text> {
    if flat.len() > 4096 || !flat.is_ascii() {
        return None;
    }
    let mut pps = Text::default();
    let mut allowed = Text::default();
    let mut names = [false; KEYS.len()];
    for line in flat.lines() {
        let (name, value) = line.split_once('=')?;
        let i = KEYS.iter().position(|key| *key == name)?;
        if i == ALLOW {
            names[ALLOW] = true;
            if !allowed.is_empty() {
                allowed.push(",")?;
            }
            allowed.push(value)?;
        } else {
            if names[i] {
                return None;
            }
            names[i] = true;
            pps.push(name)?;
            pps.push("::")?;
            pps.push(value)?;
            pps.push("\n")?;
        }
    }
    if names[SELECTED] || names[ALLOW] {
        pps.push("allow::")?;
        pps.push(allowed.as_str())?;
        pps.push("\n")?;
    }
    Some(pps)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn qualified_object_and_attribute_lines_decode() {
        let decoded = decode("[n]@policy\n[n]version::1\nrevision::2\n").unwrap();
        assert_eq!(decoded.as_str(), "version=1\nrevision=2\n");
        assert!(decode("[n@policy\nversion::1\n").is_none());
        assert!(decode("[n]epoch::1\nepoch::2\n").is_none());
    }
    #[test]
    fn roundtrip_sorts_pps_and_clears_old_permissions() {
        let flat = "version=1\nrevision=2\nepoch=3\nupdated_ms=1000\nenabled=1\nselected=01:23:45:67:89:AB\nallow=01:23:45:67:89:AB\nend=1\n";
        assert_eq!(
            decode(encode(flat).unwrap().as_str()).unwrap().as_str(),
            flat
        );
        assert!(encode(&flat.replace("allow=01:23:45:67:89:AB\n", ""))
            .unwrap()
            .as_str()
            .contains("allow::\n"));
        assert!(decode("@policy\nepoch::1\nepoch::2\n").is_none());
    }
    #[test]
    fn allow_only_policy_survives_without_a_selected_peer() {
        let flat = "allow=02:00:00:00:00:01\nallow=02:00:00:00:00:02\n";
        let encoded = encode(flat).unwrap();
        assert_eq!(decode(encoded.as_str()).unwrap().as_str(), flat);
    }
}
