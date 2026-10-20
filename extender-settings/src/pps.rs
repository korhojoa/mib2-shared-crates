// SPDX-License-Identifier: MIT OR Apache-2.0
//! The QNX PPS object that carries gal's live settings status
//! (`/ramdisk/pps/gal/settings_active`; gal-extender `docs/PPS_IPC.md`). The
//! same format as gal-extender's `gal_feature_api::pps`, kept here because
//! this crate is self-contained: one write of `attr::value` lines on a
//! descriptor opened without truncation (a truncating open deletes every
//! attribute first), `-attr` lines deleting what the previous write had and
//! this one lacks, and a reader that takes `name:<encoding>:value` lines and
//! skips the `@object` line and the deletions.

use std::io::{Read as _, Write as _};

/// The largest object text read: well above any settings or status object.
const READ_LIMIT: usize = 64 * 1024;

/// The whole object text in ONE read(). PPS returns an object only into a
/// buffer that holds all of it and fails a smaller one with EMSGSIZE (os
/// error 240); std's read_to_end starts with a small probe read, so it fails
/// on any object larger than that initial buffer.
pub fn read(path: &std::path::Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = vec![0u8; READ_LIMIT];
    let n = f
        .read(&mut buf)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    buf.truncate(n);
    String::from_utf8(buf).map_err(|_| format!("{}: not UTF-8", path.display()))
}

/// The attributes of an object's text, in order.
pub fn attrs(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.lines().filter_map(attr)
}

/// One `name::value` (or `name:enc:value`) line; None for `@object`,
/// `-name` and anything else.
pub fn attr(line: &str) -> Option<(&str, &str)> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let first = line.find(':')?;
    if first == 0 || line.starts_with('@') || line.starts_with('-') {
        return None;
    }
    let rest = &line[first + 1..];
    let second = rest.find(':')?;
    Some((&line[..first], &rest[second + 1..]))
}

/// The text of one publish: `key=value` lines as `key::value`, plus a
/// `-key` for every key of `previous` that `kv` lacks; and the keys of `kv`
/// for the next call.
pub fn render(kv: &str, previous: &[String]) -> (String, Vec<String>) {
    let mut text = String::with_capacity(kv.len() + 16);
    let mut keys: Vec<String> = Vec::new();
    for line in kv.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.is_empty() || k.contains(':') {
            continue;
        }
        text.push_str(k);
        text.push_str("::");
        text.push_str(v);
        text.push('\n');
        if !keys.iter().any(|x| x == k) {
            keys.push(k.to_string());
        }
    }
    for k in previous {
        if !keys.iter().any(|x| x == k) {
            text.push('-');
            text.push_str(k);
            text.push('\n');
        }
    }
    (text, keys)
}

/// The keys an existing object holds (a new process deletes the ones it
/// does not write in its first publish); none when it is absent.
pub fn existing_keys(path: &std::path::Path) -> Vec<String> {
    read(path)
        .map(|t| attrs(&t).map(|(k, _)| k.to_string()).collect())
        .unwrap_or_default()
}

/// One write of `text` to the object at `path`, its directory created, the
/// object never truncated.
pub fn write(path: &std::path::Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let n = f.write(text.as_bytes()).map_err(|e| e.to_string())?;
    if n == text.len() {
        Ok(())
    } else {
        Err("short PPS write".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_round_trip() {
        let (text, keys) = render(
            "version=1\npid=7\nloaded.cluster=1\nend=1\n",
            &["loaded.sensor".into()],
        );
        assert_eq!(
            text,
            "version::1\npid::7\nloaded.cluster::1\nend::1\n-loaded.sensor\n"
        );
        assert_eq!(keys, ["version", "pid", "loaded.cluster", "end"]);
        let object = format!("@settings_active\n{text}");
        let seen: Vec<_> = attrs(&object).collect();
        assert_eq!(
            seen,
            [
                ("version", "1"),
                ("pid", "7"),
                ("loaded.cluster", "1"),
                ("end", "1")
            ]
        );
        assert_eq!(
            attr("dir::/fs/sdb0/lsd-extender"),
            Some(("dir", "/fs/sdb0/lsd-extender"))
        );
        assert_eq!(attr("error::a: b"), Some(("error", "a: b")));
        assert_eq!(attr("pid=1"), None);
    }

    #[test]
    fn objects_are_written_whole() {
        let dir = std::env::temp_dir().join(format!("aa-settings-pps-{}", std::process::id()));
        let p = dir.join("gal/settings_active");
        write(&p, "version::1\nend::1\n").unwrap();
        assert_eq!(existing_keys(&p), ["version", "end"]);
        assert!(existing_keys(&dir.join("none")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
