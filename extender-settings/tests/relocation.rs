// SPDX-License-Identifier: MIT OR Apache-2.0
//! Execute copied helpers from three independent install folders.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn answer(executable: &Path, args: &[&str], cwd: &Path) -> String {
    let mut child = Command::new(executable)
        .args(args)
        .current_dir(cwd)
        .env_remove("LSD_SETTINGS_HMI_PID")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("copied settings helper exceeded the test deadline");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn both_helpers_use_their_loaded_folder_in_three_locations() {
    let root = std::env::temp_dir().join(format!("settings-relocation-{}", std::process::id()));
    let _cleanup = Cleanup(root.clone());
    let elsewhere = root.join("unrelated-working-directory");
    std::fs::create_dir_all(&elsewhere).unwrap();
    // A decoy proves that the helper does not read the current directory.
    std::fs::write(elsewhere.join("gal_loader.conf"), "cluster\nmain_fps=5\n").unwrap();
    std::fs::write(elsewhere.join("dio_loader.conf"), "altscreen_max_fps=5\n").unwrap();
    for (location, fps) in [
        ("fs/sdb0/extenders", 21),
        ("usb/device/extenders", 27),
        ("internal/apps/extenders", 33),
    ] {
        let folder = root.join(location);
        std::fs::create_dir_all(&folder).unwrap();
        let android_auto = folder.join("android-auto-settings");
        let carplay = folder.join("carplay-settings");
        std::fs::copy(env!("CARGO_BIN_EXE_android-auto-settings"), &android_auto).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_carplay-settings"), &carplay).unwrap();
        std::fs::write(
            folder.join("gal_loader.conf"),
            format!("cluster\nmain_fps={fps}\n"),
        )
        .unwrap();
        std::fs::write(
            folder.join("dio_loader.conf"),
            format!("altscreen_max_fps={fps}\n"),
        )
        .unwrap();
        let aa = answer(
            &android_auto,
            &[folder.to_str().unwrap(), "snapshot"],
            &elsewhere,
        );
        assert!(
            aa.lines().any(|line| line.starts_with("row\tmain_fps\t")
                && line.split('\t').nth(5) == Some(fps.to_string().as_str())),
            "{aa}"
        );
        let cp = answer(&carplay, &["auto", "snapshot"], &elsewhere);
        assert!(
            cp.lines()
                .any(|line| line.starts_with("row\taltscreen_max_fps\t")
                    && line.split('\t').nth(5) == Some(fps.to_string().as_str())),
            "{cp}"
        );
        std::fs::remove_file(folder.join("dio_loader.conf")).unwrap();
        let absent = answer(&carplay, &["auto", "snapshot"], &elsewhere);
        assert!(
            absent.starts_with(&format!(
                "absent\t{}\n",
                folder.join("dio_loader.conf").display()
            )),
            "{absent}"
        );
    }
}
