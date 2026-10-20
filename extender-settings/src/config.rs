// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::schema;

/// The EV routing choice owns these keys, never the developer identity.
pub const EV_KEYS: [&str; 3] = ["feature.sensor", "sensor_ev_advertise", "sensor_ev_send"];
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub base: String,
    pub values: BTreeMap<String, i32>,
    pub revision: u64,
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

// Read a signed decimal prefix and clamp it to a 32-bit integer.
fn decimal_int(text: &str) -> Option<i32> {
    let text = text.trim_start_matches([' ', '\t']);
    let sign = usize::from(text.starts_with(['+', '-']));
    let digits = text
        .as_bytes()
        .get(sign..)?
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    if digits == 0 {
        return None;
    }
    let parsed = text[..sign + digits].parse::<i64>().unwrap_or_else(|_| {
        if text.starts_with('-') {
            i64::MIN
        } else {
            i64::MAX
        }
    });
    Some(parsed.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32)
}

/// Read the first matching key as a signed decimal prefix.
/// Leading zeroes do not change the number base. An invalid value uses the default.
pub fn base_int(base: &str, key: &str, default: i32) -> i32 {
    crate::raw_conf_str(base, key)
        .and_then(decimal_int)
        .unwrap_or(default)
}

pub fn base_feature(base: &str, feature: &str) -> bool {
    !feature.is_empty()
        && base
            .split(['\n', '\r'])
            .map(|line| line.trim_matches([' ', '\t']))
            .any(|line| line == feature && !line.starts_with('#'))
}

impl Snapshot {
    pub fn token(&self) -> String {
        format!("{:08x}-{}", crc32(self.base.as_bytes()), self.revision)
    }

    pub fn same_startup_settings(&self, other: &Self) -> bool {
        schema::SETTINGS
            .iter()
            .all(|s| self.get(s.key, s.default) == other.get(s.key, s.default))
    }

    pub fn get(&self, key: &str, default: i32) -> i32 {
        if key == "ev_routing" {
            let on = self.feature("sensor")
                && self.get("sensor_ev_advertise", 0) != 0
                && self.get("sensor_ev_send", 0) != 0;
            return if on {
                1
            } else if self.get("sensor_ev_advertise", 0) == 0 && self.get("sensor_ev_send", 0) == 0
            {
                0
            } else {
                2
            };
        }
        if let Some(n) = self.values.get(key) {
            return *n;
        }
        if let Some(name) = key.strip_prefix("feature.") {
            return i32::from(base_feature(&self.base, name));
        }
        base_int(&self.base, key, default)
    }

    pub fn feature(&self, name: &str) -> bool {
        schema::FEATURES.contains(&name) && self.get(&format!("feature.{name}"), 0) != 0
    }

    pub fn changed(&self, edits: &[(String, String)]) -> Result<Self, String> {
        if edits.is_empty() || edits.len() > schema::SETTINGS.len() {
            return Err("empty or oversized changes".into());
        }
        let mut next = self.clone();
        let mut seen = BTreeMap::new();
        for (key, text) in edits {
            let s = schema::find(key).ok_or_else(|| format!("unsupported setting: {key}"))?;
            if s.readonly {
                return Err(format!("legacy setting is read only: {key}"));
            }
            if seen.insert(key, ()).is_some() {
                return Err(format!("duplicate edit: {key}"));
            }
            if text == "inherit" {
                if key == "ev_routing" {
                    for key in EV_KEYS {
                        next.values.remove(key);
                    }
                    continue;
                }
                next.values.remove(key);
                continue;
            }
            let n: i32 = text
                .parse()
                .map_err(|_| format!("invalid integer: {key}"))?;
            if n < s.min
                || n > s.max
                || (key == "main_dpi" && n != 0 && n < 60)
                || (key == "main_fps" && n != 0 && n != 30)
                || (key == "media_audio_codec" && !matches!(n, 0 | 2 | 4))
            {
                return Err(format!("value outside supported range: {key}"));
            }
            if key == "ev_routing" {
                // EV routing does not select a developer head-unit identity.
                next.values.insert("sensor_ev_advertise".into(), n);
                next.values.insert("sensor_ev_send".into(), n);
            } else {
                next.values.insert(key.clone(), n);
            }
        }
        // Enabling these user controls also enables their implementation prerequisites.
        for key in [
            "ev_routing",
            "sensor_ev_send",
            "sensor_ev_advertise",
            "sensor_raw_ev_trip",
        ] {
            if edits.iter().any(|(k, v)| k == key && v == "1") {
                if edits.iter().any(|(k, v)| k == "feature.sensor" && v != "1") {
                    return Err("conflicting dependency: feature.sensor".into());
                }
                next.values.insert("feature.sensor".into(), 1);
            }
        }
        if edits.iter().any(|(k, v)| k == "sensor_ev_send" && v == "1")
            && next.get("sensor_ev_advertise", 0) == 0
        {
            return Err("enable EV routing integration before sending the model".into());
        }
        if edits.iter().any(|(k, _)| k == "ev_routing")
            && edits
                .iter()
                .any(|(k, _)| ["sensor_ev_advertise", "sensor_ev_send"].contains(&k.as_str()))
        {
            return Err("EV preset and individual EV fields cannot be changed together".into());
        }
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> Snapshot {
        Snapshot {
            base: "sensor\nsensor_ev_capacity_wh=28500\n".into(),
            values: BTreeMap::new(),
            revision: 0,
        }
    }
    fn edit(key: &str, value: &str) -> (String, String) {
        (key.into(), value.into())
    }
    #[test]
    fn media_codec_accepts_only_consumer_enum_values() {
        let state = snapshot();
        for value in ["0", "2", "4", "inherit"] {
            assert!(state.changed(&[edit("media_audio_codec", value)]).is_ok());
        }
        for value in ["-1", "1", "3", "5"] {
            assert!(state.changed(&[edit("media_audio_codec", value)]).is_err());
        }
        assert!(!state.values.contains_key("media_audio_codec"));
    }
    #[test]
    fn decimal_reader_uses_first_key_and_distinguishes_features() {
        let base = "# sensor\ncluster=0\ncluster \nx=no\n x = 13 # comment\nx=42\n";
        assert!(base_feature(base, "cluster"));
        assert!(!base_feature(base, "sensor"));
        assert_eq!(base_int(base, "x", 9), 9);
        assert_eq!(base_int("x=13tail", "x", 0), 13);
        assert_eq!(base_int("x=-9", "x", 0), -9);
    }
    #[test]
    fn preset_is_one_transaction_and_off_preserves_identity() {
        let on = snapshot().changed(&[edit("ev_routing", "1")]).unwrap();
        assert_eq!(on.get("ev_routing", 0), 1);
        assert!(on.feature("sensor"));
        let off = on.changed(&[edit("ev_routing", "0")]).unwrap();
        assert_eq!(off.get("sensor_ev_send", 1), 0);
        assert_eq!(off.get("identify_as_dhu", 0), 0);
        let mut custom = snapshot();
        custom.values.insert("identify_as_dhu".into(), 1);
        for value in ["1", "0"] {
            custom = custom.changed(&[edit("ev_routing", value)]).unwrap();
            assert_eq!(custom.get("identify_as_dhu", 0), 1);
        }
        assert_eq!(off.get("sensor_ev_capacity_wh", 0), 28500);
        let reset = custom.changed(&[edit("ev_routing", "inherit")]).unwrap();
        assert!(EV_KEYS.iter().all(|key| !reset.values.contains_key(*key)));
        assert_eq!(reset.get("identify_as_dhu", 0), 1);
        assert_eq!(reset.get("ev_routing", 0), 0);
    }
    #[test]
    fn identity_uses_only_the_canonical_key_and_preserves_unknown_data() {
        let mut state = snapshot();
        state.base.push_str("unknown_saved_choice=1\n");
        state.values.insert("unknown_saved_choice".into(), 1);
        assert_eq!(state.get("identify_as_dhu", 0), 0);
        state.base.push_str("identify_as_dhu=1\n");
        assert_eq!(state.get("identify_as_dhu", 0), 1);
        state.values.insert("identify_as_dhu".into(), 0);
        assert_eq!(state.get("identify_as_dhu", 1), 0);
        let enabled = state.changed(&[edit("ev_routing", "1")]).unwrap();
        assert_eq!(enabled.get("ev_routing", 0), 1);
        assert_eq!(enabled.get("identify_as_dhu", 1), 0);
        assert_eq!(enabled.values.get("unknown_saved_choice"), Some(&1));
        assert!(state.changed(&[edit("unknown_saved_choice", "1")]).is_err());
    }

    #[test]
    fn strict_new_values_and_conflicting_dependencies_are_rejected() {
        for value in ["3foo", "2147483648", "0", "-1"] {
            assert!(snapshot()
                .changed(&[edit("sensor_ev_capacity_wh", value)])
                .is_err());
        }
        assert!(snapshot()
            .changed(&[edit("ev_routing", "1"), edit("feature.sensor", "0")])
            .is_err());
        assert!(snapshot().changed(&[edit("feature.ui", "1")]).is_err());
        assert!(snapshot().changed(&[edit("not_supported", "1")]).is_err());
    }
    #[test]
    fn inherit_and_feature_off_override_without_modifying_base() {
        let s = snapshot();
        let n = s
            .changed(&[
                edit("feature.sensor", "0"),
                edit("sensor_ev_capacity_wh", "30100"),
            ])
            .unwrap();
        assert!(!n.feature("sensor"));
        assert_eq!(n.base, s.base);
        let reset = n
            .changed(&[edit("sensor_ev_capacity_wh", "inherit")])
            .unwrap();
        assert_eq!(reset.get("sensor_ev_capacity_wh", 30000), 28500);
    }
    #[test]
    fn custom_import_is_preserved_by_unrelated_edit() {
        let mut s = snapshot();
        s.base.push_str("sensor_ev_send=1\n");
        assert_eq!(s.get("ev_routing", 0), 2);
        assert_eq!(
            s.changed(&[edit("main_dpi", "160")])
                .unwrap()
                .get("ev_routing", 0),
            2
        );
    }
}

#[cfg(test)]
mod decimal_contract {

    use super::*;
    #[test]
    fn both_loaders_use_one_decimal_reader() {
        for (text, value) in [
            ("x=010", 10),
            ("x=-020", -20),
            ("x=08", 8),
            ("x=0x10", 0),
            ("x=+19tail", 19),
            ("x=bad\nx=3", 7),
            ("x =5\nx=9", 5),
            ("#x=12\r\n\tx=17\r", 17),
            ("x=4294967295", i32::MAX),
            ("x=-4294967295", i32::MIN),
        ] {
            assert_eq!(base_int(text, "x", 7), value);
            assert_eq!(crate::carplay::base_int(text, "x", 7), value);
        }
    }
}
