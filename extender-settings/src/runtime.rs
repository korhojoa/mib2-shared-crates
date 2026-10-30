// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{config::Snapshot, schema, store, wall_ms, ABI};
use std::collections::BTreeMap;
use std::ffi::{c_char, CStr};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

struct Session {
    startup: Snapshot,
    refused: BTreeMap<String, (String, i32)>,
    active: Snapshot,
    live: Snapshot,
    loaded: BTreeMap<String, i32>,
    error: String,
}
static SESSION: Mutex<Option<Session>> = Mutex::new(None);

/// Apply one value on the settings reader thread. The callback must not call
/// the settings reader. Return false when the consumer cannot apply it.
pub type LiveApply = fn(&str, i32) -> bool;
static LIVE_APPLY: OnceLock<LiveApply> = OnceLock::new();

/// Install the loaded component's live consumer after feature initialization.
/// The existing reader loop is the only settings refresh worker.
pub fn install_live_apply(apply: LiveApply) -> bool {
    LIVE_APPLY.set(apply).is_ok()
}

fn capable(session: &Session, key: &str) -> bool {
    crate::live::eligible(&session.startup, key)
        && crate::live::module(key).is_none_or(|module| session.loaded.get(module) == Some(&1))
}

fn refresh(session: &mut Session, next: Snapshot, apply: LiveApply) {
    let token = next.token();
    for setting in schema::SETTINGS {
        let key = setting.key;
        let value = next.get(key, setting.default);
        if capable(session, key) && session.active.get(key, setting.default) != value {
            if session.refused.get(key) == Some(&(token.clone(), value)) {
                continue;
            }
            if apply(key, value) {
                session.active.values.insert(key.into(), value);
                session.refused.remove(key);
            } else {
                session.refused.insert(key.into(), (token.clone(), value));
            }
        }
    }
    session.live = next;
}

unsafe fn string<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        None
    } else {
        CStr::from_ptr(p).to_str().ok()
    }
}

/// The status publisher: `key=value` lines in, one PPS write out, the keys
/// of the previous write (the first time: of the object a previous gal
/// process left) deleted in the same write when absent now.
struct StatusWriter {
    path: PathBuf,
    keys: Option<Vec<String>>,
}

impl StatusWriter {
    fn publish(&mut self, data: &str) -> Result<(), String> {
        let previous = match self.keys.take() {
            Some(k) => k,
            None => crate::pps::existing_keys(&self.path),
        };
        let (text, keys) = crate::pps::render(data, &previous);
        let r = crate::pps::write(&self.path, &text);
        self.keys = Some(if r.is_ok() { keys } else { previous });
        r
    }
}

/// Return the settings ABI version. This function has no initialization requirement.
#[no_mangle]
pub extern "C" fn extender_settings_abi() -> u32 {
    ABI
}

/// Initialize pinned settings. A null status path disables the optional publisher.
/// Return zero on success and -1 on invalid input or initialization failure.
///
/// # Safety
/// Non-null arguments must point to valid NUL-terminated UTF-8 strings during this call.
/// `base` and `dir` must be non-null. `status` must name a PPS object when supplied.
#[no_mangle]
pub unsafe extern "C" fn extender_settings_init(
    base: *const c_char,
    dir: *const c_char,
    status: *const c_char,
) -> i32 {
    let (Some(base), Some(dir)) = (string(base), string(dir)) else {
        return -1;
    };
    let (base, mut dir, status) = (
        PathBuf::from(base),
        PathBuf::from(dir),
        string(status).map(PathBuf::from),
    );
    let (active, error) = match store::load(&base, &dir) {
        Ok(s) => (s, String::new()),
        Err(e) => match store::read_bounded(&base) {
            Ok(text) => (
                Snapshot {
                    base: text,
                    values: BTreeMap::new(),
                    revision: 0,
                },
                e,
            ),
            Err(_) => return -1,
        },
    };
    let Ok(mut guard) = SESSION.lock() else {
        return -1;
    };
    if guard.is_some() {
        return -1;
    }
    *guard = Some(Session {
        startup: active.clone(),
        refused: BTreeMap::new(),
        live: active.clone(),
        active,
        loaded: BTreeMap::new(),
        error,
    });
    drop(guard);
    let Some(status) = status else {
        return 0;
    };
    let active_dir = dir.clone();
    let mut writer = StatusWriter {
        path: status,
        keys: None,
    };
    let spawned = std::thread::Builder::new()
        .name("settings-reader".into())
        .spawn(move || loop {
            if let Some(card) = crate::location::live_card() {
                dir = crate::settings_dir_with_runtime(&base, Some(&card));
            }
            let fresh = store::load(&base, &dir);
            let data = {
                let Ok(mut guard) = SESSION.lock() else {
                    return;
                };
                let Some(s) = guard.as_mut() else {
                    return;
                };
                match fresh {
                    Ok(next) => {
                        s.error.clear();
                        if let Some(apply) = LIVE_APPLY.get() {
                            refresh(s, next, *apply);
                        } else {
                            s.live = next;
                        }
                    }
                    Err(error) => s.error = error,
                }
                let applied_token = if dir == active_dir && s.active.same_startup_settings(&s.live)
                {
                    s.live.token()
                } else {
                    format!("pending-{}", s.active.token())
                };
                let mut text = status_header(&applied_token, &dir, &base, &s.error);
                for (name, state) in &s.loaded {
                    text.push_str(&format!("loaded.{name}={state}\n"));
                }
                for setting in schema::SETTINGS {
                    if LIVE_APPLY.get().is_some() && capable(s, setting.key) {
                        text.push_str(&format!("live.{}=1\n", setting.key));
                        text.push_str(&format!(
                            "applied.{}={}\n",
                            setting.key,
                            s.active.get(setting.key, setting.default)
                        ));
                    }
                }
                text.push_str("end=1\n");
                text
            };
            let _ = writer.publish(&data);
            std::thread::sleep(Duration::from_secs(1));
        });
    if spawned.is_err() {
        if let Ok(mut guard) = SESSION.lock() {
            *guard = None;
        }
        return -1;
    }
    0
}

fn status_header(token: &str, dir: &Path, base: &Path, error: &str) -> String {
    format!(
        "version=1\npid={}\nupdated_ms={}\ntoken={}\ndir={}\nbase={}\nerror={}\n",
        std::process::id(),
        wall_ms(),
        token,
        clean(&dir.to_string_lossy()),
        clean(&base.to_string_lossy()),
        clean(error)
    )
}

pub fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Read a pinned integer, or return the supplied default when unavailable.
///
/// # Safety
/// A non-null string argument must remain valid and NUL-terminated during this call.
#[no_mangle]
pub unsafe extern "C" fn extender_settings_get_int(key: *const c_char, default: i32) -> i32 {
    let Some(key) = string(key) else {
        return default;
    };
    let Ok(guard) = SESSION.lock() else {
        return default;
    };
    let Some(s) = guard.as_ref() else {
        return default;
    };
    s.active.get(key, default)
}

/// Return one when the named feature is enabled, otherwise zero.
///
/// # Safety
/// A non-null string argument must remain valid and NUL-terminated during this call.
#[no_mangle]
pub unsafe extern "C" fn extender_settings_feature(name: *const c_char) -> i32 {
    let Some(name) = string(name) else {
        return 0;
    };
    let Ok(guard) = SESSION.lock() else {
        return 0;
    };
    guard
        .as_ref()
        .map_or(0, |s| i32::from(s.active.feature(name)))
}

/// Record the load state of a supported feature for the optional status publisher.
///
/// # Safety
/// A non-null string argument must remain valid and NUL-terminated during this call.
#[no_mangle]
pub unsafe extern "C" fn extender_settings_loaded(name: *const c_char, state: i32) {
    let Some(name) = string(name) else {
        return;
    };
    if !schema::FEATURES.contains(&name) {
        return;
    }
    if let Ok(mut guard) = SESSION.lock() {
        if let Some(s) = guard.as_mut() {
            s.loaded.insert(name.into(), state);
        }
    }
}

pub fn application_status(snapshot: &Snapshot, dir: &Path, status: &Path) -> String {
    let text = match crate::pps::read(status) {
        Ok(text) => text,
        Err(e) => return format!("Saved for next AA start; no active status ({e})"),
    };
    // the PPS object: one atomic write per second, so `end` is a format
    // marker now, not a torn-read check
    let fields: BTreeMap<_, _> = crate::pps::attrs(&text).collect();
    let updated = fields
        .get("updated_ms")
        .and_then(|v| v.parse::<u128>().ok())
        .unwrap_or(0);
    let age = wall_ms().checked_sub(updated);
    if fields.get("end") != Some(&"1")
        || fields.get("version") != Some(&"1")
        || age.is_none_or(|v| v > 5000)
    {
        return "Saved for next AA start; active status unavailable".into();
    }
    if fields.get("dir").copied() != dir.to_str() {
        return "Settings directory mismatch".into();
    }
    if let Some(error) = fields.get("error").filter(|v| !v.is_empty()) {
        return format!("AA settings error: {error}");
    }
    if fields.get("token").copied() != Some(snapshot.token().as_str()) {
        if fields.keys().any(|key| key.starts_with("live.")) {
            return "Saved; live controls update now, other settings next AA start".into();
        }
        return "Saved; applies on next AA start".into();
    }
    for name in schema::FEATURES {
        if snapshot.feature(name) && fields.get(format!("loaded.{name}").as_str()) != Some(&"1") {
            return format!("Saved; waiting for feature {name}");
        }
    }
    "Applied in current AA process".into()
}

pub fn render(snapshot: &Snapshot, dir: &Path, status: &Path) -> String {
    // Reset removes the user override. Its preview must include deployment
    // values and enabled features, rather than only compiled fallbacks.
    let defaults = Snapshot {
        base: snapshot.base.clone(),
        values: BTreeMap::new(),
        revision: snapshot.revision,
    };
    let mut text = format!(
        "meta\t1\t{}\t{}\n",
        snapshot.token(),
        clean(&application_status(snapshot, dir, status))
    );
    for s in schema::SETTINGS.iter().filter(|s| schema::visible(s.key)) {
        let value = snapshot.get(s.key, s.default);
        let inherited = if s.key == "ev_routing" {
            crate::config::EV_KEYS
                .iter()
                .all(|key| !snapshot.values.contains_key(*key))
        } else {
            !snapshot.values.contains_key(s.key)
        };
        text.push_str(&format!(
            "row\t{}\t{}\t{}\t{}\t{value}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            s.key,
            s.group,
            s.label,
            if s.boolean { "bool" } else { "int" },
            defaults.get(s.key, s.default),
            s.min,
            s.max,
            s.step,
            i32::from(inherited),
            i32::from(s.readonly)
        ));
    }
    text.push_str("end\t1\n");
    text
}

#[cfg(test)]
mod live_tests {
    use super::*;

    fn session() -> Session {
        let startup = Snapshot {
            base: "cluster\nsensor\nsensor_ev_advertise=1\nsensor_send_compass=1\n".into(),
            values: BTreeMap::new(),
            revision: 0,
        };
        Session {
            refused: BTreeMap::new(),
            startup: startup.clone(),
            active: startup.clone(),
            live: startup,
            loaded: BTreeMap::from([("cluster".into(), 1), ("sensor".into(), 1)]),
            error: String::new(),
        }
    }

    #[test]
    fn only_accepted_runtime_values_change_and_geometry_stays_pinned() {
        let mut session = session();
        let next = session
            .active
            .changed(&[
                ("cluster_write_pace_pct".into(), "75".into()),
                ("main_dpi".into(), "160".into()),
                ("sensor_ev_capacity_wh".into(), "42000".into()),
            ])
            .unwrap();
        refresh(&mut session, next, |key, _| key != "sensor_ev_capacity_wh");
        assert_eq!(session.active.get("cluster_write_pace_pct", 100), 75);
        assert_eq!(session.active.get("main_dpi", 0), 0);
        assert_eq!(session.active.get("sensor_ev_capacity_wh", 30000), 30000);
        assert_eq!(session.live.get("main_dpi", 0), 160);
    }

    #[test]
    fn an_offered_sensor_can_stop_and_resume_but_an_unoffered_one_waits() {
        let mut session = session();
        let next = session
            .live
            .changed(&[
                ("sensor_send_compass".into(), "0".into()),
                ("sensor_send_rpm".into(), "1".into()),
            ])
            .unwrap();
        refresh(&mut session, next, |_, _| true);
        assert_eq!(session.active.get("sensor_send_compass", 1), 0);
        assert_eq!(session.active.get("sensor_send_rpm", 0), 0);
        let next = session
            .live
            .changed(&[("sensor_send_compass".into(), "1".into())])
            .unwrap();
        refresh(&mut session, next, |_, _| true);
        assert_eq!(session.active.get("sensor_send_compass", 0), 1);
    }

    #[test]
    fn missing_modules_do_not_acknowledge_and_repeated_snapshots_do_not_apply_twice() {
        let mut session = session();
        session.loaded.insert("cluster".into(), 0);
        let next = session
            .live
            .changed(&[("cluster_write_pace_pct".into(), "75".into())])
            .unwrap();
        refresh(&mut session, next.clone(), |_, _| panic!("unloaded module"));
        session.loaded.insert("cluster".into(), 1);
        refresh(&mut session, next.clone(), |_, _| true);
        refresh(&mut session, next, |_, _| panic!("unchanged value"));
    }

    #[test]
    fn refused_generation_is_not_retried_until_the_saved_generation_changes() {
        let mut session = session();
        let next = session
            .live
            .changed(&[("cluster_write_pace_pct".into(), "75".into())])
            .unwrap();
        refresh(&mut session, next.clone(), |_, _| false);
        refresh(&mut session, next, |_, _| panic!("same refused generation"));
        assert_eq!(session.active.get("cluster_write_pace_pct", 100), 100);
        let next = session
            .live
            .changed(&[("cluster_write_pace_pct".into(), "80".into())])
            .unwrap();
        refresh(&mut session, next, |_, _| true);
        assert_eq!(session.active.get("cluster_write_pace_pct", 100), 80);
    }
}

pub fn snapshot_text(snapshot: &Snapshot, dir: &Path) -> String {
    render(
        snapshot,
        dir,
        &crate::location::pps_root().join("gal/settings_active"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_without_a_lease_never_claims_applied() {
        let s = Snapshot {
            base: "cluster\n".into(),
            values: BTreeMap::new(),
            revision: 0,
        };
        let status = application_status(&s, Path::new("/missing"), Path::new("/missing/no-status"));
        assert!(!status.starts_with("Applied"));
        let output = render(&s, Path::new("/missing"), Path::new("/missing/no-status"));
        assert!(output.ends_with("end\t1\n"));
        for row in output.lines().filter(|line| line.starts_with("row\t")) {
            let fields: Vec<_> = row.split('\t').collect();
            assert_eq!(fields[3], schema::find(fields[1]).unwrap().label);
        }
        assert!(output.contains("row\tfeature.cluster\tDisplays\tCluster map\tbool\t1\t"));
    }

    #[test]
    fn active_status_requires_fresh_matching_revision_and_loaded_features() {
        let path =
            std::env::temp_dir().join(format!("extender-settings-status-{}", std::process::id()));
        let dir = Path::new("/card");
        let s = Snapshot {
            base: "cluster\n".into(),
            values: BTreeMap::new(),
            revision: 1,
        };
        // what a read of the PPS object gives: the @name line, then attributes
        let record = |updated, token: &str, loaded| {
            format!(
            "@settings_active\nversion::1\npid::1\nupdated_ms::{updated}\ntoken::{token}\ndir::/card\nerror::\nloaded.cluster::{loaded}\nend::1\n")
        };
        std::fs::write(&path, record(wall_ms(), &s.token(), 1)).unwrap();
        assert!(application_status(&s, dir, &path).starts_with("Applied"));
        std::fs::write(&path, record(1, &s.token(), 1)).unwrap();
        assert!(!application_status(&s, dir, &path).starts_with("Applied"));
        std::fs::write(&path, record(wall_ms(), "old-token", 1)).unwrap();
        assert!(application_status(&s, dir, &path).contains("next AA"));
        std::fs::write(&path, record(wall_ms(), &s.token(), -1)).unwrap();
        assert!(application_status(&s, dir, &path).contains("waiting for feature"));
        assert_eq!(
            application_status(&s, Path::new("/other"), &path),
            "Settings directory mismatch"
        );
        // the old key=value file format is not a status any more
        std::fs::write(&path, "version=1\nupdated_ms=1\nend=1\n").unwrap();
        assert!(application_status(&s, dir, &path).contains("unavailable"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn the_writer_deletes_what_a_previous_process_left() {
        let dir =
            std::env::temp_dir().join(format!("extender-settings-writer-{}", std::process::id()));
        let path = dir.join("gal/settings_active");
        crate::pps::write(&path, "version::1\npid::9\nloaded.sensor::1\nend::1\n").unwrap();
        let mut w = StatusWriter {
            path: path.clone(),
            keys: None,
        };
        w.publish("version=1\npid=10\nend=1\n").unwrap();
        assert_eq!(
            w.keys.as_deref(),
            Some(&["version".to_string(), "pid".into(), "end".into()][..])
        );
        // on a plain file the write lands at offset 0 and never truncates
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("version::1\npid::10\nend::1\n-loaded.sensor\n"),
            "{text}"
        );
        w.publish("version=1\npid=10\nloaded.cluster=1\nend=1\n")
            .unwrap();
        assert_eq!(w.keys.as_ref().unwrap().len(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod null_status_contract {

    use super::*;
    use std::ffi::CString;

    #[test]
    fn null_reserved_status_initializes_without_a_publisher() {
        let root = std::env::temp_dir().join(format!("settings-reader-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let base = root.join("loader.conf");
        std::fs::write(&base, "cluster\nmain_dpi=160\n").unwrap();
        let cbase = CString::new(base.to_str().unwrap()).unwrap();
        let cdir = CString::new(root.to_str().unwrap()).unwrap();
        let key = CString::new("main_dpi").unwrap();
        let feature = CString::new("cluster").unwrap();
        unsafe {
            assert_eq!(
                extender_settings_init(cbase.as_ptr(), cdir.as_ptr(), std::ptr::null()),
                0
            );
            assert_eq!(extender_settings_get_int(key.as_ptr(), 0), 160);
            assert_eq!(extender_settings_feature(feature.as_ptr()), 1);
            assert_eq!(
                extender_settings_init(cbase.as_ptr(), cdir.as_ptr(), std::ptr::null()),
                -1
            );
        }
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        *SESSION.lock().unwrap() = None;
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn status_paths_cannot_add_pps_fields() {
        let text = status_header(
            "token",
            Path::new("dir\nend=1"),
            Path::new("base\r\nloaded.sensor=1"),
            "error\ntext",
        );
        assert_eq!(text.lines().count(), 7);
        assert!(!text
            .lines()
            .any(|line| line == "end=1" || line == "loaded.sensor=1"));
        let valid = status_header("token", Path::new("settings"), Path::new("loader.conf"), "");
        assert!(valid.contains("\ndir=settings\nbase=loader.conf\nerror=\n"));
    }
}
