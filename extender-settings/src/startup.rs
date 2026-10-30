// SPDX-License-Identifier: MIT OR Apache-2.0
//! One native startup identity, separate from the stdout settings protocol.
use std::ffi::CString;

fn banner(mono_ms: u64, wall_ms: u64) -> CString {
    CString::new(format!(
        "android-auto-settings {} mono_ms={mono_ms} wall_ms={wall_ms}",
        env!("CARGO_PKG_VERSION")
    ))
    .expect("fixed program/version and integer clocks contain no NUL")
}

fn write_banner(mono_ms: u64, wall_ms: u64, sink: impl FnOnce(&std::ffi::CStr) -> bool) -> bool {
    sink(&banner(mono_ms, wall_ms))
}

pub(crate) fn announce() {
    #[cfg(target_os = "nto")]
    let _ = write_banner(qnx_slog::sys::mono_ms(), qnx_slog::sys::wall_ms(), |line| {
        let code = qnx_slog::sys::slog_code(qnx_slog::sys::SLOGC_PRIVATE_START, 1);
        qnx_slog::sink::write_code(code, 5, line)
    });
    #[cfg(not(target_os = "nto"))]
    let _ = write_banner(0, 0, |_| false);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_version_and_both_clocks_reach_only_one_native_sink_call() {
        let mut calls = Vec::new();
        assert!(write_banner(123, 456, |line| {
            calls.push(line.to_string_lossy().into_owned());
            true
        }));
        assert_eq!(
            calls,
            [format!(
                "android-auto-settings {} mono_ms=123 wall_ms=456",
                env!("CARGO_PKG_VERSION")
            )]
        );
    }
    #[test]
    fn unavailable_log_does_not_retry_or_emit_protocol_bytes() {
        let mut calls = 0;
        assert!(!write_banner(0, 0, |_| {
            calls += 1;
            false
        }));
        assert_eq!(calls, 1);
    }
}
