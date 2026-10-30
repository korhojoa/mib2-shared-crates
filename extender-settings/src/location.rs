// SPDX-License-Identifier: MIT OR Apache-2.0
//! Resolve loaded-component paths and validate explicit runtime location records.
use std::path::{Path, PathBuf};

/// Dynamic linker record from the QNX and POSIX dynamic loader ABI.
#[repr(C)]
struct ModuleInfo {
    file: *const std::ffi::c_char,
    base: *mut std::ffi::c_void,
    symbol: *const std::ffi::c_char,
    address: *mut std::ffi::c_void,
}

unsafe extern "C" {
    fn dladdr(address: *const std::ffi::c_void, info: *mut ModuleInfo) -> std::ffi::c_int;
}

/// Return the file that contains this function, without a guessed install root.
fn loaded_path() -> Result<PathBuf, String> {
    let mut info = ModuleInfo {
        file: std::ptr::null(),
        base: std::ptr::null_mut(),
        symbol: std::ptr::null(),
        address: std::ptr::null_mut(),
    };
    // SAFETY: The function address belongs to a loaded object. The record is writable.
    let found = unsafe {
        dladdr(
            loaded_path as *const () as *const std::ffi::c_void,
            &mut info,
        )
    };
    if found == 0 || info.file.is_null() {
        return Err("cannot resolve the loaded settings component".into());
    }
    // SAFETY: A successful lookup supplies a terminated filename owned by the linker.
    let bytes = unsafe { std::ffi::CStr::from_ptr(info.file) };
    let path = Path::new(
        bytes
            .to_str()
            .map_err(|_| "loaded component path is not UTF-8")?,
    );
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    // Relative linker names require the helper's launch directory.
    path.canonicalize()
        .map_err(|e| format!("cannot resolve loaded component path: {e}"))
}

fn beside(component: &Path, name: &str) -> Result<PathBuf, String> {
    if !component.is_absolute()
        || component
            .to_str()
            .is_none_or(|p| p.chars().any(char::is_control))
    {
        return Err("loaded component path must be absolute".into());
    }
    if Path::new(name).file_name().and_then(|s| s.to_str()) != Some(name) {
        return Err("config name must be a filename".into());
    }
    component
        .parent()
        .map(|dir| dir.join(name))
        .ok_or_else(|| "loaded component has no folder".into())
}

/// Return a config filename beside the loaded helper or library.
pub fn config_path(name: &str) -> Result<PathBuf, String> {
    beside(&loaded_path()?, name)
}

/// Resolve the AA base independently of the helper install folder. A fresh
/// running reader is authoritative. Otherwise use an adjacent base or the
/// single GAL install found on mounted media; never guess between two installs.
pub fn android_auto_config_path() -> Result<PathBuf, String> {
    if let Some(base) = live_base() {
        return Ok(base);
    }
    android_auto_base_for(&loaded_path()?, Path::new("/fs"))
}

fn android_auto_base_for(component: &Path, mounts: &Path) -> Result<PathBuf, String> {
    let adjacent = beside(component, "gal_loader.conf")?;
    if adjacent.is_file() {
        return Ok(adjacent);
    }
    let mut candidates = Vec::new();
    if let Ok(entries) = std::fs::read_dir(mounts) {
        for entry in entries {
            let entry = entry.map_err(|e| format!("cannot inspect mounted media: {e}"))?;
            let base = entry.path().join("gal-extender/gal_loader.conf");
            if base.is_file() {
                candidates.push(base);
            }
        }
    }
    match candidates.as_slice() {
        [base] => Ok(base.clone()),
        [] => Err("GAL config unavailable beside the helper or on mounted media".into()),
        _ => Err("multiple GAL configs on mounted media; select one with --base".into()),
    }
}

pub fn pps_root() -> PathBuf {
    std::env::var_os("GAL_PPS_ROOT").map_or_else(|| PathBuf::from("/ramdisk/pps"), PathBuf::from)
}

fn object_path() -> PathBuf {
    pps_root().join("extender-settings/card")
}
const PARENT_ENV: &str = "EXTENDER_SETTINGS_PARENT_PID";

fn fields(text: &str) -> std::collections::BTreeMap<&str, &str> {
    text.lines()
        .filter_map(|line| {
            let line = if line.starts_with('[') {
                line.split_once(']')?.1
            } else {
                line
            };
            crate::pps::attr(line)
        })
        .collect()
}

/// Ignore a stale publisher, malformed record or missing/relative directory.
pub fn live_card_at(object: &Path, proc_root: &Path) -> Option<PathBuf> {
    let text = crate::pps::read(object).ok()?;
    let values = fields(&text);
    if values.get("version") != Some(&"1") || values.get("end") != Some(&"1") {
        return None;
    }
    let pid = values.get("pid")?.parse::<u32>().ok()?;
    let dir = Path::new(values.get("dir")?);
    (pid != 0 && proc_root.join(pid.to_string()).exists() && dir.is_absolute() && dir.is_dir())
        .then(|| dir.to_path_buf())
}

pub fn live_card() -> Option<PathBuf> {
    live_card_at(&object_path(), Path::new("/proc"))
}

/// Accept only a complete, fresh record from a live AA reader with a real base file.
pub fn live_base_at(object: &Path, proc_root: &Path, now_ms: u128) -> Option<PathBuf> {
    let text = crate::pps::read(object).ok()?;
    let values = fields(&text);
    if values.get("version") != Some(&"1") || values.get("end") != Some(&"1") {
        return None;
    }
    let updated = values.get("updated_ms")?.parse::<u128>().ok()?;
    let age = now_ms.checked_sub(updated)?;
    if updated == 0 || age > 5000 {
        return None;
    }
    let pid = values.get("pid")?.parse::<u32>().ok()?;
    let base = values.get("base")?;
    let path = Path::new(base);
    (pid != 0
        && proc_root.join(pid.to_string()).exists()
        && !base.chars().any(char::is_control)
        && path.is_absolute()
        && path.is_file())
    .then(|| path.to_path_buf())
}

pub fn live_base() -> Option<PathBuf> {
    live_base_at(
        &pps_root().join("gal/settings_active"),
        Path::new("/proc"),
        crate::wall_ms(),
    )
}

fn record(dir: &Path, pid: u32) -> Result<String, String> {
    let path = dir.to_str().ok_or("card directory is not UTF-8")?;
    if pid == 0 || !dir.is_absolute() || !dir.is_dir() || path.chars().any(char::is_control) {
        return Err("invalid running card directory or HMI process".into());
    }
    Ok(format!("version::1\npid::{pid}\ndir::{path}\nend::1\n"))
}

/// Only the HMI launch contract enables publishing; a direct diagnostic helper
/// call does not write this global object. Parent identity is checked on QNX.
pub fn publish_from_hmi(dir: &Path) -> Result<(), String> {
    let Ok(parent) = std::env::var(PARENT_ENV) else {
        return Ok(());
    };
    let pid = parent
        .parse::<u32>()
        .map_err(|_| "invalid HMI parent PID")?;
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn getppid() -> i32;
        }
        // POSIX scalar ABI; this helper is exec'd directly by the HMI child shell.
        if u32::try_from(unsafe { getppid() }).ok() != Some(pid) {
            return Err("settings helper parent does not match HMI launch".into());
        }
    }
    let text = record(dir, pid)?;
    crate::pps::write(&object_path(), &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aa_helper_on_other_card_slot_finds_the_gal_install_and_refuses_ambiguity() {
        let root = std::env::temp_dir().join(format!("aa-card-layout-{}", std::process::id()));
        let mounts = root.join("fs");
        let helper = mounts.join("sdb0/lsd-extender/android-auto-settings");
        let gal = mounts.join("sda0/gal-extender/gal_loader.conf");
        std::fs::create_dir_all(helper.parent().unwrap()).unwrap();
        std::fs::create_dir_all(gal.parent().unwrap()).unwrap();
        std::fs::write(&gal, "cluster\n").unwrap();
        assert_eq!(android_auto_base_for(&helper, &mounts).unwrap(), gal);
        let second = mounts.join("sdb0/gal-extender/gal_loader.conf");
        std::fs::create_dir_all(second.parent().unwrap()).unwrap();
        std::fs::write(&second, "sensor\n").unwrap();
        assert!(android_auto_base_for(&helper, &mounts)
            .unwrap_err()
            .contains("multiple GAL configs"));
        std::fs::remove_file(&gal).unwrap();
        assert_eq!(android_auto_base_for(&helper, &mounts).unwrap(), second);
        let adjacent = helper.with_file_name("gal_loader.conf");
        std::fs::write(&adjacent, "input\n").unwrap();
        assert_eq!(android_auto_base_for(&helper, &mounts).unwrap(), adjacent);
        std::fs::remove_file(adjacent).unwrap();
        std::fs::remove_file(second).unwrap();
        assert!(android_auto_base_for(&helper, &mounts)
            .unwrap_err()
            .contains("unavailable"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_names_stay_beside_the_loaded_component() {
        for file in [
            "/fs/sdb0/apps/helper",
            "/fs/usb0/tools/helper",
            "/internal/apps/helper",
        ] {
            let component = Path::new(file);
            assert_eq!(
                beside(component, "gal_loader.conf").unwrap(),
                component.parent().unwrap().join("gal_loader.conf")
            );
        }
        for invalid in ["", "..", "../other.conf", "/other.conf"] {
            assert!(beside(Path::new("/apps/helper"), invalid).is_err());
        }
        assert!(beside(Path::new("relative/helper"), "gal_loader.conf").is_err());
    }

    #[test]
    fn actual_card_requires_live_complete_publisher() {
        let root = std::env::temp_dir().join(format!("settings-location-{}", std::process::id()));
        let card = root.join("custom-card");
        let proc = root.join("proc");
        std::fs::create_dir_all(&card).unwrap();
        std::fs::create_dir_all(proc.join("17")).unwrap();
        let object = root.join("location");
        for invalid in [
            "version::1\npid::17\ndir::relative\nend::1\n".to_string(),
            format!("version::1\npid::18\ndir::{}\nend::1\n", card.display()),
            format!("version::1\npid::0\ndir::{}\nend::1\n", card.display()),
            format!("version::2\npid::17\ndir::{}\nend::1\n", card.display()),
            format!("version::1\npid::17\ndir::{}\n", card.display()),
        ] {
            std::fs::write(&object, invalid).unwrap();
            assert_eq!(live_card_at(&object, &proc), None);
        }
        let text = record(&card, 17).unwrap();
        std::fs::write(
            &object,
            format!("@card\n{}", text.replace("pid::", "[n]pid::")),
        )
        .unwrap();
        assert_eq!(live_card_at(&object, &proc), Some(card.clone()));
        std::fs::remove_dir(proc.join("17")).unwrap();
        assert_eq!(live_card_at(&object, &proc), None);
        assert!(record(Path::new("relative"), 17).is_err());
        assert!(record(&card, 0).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reader_base_requires_fresh_live_complete_file_record() {
        let root = std::env::temp_dir().join(format!("settings-base-{}", std::process::id()));
        let proc = root.join("proc");
        std::fs::create_dir_all(proc.join("17")).unwrap();
        let base = root.join("custom.conf");
        std::fs::write(&base, "cluster\n").unwrap();
        let object = root.join("reader");
        let record = format!(
            "version::1\npid::17\nupdated_ms::6000\nbase::{}\nend::1\n",
            base.display()
        );
        for invalid in [
            record.replace("version::1", "version::2"),
            record.replace("pid::17", "pid::18"),
            record.replace("pid::17", "pid::0"),
            record.replace("updated_ms::6000", "updated_ms::0"),
            record.replace("updated_ms::6000", "updated_ms::999"),
            record.replace("updated_ms::6000", "updated_ms::7001"),
            record.replace("updated_ms::6000", "updated_ms::bad"),
            record.replace("end::1\n", ""),
            record.replace(&format!("base::{}", base.display()), "base::relative"),
            record.replace(&format!("base::{}", base.display()), "base::/missing/base"),
            record.replace(
                &format!("base::{}", base.display()),
                &format!("base::{}", root.display()),
            ),
        ] {
            std::fs::write(&object, invalid).unwrap();
            assert_eq!(live_base_at(&object, &proc, 7000), None);
        }
        std::fs::write(
            &object,
            format!(
                "@settings_active\n{}",
                record.replace("base::", "[n]base::")
            ),
        )
        .unwrap();
        assert_eq!(live_base_at(&object, &proc, 7000), Some(base));
        std::fs::remove_dir(proc.join("17")).unwrap();
        assert_eq!(live_base_at(&object, &proc, 7000), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
