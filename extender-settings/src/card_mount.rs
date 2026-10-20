// SPDX-License-Identifier: MIT OR Apache-2.0
//! Makes a read-only card writable for the length of one save.
//!
//! The unit mounts the SD cards read-only. A save that fails with EROFS asks
//! for a remount of the card that holds the file, retries once, and attempts
//! read-only restoration. Each remount has a bounded operation and owned-child
//! retirement phase; a failed or unproved operation returns an error.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// A mount call that does not finish in this time enters bounded retirement.
const TIMEOUT: Duration = Duration::from_millis(5000);
const RETIRE_TIMEOUT: Duration = Duration::from_millis(1000);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// EROFS as std formats it in an io::Error message.
const READ_ONLY: &str = "os error 30";

/// The card mount point that holds `path`: the ancestor directly under /fs
/// Return `None` when the path is outside that mount namespace.
pub fn mount_point(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|a| a.parent() == Some(Path::new("/fs")))
        .map(Path::to_path_buf)
}

/// Runs `save`; when it fails because the card holding `path` is read-only,
/// remounts that card read-write, runs `save` once more and remounts it
/// read-only. The read-only remount runs whatever the second attempt returns.
pub fn with_writable<T>(path: &Path, save: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    with_writable_using(path, save, remount)
}

fn with_writable_using<T>(
    path: &Path,
    mut save: impl FnMut() -> Result<T, String>,
    mut mount: impl FnMut(&Path, bool) -> Result<(), String>,
) -> Result<T, String> {
    match save() {
        Err(e) if e.contains(READ_ONLY) => {
            let Some(card) = mount_point(path) else {
                return Err(e);
            };
            mount(&card, true).map_err(|m| format!("{e}; {m}"))?;
            let result = save();
            let restored = mount(&card, false);
            match (result, restored) {
                (Ok(v), Ok(())) => Ok(v),
                (Ok(_), Err(m)) => Err(format!("saved, but {m}")),
                (Err(e), Ok(())) => Err(e),
                (Err(e), Err(m)) => Err(format!("{e}; {m}")),
            }
        }
        other => other,
    }
}

fn remount(card: &Path, writable: bool) -> Result<(), String> {
    let mode = if writable { "-uw" } else { "-ur" };
    let what = format!(
        "remount of {} {}",
        card.display(),
        if writable { "read-write" } else { "read-only" }
    );
    // A constant shell program with the mount point as an argv element, so a
    // path never becomes shell syntax. The HMI's LD_PRELOAD is not for mount.
    let mut child = Command::new("/bin/sh")
        .args(["-c", "unset LD_PRELOAD; exec mount \"$1\" \"$2\""])
        .arg("card-mount")
        .arg(mode)
        .arg(card)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{what} failed: {e}"))?;
    let began = Instant::now();
    await_mount_child(
        &mut child,
        &what,
        || began.elapsed(),
        || std::thread::sleep(POLL_INTERVAL),
    )
}

trait MountChild {
    fn poll(&mut self) -> io::Result<Option<ExitStatus>>;
    fn kill(&mut self) -> io::Result<()>;
}

impl MountChild for Child {
    fn poll(&mut self) -> io::Result<Option<ExitStatus>> {
        self.try_wait()
    }

    fn kill(&mut self) -> io::Result<()> {
        Child::kill(self)
    }
}

fn await_mount_child(
    child: &mut impl MountChild,
    what: &str,
    mut now: impl FnMut() -> Duration,
    mut pause: impl FnMut(),
) -> Result<(), String> {
    let began = now();
    let error = loop {
        match child.poll() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("{what} failed: {status}")),
            Ok(None) if now().saturating_sub(began) >= TIMEOUT => {
                break format!("{what} did not finish");
            }
            Ok(None) => pause(),
            Err(e) => break format!("{what} failed: {e}"),
        }
    };
    // The Child belongs to this exact spawn. Polling also reaps a terminal
    // child; never call unbounded wait after a kill or polling failure.
    let kill_error = child.kill().err();
    let began = now();
    let mut poll_error = None;
    loop {
        match child.poll() {
            Ok(Some(_)) => return Err(error),
            Ok(None) => {}
            Err(e) => poll_error = Some(e),
        }
        if now().saturating_sub(began) >= RETIRE_TIMEOUT {
            let mut message = format!("{error}; child termination NOT_PROVEN");
            if let Some(e) = kill_error {
                message.push_str(&format!("; kill failed: {e}"));
            }
            if let Some(e) = poll_error {
                message.push_str(&format!("; poll failed: {e}"));
            }
            return Err(message);
        }
        pause();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_is_the_ancestor_under_fs() {
        assert_eq!(
            mount_point(Path::new("/fs/sdb0/lsd-extender")),
            Some(PathBuf::from("/fs/sdb0"))
        );
        assert_eq!(
            mount_point(Path::new("/fs/sda0/dio-extender/dio_loader.conf")),
            Some(PathBuf::from("/fs/sda0"))
        );
        assert_eq!(mount_point(Path::new("/navigation/gal_loader.conf")), None);
        assert_eq!(mount_point(Path::new("/fs")), None);
    }

    #[test]
    fn other_errors_and_paths_off_card_are_not_retried() {
        let mut calls = 0;
        let r: Result<(), String> = with_writable(Path::new("/fs/sdb0/x"), || {
            calls += 1;
            Err("settings changed; reload before saving".into())
        });
        assert!(r.is_err());
        assert_eq!(calls, 1);
        let mut calls = 0;
        let r: Result<(), String> = with_writable(Path::new("/tmp/x"), || {
            calls += 1;
            Err("Read-only file system (os error 30)".into())
        });
        assert_eq!(r.unwrap_err(), "Read-only file system (os error 30)");
        assert_eq!(calls, 1);
    }

    #[test]
    fn retry_and_restore_order_preserves_both_failure_results() {
        use std::cell::{Cell, RefCell};

        for save_fails in [false, true] {
            for restore_fails in [false, true] {
                let calls = Cell::new(0);
                let events = RefCell::new(Vec::new());
                let result = with_writable_using(
                    Path::new("/fs/sdb0/settings"),
                    || {
                        calls.set(calls.get() + 1);
                        if calls.get() == 1 {
                            events.borrow_mut().push("initial save");
                            Err("Read-only file system (os error 30)".into())
                        } else {
                            events.borrow_mut().push("retry save");
                            if save_fails {
                                Err("retry save failed".into())
                            } else {
                                Ok(7)
                            }
                        }
                    },
                    |card, writable| {
                        assert_eq!(card, Path::new("/fs/sdb0"));
                        events.borrow_mut().push(if writable { "rw" } else { "ro" });
                        if !writable && restore_fails {
                            Err("read-only restore failed".into())
                        } else {
                            Ok(())
                        }
                    },
                );
                assert_eq!(*events.borrow(), ["initial save", "rw", "retry save", "ro"]);
                match (save_fails, restore_fails) {
                    (false, false) => assert_eq!(result.unwrap(), 7),
                    (true, false) => assert_eq!(result.unwrap_err(), "retry save failed"),
                    (false, true) => {
                        assert_eq!(result.unwrap_err(), "saved, but read-only restore failed");
                    }
                    (true, true) => {
                        let error = result.unwrap_err();
                        assert!(error.contains("retry save failed"));
                        assert!(error.contains("read-only restore failed"));
                    }
                }
            }
        }
    }

    #[test]
    fn unresolved_writable_mount_does_not_retry_or_restore() {
        let mut saves = 0;
        let mut mounts = 0;
        let result: Result<(), String> = with_writable_using(
            Path::new("/fs/sdb0/settings"),
            || {
                saves += 1;
                Err("Read-only file system (os error 30)".into())
            },
            |_, writable| {
                mounts += 1;
                assert!(writable);
                Err("remount did not finish; child termination NOT_PROVEN".into())
            },
        );
        assert!(result.unwrap_err().contains("child termination NOT_PROVEN"));
        assert_eq!(saves, 1);
        assert_eq!(mounts, 1);
    }

    #[cfg(unix)]
    mod child_tests {
        use super::*;
        use std::cell::Cell;
        use std::collections::VecDeque;
        use std::os::unix::process::ExitStatusExt;

        enum Poll {
            Pending,
            Exit(i32),
            Error,
        }

        struct FakeChild {
            polls: VecDeque<Poll>,
            kills: usize,
            kill_fails: bool,
        }

        impl MountChild for FakeChild {
            fn poll(&mut self) -> io::Result<Option<ExitStatus>> {
                match self.polls.pop_front().unwrap_or(Poll::Pending) {
                    Poll::Pending => Ok(None),
                    Poll::Exit(code) => Ok(Some(ExitStatus::from_raw(code << 8))),
                    Poll::Error => Err(io::Error::other("injected poll error")),
                }
            }

            fn kill(&mut self) -> io::Result<()> {
                self.kills += 1;
                if self.kill_fails {
                    Err(io::Error::other("injected kill error"))
                } else {
                    Ok(())
                }
            }
        }

        fn run(child: &mut FakeChild) -> (Result<(), String>, Duration) {
            let clock = Cell::new(Duration::ZERO);
            let result = await_mount_child(
                child,
                "owned remount",
                || clock.get(),
                || clock.set(clock.get() + POLL_INTERVAL),
            );
            (result, clock.get())
        }

        #[test]
        fn terminal_children_are_reaped_without_kill() {
            for status in [0, 7] {
                let mut child = FakeChild {
                    polls: VecDeque::from([Poll::Exit(status)]),
                    kills: 0,
                    kill_fails: false,
                };
                let (result, elapsed) = run(&mut child);
                assert_eq!(result.is_ok(), status == 0);
                assert_eq!(child.kills, 0);
                assert_eq!(elapsed, Duration::ZERO);
            }
        }

        #[test]
        fn poll_error_retires_owned_child_and_preserves_original_error() {
            let mut child = FakeChild {
                polls: VecDeque::from([Poll::Error, Poll::Pending, Poll::Exit(0)]),
                kills: 0,
                kill_fails: false,
            };
            let (result, elapsed) = run(&mut child);
            let error = result.unwrap_err();
            assert!(error.contains("injected poll error"));
            assert!(!error.contains("NOT_PROVEN"));
            assert_eq!(child.kills, 1);
            assert_eq!(elapsed, POLL_INTERVAL);
        }

        #[test]
        fn timeout_and_nonterminal_retirement_have_separate_bounds() {
            let mut child = FakeChild {
                polls: VecDeque::new(),
                kills: 0,
                kill_fails: false,
            };
            let (result, elapsed) = run(&mut child);
            let error = result.unwrap_err();
            assert!(error.contains("did not finish"));
            assert!(error.contains("child termination NOT_PROVEN"));
            assert_eq!(child.kills, 1);
            assert_eq!(elapsed, TIMEOUT + RETIRE_TIMEOUT);
        }

        #[test]
        fn kill_and_poll_failures_are_explicit_and_still_bounded() {
            let mut child = FakeChild {
                polls: VecDeque::from([Poll::Error, Poll::Error]),
                kills: 0,
                kill_fails: true,
            };
            let (result, elapsed) = run(&mut child);
            let error = result.unwrap_err();
            assert!(error.contains("child termination NOT_PROVEN"));
            assert!(error.contains("kill failed: injected kill error"));
            assert!(error.contains("poll failed: injected poll error"));
            assert_eq!(child.kills, 1);
            assert_eq!(elapsed, RETIRE_TIMEOUT);
        }
    }
}
