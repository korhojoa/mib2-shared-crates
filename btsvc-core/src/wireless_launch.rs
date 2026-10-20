// SPDX-License-Identifier: MIT OR Apache-2.0
//! Priority and coalescing for early preparation and first-USB-write fallback.

use crate::wireless_policy::Address;

/// Bound an unanswered setup without disabling discovery indefinitely.
pub const SETUP_PRIORITY_MS: u32 = 15_000;

#[derive(Default)]
pub struct InquiryPriority {
    started: Option<u32>,
}

impl InquiryPriority {
    #[must_use]
    pub const fn new() -> Self {
        Self { started: None }
    }

    pub fn begin(&mut self, now: u32) {
        self.started = Some(now);
    }

    #[must_use]
    pub fn active(&self, now: u32) -> bool {
        self.started
            .is_some_and(|start| now.wrapping_sub(start) < SETUP_PRIORITY_MS)
    }

    /// True exactly once when completion, cancellation or expiry releases priority.
    pub fn release(&mut self, now: u32, setup_open: bool) -> bool {
        if self.started.is_some() && (!setup_open || !self.active(now)) {
            self.started = None;
            true
        } else {
            false
        }
    }
}

#[derive(Default)]
pub struct EarlyPrepare {
    pending: Option<(Address, u64, u32)>,
    passed: bool,
}

impl EarlyPrepare {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pending: None,
            passed: false,
        }
    }

    /// False for a duplicate early request. Do not extend its deadline.
    pub fn record(&mut self, peer: Address, epoch: u64, now: u32) -> bool {
        if self.pending.is_some_and(|(p, e, start)| {
            p == peer && e == epoch && now.wrapping_sub(start) < SETUP_PRIORITY_MS
        }) {
            return false;
        }
        self.pending = Some((peer, epoch, now));
        self.passed = false;
        true
    }

    pub fn clear(&mut self) {
        self.pending = None;
        self.passed = false;
    }

    /// Only a PASS from this preparation can suppress a fallback after close.
    pub fn mark_pass(&mut self, peer: Address, epoch: u64) {
        if self
            .pending
            .is_some_and(|(p, e, _)| p == peer && e == epoch)
        {
            self.passed = true;
        }
    }

    /// Consume the first-write fallback once, only for a matching live setup.
    pub fn coalesce(&mut self, peer: Address, epoch: u64, now: u32, setup_live: bool) -> bool {
        self.pending.take().is_some_and(|(p, e, start)| {
            p == peer
                && e == epoch
                && now.wrapping_sub(start) < SETUP_PRIORITY_MS
                && (setup_live || self.passed)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: Address = [1; 6];
    const B: Address = [2; 6];

    #[test]
    fn priority_finishes_cancels_expires_and_survives_clock_wrap() {
        let mut priority = InquiryPriority::new();
        assert!(!priority.active(0));
        priority.begin(u32::MAX - 10);
        assert!(priority.active(20));
        assert!(!priority.release(20, true));
        assert!(priority.release(20, false));
        assert!(!priority.release(20, false));
        priority.begin(50);
        assert!(priority.active(50 + SETUP_PRIORITY_MS - 1));
        assert!(priority.release(50 + SETUP_PRIORITY_MS, true));
    }

    #[test]
    fn early_request_coalesces_fallback_once_without_extending_deadline() {
        let mut early = EarlyPrepare::new();
        assert!(early.record(A, 1, 10));
        assert!(!early.record(A, 1, 20));
        assert!(early.coalesce(A, 1, 30, true));
        assert!(!early.coalesce(A, 1, 30, true));
        assert!(early.record(A, 1, 40));
        assert!(!early.record(A, 1, 40 + SETUP_PRIORITY_MS - 1));
        assert!(!early.coalesce(A, 1, 40 + SETUP_PRIORITY_MS, true));
    }

    #[test]
    fn failed_changed_or_revoked_setup_keeps_the_fallback() {
        for (peer, epoch, live) in [(B, 1, true), (A, 2, true), (A, 1, false)] {
            let mut early = EarlyPrepare::new();
            early.record(A, 1, 0);
            assert!(!early.coalesce(peer, epoch, 1, live));
        }
        let mut early = EarlyPrepare::new();
        early.record(A, 1, 0);
        early.clear();
        assert!(!early.coalesce(A, 1, 1, true));
    }

    #[test]
    fn only_this_attempts_pass_suppresses_fallback_after_channel_close() {
        let mut early = EarlyPrepare::new();
        early.record(A, 2, 0);
        early.mark_pass(A, 1);
        assert!(!early.coalesce(A, 2, 1, false));
        early.record(A, 2, 2);
        early.mark_pass(A, 2);
        assert!(early.coalesce(A, 2, 3, false));
        assert!(!early.coalesce(A, 2, 3, false));
    }
}
