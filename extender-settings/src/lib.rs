// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared Android Auto and CarPlay settings model and native helpers.
pub mod card_mount;
pub mod carplay;
mod carplay_store;
pub mod config;
pub mod location;
pub mod pps;
mod runtime;
pub mod schema;
pub mod store;
pub use runtime::snapshot_text;

pub const ABI: u32 = 1;
/// The base conf key that names the settings override directory outright.
/// A string value: the settings reader has integer keys only, so this one
/// line is read here, before the reader starts.
pub const SETTINGS_DIR_KEY: &str = "extender_settings_dir";
/// Android Auto reader status in its existing protocol-specific PPS domain.
/// The loader passes this path to `extender_settings_init`.
pub const ANDROID_AUTO_STATUS: &str = "/ramdisk/pps/gal/settings_active";
/// Resolve the Android Auto config beside this loaded component.
pub fn base_path() -> Result<std::path::PathBuf, String> {
    location::config_path("gal_loader.conf")
}

/// The value of `key=` in conf text, by the integer-key rules (white space
/// before the key and before `=` ignored, `#` lines skipped, the first line
/// with the key wins), trimmed of white space. None when absent or empty.
pub fn conf_str<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    raw_conf_str(text, key).filter(|value| !value.is_empty() && !value.contains('\0'))
}

pub fn raw_conf_str<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    for line in text.split(['\n', '\r']) {
        let line = line.trim_start_matches([' ', '\t']);
        if line.starts_with('#') {
            continue;
        }
        if let Some((name, value)) = line.split_once('=') {
            if name.trim_end_matches([' ', '\t']) == key {
                return Some(value.trim_matches([' ', '\t', '\r']));
            }
        }
    }
    None
}

fn configured_directory(text: &str) -> Option<&str> {
    conf_str(text, SETTINGS_DIR_KEY)
}

/// Resolve overrides from the reader config, then from its containing folder.
pub fn settings_dir(base: &std::path::Path) -> std::path::PathBuf {
    settings_dir_with_runtime(base, None)
}

/// An explicit settings folder wins over a supplied runtime folder.
/// Without either input, use the folder that contains the reader config.
pub fn settings_dir_with_runtime(
    base: &std::path::Path,
    runtime: Option<&std::path::Path>,
) -> std::path::PathBuf {
    if let Ok(text) = store::read_bounded(base) {
        if let Some(dir) = configured_directory(&text) {
            let dir = std::path::Path::new(dir);
            return if dir.is_absolute() {
                dir.to_path_buf()
            } else {
                base.parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .join(dir)
            };
        }
    }
    runtime.map_or_else(
        || {
            base.parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .to_path_buf()
        },
        std::path::Path::to_path_buf,
    )
}

/// Writes a helper answer to stdout. A reader that closed the pipe early
/// (`| head` on the unit) is not an error: print! would panic and abort.
pub fn emit(text: &str) {
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
}

pub fn wall_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_settings_dir_key() {
        assert_eq!(conf_str("cluster\naa_rx_log=1\n", SETTINGS_DIR_KEY), None);
        assert_eq!(
            conf_str(
                "# extender_settings_dir=/a\n  extender_settings_dir = /hd/lsd \r\n",
                SETTINGS_DIR_KEY
            ),
            Some("/hd/lsd")
        );
        assert_eq!(
            conf_str(
                "extender_settings_dir=/one\nextender_settings_dir=/two\n",
                SETTINGS_DIR_KEY
            ),
            Some("/one")
        );
        assert_eq!(conf_str("extender_settings_dir=\n", SETTINGS_DIR_KEY), None);
        assert_eq!(
            conf_str("extender_settings_dirx=/a\n", SETTINGS_DIR_KEY),
            None
        );
        assert_eq!(
            conf_str("extender_settings_dir=/a\0b\n", SETTINGS_DIR_KEY),
            None
        );
    }

    #[test]
    fn settings_follow_the_reader_folder_in_three_locations() {
        let root = std::env::temp_dir().join(format!("settings-folders-{}", std::process::id()));
        for location in [
            "second-sd/extensions",
            "usb/extensions",
            "internal/extensions",
        ] {
            let dir = root.join(location);
            std::fs::create_dir_all(&dir).unwrap();
            let base = dir.join("gal_loader.conf");
            std::fs::write(&base, "cluster\n").unwrap();
            assert_eq!(settings_dir(&base), dir);
            std::fs::write(&base, "extender_settings_dir=overrides\n").unwrap();
            assert_eq!(settings_dir(&base), dir.join("overrides"));
            let explicit = root.join("explicit-settings");
            std::fs::write(
                &base,
                format!("extender_settings_dir={}\n", explicit.display()),
            )
            .unwrap();
            assert_eq!(settings_dir(&base), explicit);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
