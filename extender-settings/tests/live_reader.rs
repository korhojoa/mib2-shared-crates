// SPDX-License-Identifier: MIT OR Apache-2.0
//! Run the real reader worker against verified store generations in an owned process.
use std::ffi::{c_char, CString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

extern "C" {
    fn extender_settings_init(
        base: *const c_char,
        dir: *const c_char,
        status: *const c_char,
    ) -> i32;
    fn extender_settings_get_int(key: *const c_char, fallback: i32) -> i32;
}

static CALLS: AtomicUsize = AtomicUsize::new(0);

fn wait_for(check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(4);
    while !check() {
        assert!(
            Instant::now() < deadline,
            "reader did not acknowledge the stored generation"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn child(root: &Path) {
    let base = root.join("loader.conf");
    let status = root.join("gal/settings_active");
    std::fs::write(&base, "aa_suppress_navigation_active=0\nmain_dpi=120\n").unwrap();
    let cbase = CString::new(base.to_str().unwrap()).unwrap();
    let cdir = CString::new(root.to_str().unwrap()).unwrap();
    let cstatus = CString::new(status.to_str().unwrap()).unwrap();
    // SAFETY: all three owned strings remain valid for the initialization call.
    assert_eq!(
        unsafe { extender_settings_init(cbase.as_ptr(), cdir.as_ptr(), cstatus.as_ptr()) },
        0
    );
    assert!(extender_settings::install_live_apply(|key, value| {
        assert_eq!(key, "aa_suppress_navigation_active");
        assert_eq!(value, 1);
        CALLS.fetch_add(1, Ordering::SeqCst);
        true
    }));
    let before = extender_settings::store::load(&base, root).unwrap();
    let after = extender_settings::store::commit(
        &base,
        root,
        &before.token(),
        &[
            ("aa_suppress_navigation_active".into(), "1".into()),
            ("main_dpi".into(), "160".into()),
        ],
    )
    .unwrap();
    wait_for(|| CALLS.load(Ordering::SeqCst) == 1);
    wait_for(|| {
        extender_settings::pps::read(&status)
            .unwrap_or_default()
            .contains("applied.aa_suppress_navigation_active::1")
    });
    // SAFETY: the static strings are valid NUL-terminated keys.
    unsafe {
        assert_eq!(
            extender_settings_get_int(c"aa_suppress_navigation_active".as_ptr(), -1),
            1
        );
        assert_eq!(extender_settings_get_int(c"main_dpi".as_ptr(), -1), 120);
    }
    let answer = extender_settings::snapshot_text(&after, root);
    assert!(answer.contains("Suppress phone navigation activity"));
    assert!(answer.contains("Main screen density (dpi)"));
    // The displayed names and choices are the schema values even when the
    // active reader acknowledges a live change and keeps geometry pinned.
    for row in answer.lines().filter(|line| line.starts_with("row\t")) {
        let fields: Vec<_> = row.split('\t').collect();
        let setting = extender_settings::schema::find(fields[1]).unwrap();
        assert_eq!(fields[3], setting.label);
    }
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(CALLS.load(Ordering::SeqCst), 1);
}

#[test]
fn stored_generation_applies_once_with_acknowledgement_and_pinned_geometry() {
    if let Some(root) = std::env::var_os("LIVE_READER_FIXTURE") {
        child(&PathBuf::from(root));
        return;
    }
    let root = std::env::temp_dir().join(format!("live-reader-fixture-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let mut process = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "stored_generation_applies_once_with_acknowledgement_and_pinned_geometry",
            "--nocapture",
        ])
        .env("LIVE_READER_FIXTURE", &root)
        .env("GAL_PPS_ROOT", &root)
        .env_remove("EXTENDER_SETTINGS_PARENT_PID")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    let result = loop {
        if let Some(status) = process.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            process.kill().unwrap();
            break process.wait().unwrap();
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // The reader process has exited; its worker cannot recreate removed files.
    std::fs::remove_dir_all(root).unwrap();
    assert!(result.success());
}
