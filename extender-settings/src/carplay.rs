// SPDX-License-Identifier: MIT OR Apache-2.0
//! CarPlay setting descriptions and configuration editing.
use crate::{carplay_store, config::crc32, schema::Setting};
use std::{collections::BTreeMap, path::Path};

pub const BEGIN: &str = "# carplay-settings overrides v1\n";
pub const END: &str = "# end carplay-settings overrides\n";
const MARKER: &str = "carplay-settings overrides";
// Read these delimiters for compatibility only.
const LEGACY_BEGIN: &str = "# lsd-extender CarPlay overrides v1\n";
const LEGACY_END: &str = "# end lsd-extender CarPlay overrides\n";
const LEGACY_MARKER: &str = "lsd-extender CarPlay overrides";
pub const LIMIT: usize = 65534;

const fn field(
    key: &'static str,
    group: &'static str,
    label: &'static str,
    default: i32,
    min: i32,
    max: i32,
) -> Setting {
    Setting {
        key,
        group,
        label,
        boolean: min == 0 && max == 1,
        default,
        min,
        max,
        step: 1,
        readonly: false,
    }
}

/// The shared Displays row: where the phone's cluster stream goes. A
/// Derived setting: it is not a key in
/// dio_loader.conf. Its value is derived from the keys in VIEW_KEYS and a
/// save writes all of them. The label carries the choices after `|`; the UI
/// shows it as a dropdown. Value 3 = the keys match no choice (hand-edited).
pub const VIEW_KEY: &str = "cluster_view";
const VIEW_LABEL: &str = "Cluster map|Off|Instrument cluster|Head unit";
pub const VIEW_CUSTOM: i32 = 3;

/// Keys owned by the Cluster map choice. cluster_output=0 disables the
/// actual display/service offer; 1 selects MOST and 2 the head-unit decoder.
/// cluster_enable controls the direct MOST writer independently.
pub const VIEW_KEYS: [&str; 2] = ["cluster_enable", "cluster_output"];

fn view_values(view: i32) -> [i32; 2] {
    match view {
        0 => [0, 0],
        1 => [1, 1],
        _ => [0, 2],
    }
}

/// Cluster view of a config text, with the native consumer fallbacks.
/// An absent output preserves the configured bare-feature behavior.
pub fn view_of(text: &str) -> i32 {
    let output = base_int(text, "cluster_output", -1);
    if output == 0 {
        0
    } else if output == 2 {
        2
    } else if base_int(text, "cluster_enable", 0) != 0 {
        1
    } else {
        VIEW_CUSTOM
    }
}

/// Editable by key (older saves, the diagnostic feed, tests) but not shown:
/// the Cluster view row owns them.
const HIDDEN: &[Setting] = &[
    field(
        "cluster_enable",
        "Projection",
        "Cluster video output",
        0,
        0,
        1,
    ),
    field(
        "cluster_output",
        "Projection",
        "Cluster video output (0 off, 1 cluster, 2 head unit)",
        -1,
        -1,
        2,
    ),
];

fn setting(key: &str) -> Option<&'static Setting> {
    SETTINGS.iter().chain(HIDDEN).find(|s| s.key == key)
}

// Fallbacks checked against the native consumer calls, not the sample config.
pub const SETTINGS: &[Setting] = &[
    field(
        "altscreen_override_geometry",
        "Displays",
        "Custom display settings",
        1,
        0,
        1,
    ),
    field(
        "altscreen_width",
        "Displays",
        "Width (pixels)",
        800,
        320,
        1920,
    ),
    field(
        "altscreen_height",
        "Displays",
        "Height (pixels)",
        480,
        240,
        1080,
    ),
    field("altscreen_max_fps", "Displays", "Frame rate", 30, 1, 60),
    field(
        "altscreen_width_mm",
        "Displays",
        "Physical width (mm)",
        217,
        1,
        1000,
    ),
    field(
        "altscreen_height_mm",
        "Displays",
        "Physical height (mm)",
        130,
        1,
        1000,
    ),
    field(
        "altscreen_initial_url",
        "Navigation",
        "Open cluster map on connection",
        0,
        0,
        1,
    ),
    field(
        "altscreen_suggest_urls",
        "Navigation",
        "Offer Maps cluster views",
        1,
        0,
        1,
    ),
];

/// Reviewed driver choices include all maintained descriptors. The underlying
/// Cluster map aliases remain hidden because the virtual row owns their edits.
pub const VISIBLE_KEYS: &[&str] = &[
    "altscreen_override_geometry",
    "altscreen_width",
    "altscreen_height",
    "altscreen_width_mm",
    "altscreen_height_mm",
    "altscreen_max_fps",
    "altscreen_initial_url",
    "altscreen_suggest_urls",
];

pub fn base_path() -> Result<std::path::PathBuf, String> {
    crate::location::config_path("dio_loader.conf")
}

pub fn base_int(text: &str, key: &str, default: i32) -> i32 {
    crate::config::base_int(text, key, default)
}

pub fn base_str(text: &str, key: &str, cap: usize) -> Option<String> {
    for line in text.split(['\n', '\r']) {
        let line = line.trim_matches([' ', '\t']);
        if line.starts_with('#') {
            continue;
        }
        if let Some(value) = line.strip_prefix(key).and_then(|s| s.strip_prefix('=')) {
            let bytes = value.as_bytes();
            let n = bytes.len().min(cap.saturating_sub(1));
            return Some(String::from_utf8_lossy(&bytes[..n]).into_owned());
        }
    }
    None
}

/// The dio loader's `dio_feature_enabled`: some non-comment line is exactly
/// `name` after trimming spaces and tabs. A `name=value` line never enables.
pub fn feature_enabled(text: &str, name: &str) -> bool {
    !name.is_empty()
        && text
            .split(['\n', '\r'])
            .map(|l| l.trim_matches([' ', '\t']))
            .any(|l| l == name && !l.starts_with('#'))
}

pub struct Snapshot {
    pub text: String,
    pub base: String,
    pub overrides: BTreeMap<String, i32>,
}

impl Snapshot {
    pub fn parse(text: String) -> Result<Self, String> {
        if text.len() > LIMIT || text.contains('\0') {
            return Err("config exceeds native limits".into());
        }
        let section = text
            .strip_prefix(BEGIN)
            .map(|rest| (rest, END))
            .or_else(|| {
                text.strip_prefix(LEGACY_BEGIN)
                    .map(|rest| (rest, LEGACY_END))
            });
        let (base, overrides) = if let Some((rest, end)) = section {
            let (body, base) = rest
                .split_once(end)
                .ok_or("incomplete CarPlay override section")?;
            let mut values = BTreeMap::new();
            for line in body.lines() {
                let (key, value) = line.split_once('=').ok_or("invalid CarPlay override")?;
                if key.is_empty()
                    || key.len() > 80
                    || !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                {
                    return Err("invalid CarPlay override key".into());
                }
                let value = value
                    .parse::<i32>()
                    .map_err(|_| "invalid CarPlay override value")?;
                if values.insert(key.to_owned(), value).is_some() {
                    return Err("duplicate CarPlay override".into());
                }
            }
            (base.to_owned(), values)
        } else {
            (text.clone(), BTreeMap::new())
        };
        if base.contains(MARKER) || base.contains(LEGACY_MARKER) {
            return Err("unsupported or misplaced CarPlay override section".into());
        }
        Ok(Self {
            text,
            base,
            overrides,
        })
    }

    pub fn token(&self) -> String {
        format!(
            "cp1-{:08x}-{}",
            crc32(self.text.as_bytes()),
            self.text.len()
        )
    }

    pub fn changed(&self, edits: &[(String, String)]) -> Result<String, String> {
        if edits.is_empty() {
            return Err("no settings supplied".into());
        }
        let mut values = self.overrides.clone();
        let mut seen = std::collections::BTreeSet::new();
        if edits.iter().any(|(k, _)| k == VIEW_KEY)
            && edits.iter().any(|(k, _)| VIEW_KEYS.contains(&k.as_str()))
        {
            return Err("Cluster view and its underlying keys edited together".into());
        }
        for (key, value) in edits {
            if !seen.insert(key) {
                return Err("duplicate CarPlay edit".into());
            }
            if key == VIEW_KEY {
                self.apply_view(&mut values, value)?;
                continue;
            }
            let setting = setting(key).ok_or("unknown CarPlay setting")?;
            if value == "inherit" {
                values.remove(key);
            } else {
                let number = value.parse::<i32>().map_err(|_| "expected an integer")?;
                if !(setting.min..=setting.max).contains(&number) {
                    return Err(format!(
                        "{} is outside {}..{}",
                        key, setting.min, setting.max
                    ));
                }
                values.insert(key.clone(), number);
            }
        }
        // The display receiver omits its offer at session start when custom
        // geometry has a physical size <= 0; refuse it here instead.
        let effective = |key: &str, default: i32| {
            values
                .get(key)
                .copied()
                .unwrap_or_else(|| base_int(&self.base, key, default))
        };
        if effective("altscreen_override_geometry", 1) != 0
            && (effective("altscreen_width_mm", 217) <= 0
                || effective("altscreen_height_mm", 130) <= 0)
        {
            return Err("custom size needs a physical width and height above 0 mm".into());
        }
        let mut text = String::new();
        if !values.is_empty() {
            text.push_str(BEGIN);
            for (key, value) in values {
                text.push_str(&format!("{key}={value}\n"));
            }
            text.push_str(END);
        }
        text.push_str(&self.base);
        Self::parse(text.clone())?;
        Ok(text)
    }

    /// Write the key set of one Cluster view choice, or remove it for Default.
    fn apply_view(&self, values: &mut BTreeMap<String, i32>, value: &str) -> Result<(), String> {
        if value == "inherit" {
            for k in VIEW_KEYS {
                values.remove(k);
            }
            return Ok(());
        }
        let view = value.parse::<i32>().map_err(|_| "expected an integer")?;
        if !(0..=2).contains(&view) {
            return Err(format!("{VIEW_KEY} is outside 0..2"));
        }
        // The loader loads a feature only if its bare line is in the file,
        // and the managed section holds only key=value lines.
        let needs: &[&str] = match view {
            1 => &["altscreen", "cluster"],
            2 => &["altscreen", "clustervideo"],
            _ => &[],
        };
        for f in needs {
            if !feature_enabled(&self.base, f) {
                return Err(match *f {
                    "clustervideo" => {
                        "Head unit view needs the cluster video module, not installed"
                    }
                    "cluster" => "Cluster map needs the instrument cluster module, not installed",
                    _ => "Cluster map needs the CarPlay display module, not installed",
                }
                .into());
            }
        }
        for (k, v) in VIEW_KEYS.iter().zip(view_values(view)) {
            values.insert((*k).to_owned(), v);
        }
        Ok(())
    }

    pub fn protocol(&self) -> String {
        let mut out = format!(
            "meta\t1\t{}\tSaved settings; apply on next CarPlay start\n",
            self.token()
        );
        out.push_str(&format!(
            "row\t{VIEW_KEY}\tDisplays\t{VIEW_LABEL}\tint\t{}\t{}\t0\t{VIEW_CUSTOM}\t1\t{}\t0\n",
            view_of(&self.text),
            view_of(&self.base),
            u8::from(!VIEW_KEYS.iter().any(|k| self.overrides.contains_key(*k)))
        ));
        for s in SETTINGS.iter().filter(|s| VISIBLE_KEYS.contains(&s.key)) {
            let display = |value| {
                if s.boolean {
                    i32::from(value != 0)
                } else {
                    value
                }
            };
            let default = display(base_int(&self.base, s.key, s.default));
            let value = display(base_int(&self.text, s.key, s.default));
            out.push_str(&format!(
                "row\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t1\t{}\t0\n",
                s.key,
                s.group,
                s.label,
                if s.boolean { "bool" } else { "int" },
                value,
                default,
                s.min,
                s.max,
                u8::from(!self.overrides.contains_key(s.key))
            ));
        }
        out.push_str("end\t1\n");
        out
    }
}

pub use carplay_store::{commit, load, load_present};

/// The resolved configuration path.
pub fn absent_text(_argument: &str, path: &Path) -> String {
    path.display().to_string()
}

/// The helper's answer for a missing config: one `absent` record with the
/// config name, then the end record. Exit status 0: this is a normal state.
pub fn absent_protocol(name: &str) -> String {
    format!(
        "absent\t{}\nend\t1\n",
        name.replace(['\n', '\r', '\t'], " ")
    )
}

#[cfg(test)]
mod loader_semantics {
    use super::*;

    #[test]
    fn legacy_section_reads_and_next_edit_uses_current_delimiters() {
        let base = "altscreen\ncluster\ncluster_output=1\n";
        let original = format!("{LEGACY_BEGIN}altscreen_max_fps=30\n{LEGACY_END}{base}");
        let snapshot = Snapshot::parse(original.clone()).unwrap();
        assert_eq!(snapshot.base, base);
        assert_eq!(snapshot.overrides.get("altscreen_max_fps"), Some(&30));
        let updated = snapshot
            .changed(&[("altscreen_max_fps".into(), "30".into())])
            .unwrap();
        assert!(updated.starts_with(BEGIN));
        assert!(updated.contains(END));
        assert!(!updated.contains(LEGACY_MARKER));
        assert_eq!(
            base_int(&original, "altscreen_max_fps", 0),
            base_int(&updated, "altscreen_max_fps", 0)
        );
        assert_eq!(Snapshot::parse(updated).unwrap().base, base);
    }

    #[test]
    fn section_delimiters_cannot_be_mixed_or_repeated() {
        for (begin, end) in [(BEGIN, LEGACY_END), (LEGACY_BEGIN, END)] {
            assert!(Snapshot::parse(format!("{begin}cluster_width=800\n{end}")).is_err());
        }
        for marker in [BEGIN, LEGACY_BEGIN] {
            assert!(Snapshot::parse(format!("cluster\n{marker}")).is_err());
            assert!(Snapshot::parse(format!("{BEGIN}{END}{marker}")).is_err());
        }
    }

    #[test]
    fn str_first_match_trimmed_and_capped() {
        let t = "# c\n  altscreen_uuid=abc \t\nkey=v1\nkey=v2\nlong=0123456789\n";
        assert_eq!(base_str(t, "altscreen_uuid", 64).as_deref(), Some("abc"));
        assert_eq!(base_str(t, "key", 64).as_deref(), Some("v1"));
        assert_eq!(base_str(t, "long", 5).as_deref(), Some("0123"));
        assert_eq!(base_str(t, "missing", 64), None);
        assert_eq!(base_str("keyx=1\nkey=2", "key", 64).as_deref(), Some("2"));
    }

    #[test]
    fn feature_whole_line_only() {
        let t = "cluster_width=800\n  probe \n#altscreen\n";
        assert!(feature_enabled(t, "probe"));
        assert!(!feature_enabled(t, "cluster"));
        assert!(!feature_enabled(t, "altscreen"));
        assert!(!feature_enabled(t, ""));
    }
}
