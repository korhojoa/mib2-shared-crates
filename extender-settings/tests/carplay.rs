// SPDX-License-Identifier: MIT OR Apache-2.0
use extender_settings::carplay::{self, Snapshot, BEGIN, END, LIMIT};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    dir: PathBuf,
    file: PathBuf,
}
impl Fixture {
    fn new(text: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "carplay-settings-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let file = dir.join("dio_loader.conf");
        fs::write(&file, text).unwrap();
        Self { dir, file }
    }
    fn save(&self, key: &str, value: &str) -> Result<Snapshot, String> {
        carplay::commit(
            &self.file,
            &carplay::load(&self.file)?.token(),
            &[(key.into(), value.into())],
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.dir).unwrap();
    }
}

#[test]
fn first_key_uses_shared_decimal_integer() {
    for (text, value) in [
        ("x=0x10", 0),
        ("x=-020 # decimal", -20),
        ("x=08", 8),
        ("x=  +19tail", 19),
        ("x=bad\nx=3", 7),
        ("x =5\nx=9", 5),
        ("#x=12\r\n\tx=17\r", 17),
        ("x=4294967295", i32::MAX),
        ("x=-4294967295", i32::MIN),
        ("x=0xjunk", 0),
    ] {
        assert_eq!(carplay::base_int(text, "x", 7), value, "{text}");
    }
    assert_eq!(
        carplay::base_int(&format!("x={}1", " ".repeat(63)), "x", 7),
        1
    );
}

#[test]
fn saves_survive_reopen_and_default_restores_exact_original() {
    let original = "# original\r\ncluster\nunknown=keep # exact\naltscreen_max_fps=025\nurl=maps:/car/instrumentcluster/map\n";
    let f = Fixture::new(original);
    let saved = f.save("altscreen_max_fps", "30").unwrap();
    assert_eq!(saved.base, original);
    assert_eq!(
        carplay::base_int(
            &carplay::load(&f.file).unwrap().text,
            "altscreen_max_fps",
            0
        ),
        30
    );
    assert_eq!(
        fs::read_to_string(f.file.with_extension("lsd-backup")).unwrap(),
        original
    );
    assert!(saved.protocol().contains("\t30\t25\t1\t60\t1\t0\t0\n"));
    f.save("altscreen_max_fps", "inherit").unwrap();
    assert_eq!(fs::read_to_string(&f.file).unwrap(), original);
}

#[test]
fn stale_drafts_external_edits_and_invalid_transactions_do_not_write() {
    let f = Fixture::new("cluster_enable=0\n");
    let old = carplay::load(&f.file).unwrap().token();
    f.save("cluster_enable", "1").unwrap();
    let previous = fs::read(&f.file).unwrap();
    assert!(carplay::commit(&f.file, &old, &[("cluster_enable".into(), "0".into())]).is_err());
    assert!(f.save("altscreen_width", "0").is_err());
    assert!(f.save("other", "2").is_err());
    assert_eq!(fs::read(&f.file).unwrap(), previous);
    let token = carplay::load(&f.file).unwrap().token();
    fs::write(&f.file, "cluster_enable=0\n# external\n").unwrap();
    assert!(carplay::commit(&f.file, &token, &[("cluster_enable".into(), "1".into())]).is_err());
}

#[test]
fn rejects_malformed_sections_and_native_buffer_overflow() {
    for text in [
        format!("{BEGIN}cluster_enable=1\n"),
        format!("{BEGIN}x=1\nx=2\n{END}"),
        format!("# extra\n{BEGIN}{END}"),
        "# lsd-extender CarPlay overrides v2\n".into(),
        "x=1\0".into(),
    ] {
        assert!(Snapshot::parse(text).is_err());
    }
    let f = Fixture::new(&format!("#{}", "x".repeat(LIMIT - 1)));
    assert!(f.save("cluster_enable", "1").is_err());
    assert_eq!(fs::metadata(&f.file).unwrap().len(), LIMIT as u64);
}

#[test]
fn preserves_unknown_overrides_and_validates_the_whole_transaction() {
    let f = Fixture::new(&format!("{BEGIN}future_key=42\n{END}cluster_enable=0\n"));
    let before = carplay::load(&f.file).unwrap();
    assert!(carplay::commit(
        &f.file,
        &before.token(),
        &[
            ("cluster_enable".into(), "1".into()),
            ("cluster_enable".into(), "0".into())
        ]
    )
    .is_err());
    assert_eq!(carplay::load(&f.file).unwrap().text, before.text);
    assert_eq!(
        f.save("cluster_enable", "1")
            .unwrap()
            .overrides
            .get("future_key"),
        Some(&42)
    );
}

fn view_row(s: &Snapshot) -> Vec<String> {
    let p = s.protocol();
    let line = p
        .lines()
        .find(|l| l.starts_with("row\tcluster_view\t"))
        .unwrap();
    line.split('\t').map(str::to_owned).collect()
}

#[test]
fn cluster_view_writes_its_key_set_and_reads_back() {
    let f = Fixture::new("altscreen\ncluster\nclustervideo\n");
    // No explicit output and no direct writer: preserve the unclassified native state.
    let s = carplay::load(&f.file).unwrap();
    assert_eq!(
        view_row(&s)[3],
        "Cluster map|Off|Instrument cluster|Head unit"
    );
    assert_eq!(view_row(&s)[5..8], ["3", "3", "0"]);
    for (view, set) in [("1", [1, 1]), ("2", [0, 2]), ("0", [0, 0])] {
        let s = f.save("cluster_view", view).unwrap();
        for (k, v) in carplay::VIEW_KEYS.iter().zip(set) {
            assert_eq!(carplay::base_int(&s.text, k, -9), v, "{view} {k}");
        }
        assert_eq!(view_row(&s)[5], view);
        assert_eq!(view_row(&s)[10], "0", "not inherited after a save");
    }
    let s = f.save("cluster_view", "inherit").unwrap();
    assert_eq!(s.text, "altscreen\ncluster\nclustervideo\n");
    assert_eq!(view_row(&s)[10], "1");
}

#[test]
fn cluster_view_needs_the_feature_lines_and_is_one_transaction() {
    let f = Fixture::new("altscreen\ncluster\n");
    assert!(f.save("cluster_view", "1").is_ok());
    let e = f.save("cluster_view", "2").err().unwrap();
    assert!(e.contains("cluster video module"), "{e}");
    assert!(f.save("cluster_view", "3").is_err());
    let s = carplay::load(&f.file).unwrap();
    assert!(carplay::commit(
        &f.file,
        &s.token(),
        &[
            ("cluster_view".into(), "0".into()),
            ("cluster_enable".into(), "1".into())
        ]
    )
    .is_err());
}

#[test]
fn output_selection_preserves_unknown_configuration() {
    for (text, view) in [
        ("cluster_output=0\ncluster_enable=1\n", 0),
        ("cluster_output=1\ncluster_enable=1\n", 1),
        ("cluster_output=2\ncluster_enable=1\n", 2),
        ("cluster_enable=1\n", 1),
        ("", 3),
        ("unknown_saved_choice=1\ncluster_output=2\n", 2),
    ] {
        assert_eq!(carplay::view_of(text), view, "{text}");
    }
    let f = Fixture::new("altscreen\ncluster\nclustervideo\n");
    assert!(f.save("unknown_saved_choice", "1").is_err());
    let f = Fixture::new("altscreen\ncluster\nclustervideo\nunknown_saved_choice=7\n");
    f.save("cluster_view", "0").unwrap();
    assert!(fs::read_to_string(&f.file)
        .unwrap()
        .contains("unknown_saved_choice=7\n"));
}

#[test]
fn default_geometry_and_maps_offer_follow_native_policy() {
    let original = "altscreen\ncluster\nclustervideo\n";
    let f = Fixture::new(original);
    for key in ["altscreen_override_geometry", "altscreen_suggest_urls"] {
        let descriptor = carplay::SETTINGS.iter().find(|row| row.key == key).unwrap();
        assert_eq!(descriptor.default, 1, "{key}");
        let empty = carplay::load(&f.file).unwrap().protocol();
        let row: Vec<&str> = empty
            .lines()
            .find(|line| line.starts_with(&format!("row\t{key}\t")))
            .unwrap()
            .split('\t')
            .collect();
        assert_eq!(&row[5..7], ["1", "1"], "{key}");
        let off = f.save(key, "0").unwrap().protocol();
        let row: Vec<&str> = off
            .lines()
            .find(|line| line.starts_with(&format!("row\t{key}\t")))
            .unwrap()
            .split('\t')
            .collect();
        assert_eq!(&row[5..7], ["0", "1"], "{key}");
        f.save(key, "inherit").unwrap();
        assert_eq!(fs::read_to_string(&f.file).unwrap(), original);
    }
}

#[test]
fn custom_size_needs_a_physical_size() {
    let f = Fixture::new("altscreen_width_mm=0\n");
    let e = f.save("altscreen_override_geometry", "1").err().unwrap();
    assert!(e.contains("mm"), "{e}");
    assert!(f.save("altscreen_max_fps", "25").is_err());
    f.save("altscreen_override_geometry", "0").unwrap();
    assert!(f.save("altscreen_max_fps", "25").is_ok());
}

#[cfg(unix)]
#[test]
fn refuses_readonly_config_and_links_and_preserves_file_mode() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let f = Fixture::new("cluster_enable=0\n");
    fs::set_permissions(&f.file, fs::Permissions::from_mode(0o640)).unwrap();
    f.save("cluster_enable", "1").unwrap();
    assert_eq!(
        fs::metadata(&f.file).unwrap().permissions().mode() & 0o777,
        0o640
    );
    fs::set_permissions(&f.file, fs::Permissions::from_mode(0o440)).unwrap();
    assert!(f.save("cluster_enable", "0").is_err());
    let link = f.dir.join("link.conf");
    symlink(&f.file, &link).unwrap();
    assert!(carplay::load(&link).is_err());
}

#[test]
fn missing_config_is_absent_not_an_error() {
    let f = Fixture::new("cluster_enable=0\n");
    let missing = f.dir.join("none.conf");
    assert!(carplay::load_present(&missing).unwrap().is_none());
    // Saving still needs an existing config.
    assert!(carplay::load(&missing).is_err());
    assert!(carplay::load_present(&f.file).unwrap().is_some());
    assert_eq!(
        carplay::absent_protocol(&carplay::absent_text("auto", &missing)),
        format!("absent\t{}\nend\t1\n", missing.display())
    );
    assert_eq!(
        carplay::absent_protocol(&carplay::absent_text("/x/a\tb.conf", &missing)),
        format!("absent\t{}\nend\t1\n", missing.display())
    );
}

#[cfg(unix)]
#[test]
fn a_link_or_directory_is_still_an_error() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new("cluster_enable=0\n");
    let link = f.dir.join("link.conf");
    symlink(&f.file, &link).unwrap();
    assert!(carplay::load_present(&link).is_err());
    let dangling = f.dir.join("dangling.conf");
    symlink(f.dir.join("gone"), &dangling).unwrap();
    assert!(carplay::load_present(&dangling).is_err());
    assert!(carplay::load_present(&f.dir).is_err());
}

#[test]
fn fixed_native_severity_is_not_an_editable_setting() {
    let original = "# Existing opaque user value\nlog_level=6\ncluster\n";
    let f = Fixture::new(original);
    assert!(!carplay::SETTINGS.iter().any(|row| row.key == "log_level"));
    for value in ["0", "7", "8", "inherit"] {
        assert!(f.save("log_level", value).is_err());
        assert_eq!(fs::read_to_string(&f.file).unwrap(), original);
        assert!(!f.file.with_extension("lsd-backup").exists());
    }
    let saved = f.save("altscreen_max_fps", "35").unwrap();
    assert_eq!(saved.base, original);
    assert!(saved.text.contains("log_level=6\n"));
    assert!(!saved.protocol().contains("row\tlog_level\t"));
    f.save("altscreen_max_fps", "inherit").unwrap();
    assert_eq!(fs::read_to_string(&f.file).unwrap(), original);
}

#[test]
fn driver_protocol_hides_diagnostics_without_dropping_saved_keys() {
    let original =
        format!("{BEGIN}altscreen_decrypt=0\nprobe_verbose=1\nfuture_key=73\n{END}cluster\n");
    let f = Fixture::new(&original);
    let saved = f.save("altscreen_max_fps", "35").unwrap();
    for (key, value) in [
        ("altscreen_decrypt", 0),
        ("probe_verbose", 1),
        ("future_key", 73),
    ] {
        assert_eq!(saved.overrides.get(key), Some(&value));
        assert!(!saved.protocol().contains(&format!("row\t{key}\t")));
    }
    assert!(!saved.protocol().contains("\tDiagnostics\t"));
    let restored = f.save("altscreen_max_fps", "inherit").unwrap();
    let previous = Snapshot::parse(original).unwrap();
    assert_eq!(restored.base, previous.base);
    assert_eq!(restored.overrides, previous.overrides);
    assert_eq!(view_row(&saved)[2], "Displays");
    assert_eq!(
        extender_settings::schema::find("feature.cluster")
            .unwrap()
            .label,
        view_row(&saved)[3].split('|').next().unwrap()
    );
    assert_eq!(
        extender_settings::schema::find("feature.cluster")
            .unwrap()
            .group,
        view_row(&saved)[2]
    );
}
