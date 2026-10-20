// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::config::{crc32, Snapshot};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const LIMIT: u64 = 128 * 1024;
const NAME: &str = "android-auto.conf";

pub(crate) fn lock_writer(file: &File) -> Result<(), String> {
    // Rust std::fs::File::try_lock returns Unsupported on QNX. Use the
    // explicitly sized libc entry point (unistd.h: F_TLOCK = 2) there.
    #[cfg(target_os = "nto")]
    {
        use std::os::fd::AsRawFd;
        unsafe extern "C" {
            fn lockf64(fd: i32, operation: i32, length: i64) -> i32;
        }
        if unsafe { lockf64(file.as_raw_fd(), 2, 0) } != 0 {
            return Err(format!(
                "settings writer lock: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
    #[cfg(not(target_os = "nto"))]
    file.try_lock()
        .map_err(|e| format!("settings writer lock: {e}"))
}

pub fn read_bounded(path: &Path) -> Result<String, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("settings file too large".into());
    }
    String::from_utf8(bytes).map_err(|_| "settings file is not UTF-8".into())
}

#[derive(Clone)]
struct Slot {
    revision: u64,
    values: BTreeMap<String, i32>,
    path: PathBuf,
}

fn decode(path: &Path) -> Result<Slot, String> {
    let text = read_bounded(path)?;
    let (body, check) = text.rsplit_once("crc=").ok_or("missing checksum")?;
    if check.len() != 9
        || !check.ends_with('\n')
        || !body.ends_with('\n')
        || u32::from_str_radix(check.trim_end(), 16).ok() != Some(crc32(body.as_bytes()))
    {
        return Err("invalid checksum".into());
    }
    let mut lines = body.lines();
    if lines.next() != Some("version=1") {
        return Err("unsupported settings version".into());
    }
    let revision = lines
        .next()
        .and_then(|l| l.strip_prefix("revision="))
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .ok_or("invalid revision")?;
    let mut values = BTreeMap::new();
    for line in lines {
        let (key, value) = line.split_once('=').ok_or("invalid setting")?;
        if key.is_empty()
            || key.len() > 80
            || !key
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'.')
        {
            return Err("invalid key".into());
        }
        let value = value.parse::<i32>().map_err(|_| "invalid integer")?;
        if values.insert(key.to_owned(), value).is_some() {
            return Err("duplicate setting".into());
        }
    }
    Ok(Slot {
        revision,
        values,
        path: path.to_owned(),
    })
}

fn slots(dir: &Path) -> Result<Option<Slot>, String> {
    let mut found = Vec::new();
    let mut invalid = false;
    for path in [dir.join(NAME), dir.join(format!("{NAME}.backup"))] {
        match fs::metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.to_string()),
            Ok(_) => {}
        }
        match decode(&path) {
            Ok(s) => found.push(s),
            Err(e) if e == "unsupported settings version" => return Err(e),
            Err(_) => invalid = true,
        }
    }
    found.sort_by_key(|s| s.revision);
    if found.len() == 2
        && found[0].revision == found[1].revision
        && found[0].values != found[1].values
    {
        return Err("conflicting settings generations".into());
    }
    if found.is_empty() && invalid {
        return Err("no valid settings generation".into());
    }
    Ok(found.pop())
}

pub fn load(base: &Path, dir: &Path) -> Result<Snapshot, String> {
    if !dir.is_dir() {
        return Err("settings card directory unavailable".into());
    }
    let base = read_bounded(base)?;
    let slot = slots(dir)?;
    Ok(Snapshot {
        base,
        values: slot.as_ref().map(|s| s.values.clone()).unwrap_or_default(),
        revision: slot.map_or(0, |s| s.revision),
    })
}

pub fn commit(
    base: &Path,
    dir: &Path,
    token: &str,
    edits: &[(String, String)],
) -> Result<Snapshot, String> {
    if !dir.is_dir() {
        return Err("settings card directory unavailable".into());
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("android-auto.lock"))
        .map_err(|e| e.to_string())?;
    lock_writer(&lock)?;
    let current = load(base, dir)?;
    if current.token() != token {
        return Err("settings changed; reload before saving".into());
    }
    let mut next = current.changed(edits)?;
    if next.values == current.values {
        return Ok(current);
    }
    next.revision = current
        .revision
        .checked_add(1)
        .ok_or("revision exhausted")?;
    let old = slots(dir)?;
    let a = dir.join(NAME);
    let path = if old.as_ref().is_some_and(|s| s.path == a) {
        dir.join(format!("{NAME}.backup"))
    } else {
        a
    };
    let mut body = format!("version=1\nrevision={}\n", next.revision);
    for (key, value) in &next.values {
        body.push_str(&format!("{key}={value}\n"));
    }
    let data = format!("{body}crc={:08x}\n", crc32(body.as_bytes()));
    if data.len() as u64 > LIMIT {
        return Err("settings file too large".into());
    }
    // A second base check closes the validation window before touching a slot.
    if read_bounded(base)? != current.base {
        return Err("base config changed; reload before saving".into());
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    file.write_all(data.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    drop(file);
    let verified = decode(&path)?;
    if verified.revision != next.revision || verified.values != next.values {
        return Err("settings verification failed".into());
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture {
        dir: PathBuf,
        base: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "extender-settings-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&dir).unwrap();
            let base = dir.join("base.conf");
            fs::write(&base, "sensor\nsensor_ev_capacity_wh=28500\n").unwrap();
            Self { dir, base }
        }
        fn save(&self, value: &str) -> Snapshot {
            let s = load(&self.base, &self.dir).unwrap();
            commit(
                &self.base,
                &self.dir,
                &s.token(),
                &[("sensor_ev_capacity_wh".into(), value.into())],
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
    #[test]
    fn writes_alternate_slots_and_recovers_torn_newest() {
        let f = Fixture::new();
        f.save("30100");
        f.save("30200");
        fs::write(f.dir.join("android-auto.conf.backup"), "partial").unwrap();
        let s = load(&f.base, &f.dir).unwrap();
        assert_eq!(s.revision, 1);
        assert_eq!(s.get("sensor_ev_capacity_wh", 0), 30100);
        let s = f.save("30300");
        assert_eq!(s.revision, 2);
        assert_eq!(
            load(&f.base, &f.dir)
                .unwrap()
                .get("sensor_ev_capacity_wh", 0),
            30300
        );
    }
    #[test]
    fn stale_revision_and_base_changes_are_rejected() {
        let f = Fixture::new();
        let old = load(&f.base, &f.dir).unwrap().token();
        f.save("30000");
        let edits = [("main_dpi".into(), "160".into())];
        assert!(commit(&f.base, &f.dir, &old, &edits).is_err());
        let current = load(&f.base, &f.dir).unwrap().token();
        fs::write(&f.base, "cluster\n").unwrap();
        assert!(commit(&f.base, &f.dir, &current, &edits).is_err());
    }
    #[test]
    fn missing_card_corruption_and_writer_contention_do_not_reset_settings() {
        let f = Fixture::new();
        f.save("30100");
        assert!(load(&f.base, &f.dir.join("missing")).is_err());
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(f.dir.join("android-auto.lock"))
            .unwrap();
        lock.try_lock().unwrap();
        let token = load(&f.base, &f.dir).unwrap().token();
        assert!(commit(
            &f.base,
            &f.dir,
            &token,
            &[("main_dpi".into(), "160".into())]
        )
        .is_err());
        drop(lock);
        fs::write(f.dir.join(NAME), "corrupt").unwrap();
        assert!(load(&f.base, &f.dir).is_err());
    }
    #[test]
    fn unknown_values_are_preserved_but_cannot_be_edited() {
        let f = Fixture::new();
        let body = "version=1\nrevision=1\nfuture_key=7\n";
        fs::write(
            f.dir.join(NAME),
            format!("{body}crc={:08x}\n", crc32(body.as_bytes())),
        )
        .unwrap();
        f.save("30000");
        assert_eq!(
            load(&f.base, &f.dir).unwrap().values.get("future_key"),
            Some(&7)
        );
    }

    #[test]
    fn newer_valid_format_is_not_silently_replaced_by_an_old_generation() {
        let f = Fixture::new();
        f.save("30100");
        let body = "version=2\nrevision=2\nfuture_key=8\n";
        fs::write(
            f.dir.join("android-auto.conf.backup"),
            format!("{body}crc={:08x}\n", crc32(body.as_bytes())),
        )
        .unwrap();
        assert_eq!(
            load(&f.base, &f.dir).unwrap_err(),
            "unsupported settings version"
        );
    }
}
