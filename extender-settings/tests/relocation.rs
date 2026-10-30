// SPDX-License-Identifier: MIT OR Apache-2.0
//! Execute copied helpers from three independent install folders.
use std::io::Read;
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
        .env_remove("EXTENDER_SETTINGS_PARENT_PID")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Drain both pipes while the helper runs. A complete descriptor answer
    // can exceed a pipe buffer; waiting first prevents that helper from exiting.
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let read = |pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.take(131073).read_to_end(&mut bytes).unwrap();
            bytes
        })
    };
    let out = read(Box::new(stdout));
    let err = read(Box::new(stderr));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            out.join().unwrap();
            err.join().unwrap();
            panic!("copied settings helper exceeded the test deadline");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let status = child.wait().unwrap();
    let stdout = out.join().unwrap();
    let stderr = err.join().unwrap();
    assert!(stdout.len() <= 131072 && stderr.len() <= 131072);
    assert!(status.success(), "{}", String::from_utf8_lossy(&stderr));
    String::from_utf8(stdout).unwrap()
}

#[test]
fn large_output_child() {
    print!("{}", "x".repeat(32768));
}

#[test]
fn replies_larger_than_a_pipe_buffer_are_drained_before_waiting() {
    let output = answer(
        &std::env::current_exe().unwrap(),
        &["--exact", "large_output_child", "--nocapture"],
        Path::new("/"),
    );
    assert!(output.contains(&"x".repeat(32768)));
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
