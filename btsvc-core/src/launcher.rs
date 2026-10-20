// SPDX-License-Identifier: MIT OR Apache-2.0
//! Select launch requests from held controller Guide and keyboard Home keys.
//! One request is emitted per hold. Release rearms it. A shared debounce
//! advances only after the caller records an actual launch. The caller
//! supplies input state, timestamps and output storage.

/// Poll cadence the shell drives [`Launcher::poll`] at (`GUIDE_POLL_MS`).
pub const GUIDE_POLL_MS: u32 = 250;
/// Default hold time (`BTGP_GUIDE_HOLD_MS` unset).
pub const DEFAULT_HOLD_MS: u32 = 1000;
/// Debounce interval for all launch triggers.
pub const DEBOUNCE_MS: u32 = 2000;
/// Pad slots - matches `BTGP_MAX_PADS` / the shm array.
pub const PAD_SLOTS: usize = crate::sm::MAX_PADS;
/// Maximum keyboard and mouse slots.
pub const KBM_SLOTS: usize = 3;
/// The guide/PS/Xbox button in the shm `buttons` word (bit 12, `BTN_PS`).
pub const GUIDE_BUTTON: u32 = 1 << crate::report::BTN_PS;

/// A launch trigger that crossed its hold threshold this poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Pad `slot` held the guide button past `hold_ms`.
    GuideHeld { slot: usize },
    /// kbm `slot`'s keyboard held Home past `hold_ms`.
    KeyboardHome { slot: usize },
}

/// The launcher state machine. All times are shell-provided milliseconds from
/// one monotonic clock.
pub struct Launcher {
    hold_ms: u32,
    last_launch: Option<u32>,
    pad_held_ms: [u32; PAD_SLOTS],
    pad_fired: [bool; PAD_SLOTS],
    /// Bit 0 tracks keyboard Home. Bit 1 tracks consumer Home.
    kbm_home_down: [u8; KBM_SLOTS],
    /// Hold start. None means released.
    kbm_down_since: [Option<u32>; KBM_SLOTS],
    kbm_fired: [bool; KBM_SLOTS],
}

impl Launcher {
    #[must_use]
    pub const fn new(hold_ms: u32) -> Self {
        Launcher {
            hold_ms,
            last_launch: None,
            pad_held_ms: [0; PAD_SLOTS],
            pad_fired: [false; PAD_SLOTS],
            kbm_home_down: [0; KBM_SLOTS],
            kbm_down_since: [None; KBM_SLOTS],
            kbm_fired: [false; KBM_SLOTS],
        }
    }

    /// One poll tick (`guide_poll`). `guide_down[i]` = pad slot i is connected
    /// AND its guide bit is set. Pad hold time accumulates tick_ms per call.
    /// The keyboard side compares now_ms against
    /// the hold start. Returns the number of triggers written to `out` -
    /// every pad crossing plus at most one keyboard crossing. Emit regardless
    /// of the debounce (the fired latch must be set either way); the shell
    /// gates the actual spawn on [`debounce_ok`](Self::debounce_ok) and its
    /// child check.
    pub fn poll(
        &mut self,
        now_ms: u32,
        tick_ms: u32,
        guide_down: &[bool; PAD_SLOTS],
        out: &mut [Trigger],
    ) -> usize {
        let mut n = 0;
        for (i, &down) in guide_down.iter().enumerate() {
            if down {
                self.pad_held_ms[i] = self.pad_held_ms[i].saturating_add(tick_ms);
                if !self.pad_fired[i] && self.pad_held_ms[i] >= self.hold_ms {
                    self.pad_fired[i] = true;
                    if n < out.len() {
                        out[n] = Trigger::GuideHeld { slot: i };
                        n += 1;
                    }
                }
            } else {
                self.pad_held_ms[i] = 0;
                self.pad_fired[i] = false;
            }
        }
        for i in 0..KBM_SLOTS {
            if let Some(t0) = self.kbm_down_since[i] {
                if !self.kbm_fired[i] && now_ms.wrapping_sub(t0) >= self.hold_ms {
                    self.kbm_fired[i] = true;
                    if n < out.len() {
                        out[n] = Trigger::KeyboardHome { slot: i };
                        n += 1;
                    }
                    break; // one keyboard launch per poll (kbm_home_held)
                }
            }
        }
        n
    }

    /// Track keyboard Home state from one parsed report. `saw_kbd`/`saw_consumer` = the report carried
    /// any page-7 / page-0x0C field; `home7_down` = a PRESSED page-7 0x4A was
    /// in it; `ac_home_down` = the consumer 0x223 field's value was non-zero.
    /// Each usage's release is tracked only from reports that carry its page.
    // Keep the established shell API: flags describe independent HID pages.
    #[allow(clippy::fn_params_excessive_bools)]
    pub fn kbm_report(
        &mut self,
        slot: usize,
        saw_kbd: bool,
        home7_down: bool,
        saw_consumer: bool,
        ac_home_down: bool,
        now_ms: u32,
    ) {
        if slot >= KBM_SLOTS {
            return;
        }
        let d = &mut self.kbm_home_down[slot];
        if saw_kbd {
            *d = (*d & !1) | u8::from(home7_down);
        }
        if saw_consumer {
            *d = (*d & !2) | (u8::from(ac_home_down) << 1);
        }
        if *d != 0 {
            if self.kbm_down_since[slot].is_none() {
                self.kbm_down_since[slot] = Some(now_ms);
            }
        } else {
            self.kbm_down_since[slot] = None;
            self.kbm_fired[slot] = false;
        }
    }

    /// Clear a kbm slot's hold state (its connection closed).
    pub fn kbm_reset(&mut self, slot: usize) {
        if slot >= KBM_SLOTS {
            return;
        }
        self.kbm_home_down[slot] = 0;
        self.kbm_down_since[slot] = None;
        self.kbm_fired[slot] = false;
    }

    /// The two-second debounce interval for all launch triggers.
    #[must_use]
    pub fn debounce_ok(&self, now_ms: u32) -> bool {
        match self.last_launch {
            None => true,
            Some(t) => now_ms.wrapping_sub(t) >= DEBOUNCE_MS,
        }
    }

    /// Record an actual spawn. Only a real spawn advances the debounce - the
    /// C sets `last_launch_ms` after the child-still-running check.
    pub fn note_launch(&mut self, now_ms: u32) {
        self.last_launch = Some(now_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u32 = GUIDE_POLL_MS;

    /// Drive `ticks` polls with the given pad state, collecting triggers.
    /// Time starts at `t0` and advances one `GUIDE_POLL_MS` per tick.
    fn drive(
        l: &mut Launcher,
        t0: u32,
        ticks: u32,
        down: [bool; PAD_SLOTS],
    ) -> Vec<(u32, Trigger)> {
        let mut fired = Vec::new();
        let mut buf = [Trigger::GuideHeld { slot: 0 }; 8];
        for k in 0..ticks {
            let now = t0 + (k + 1) * T;
            let n = l.poll(now, T, &down, &mut buf);
            for t in &buf[..n] {
                fired.push((now, *t));
            }
        }
        fired
    }

    const NONE: [bool; PAD_SLOTS] = [false; PAD_SLOTS];

    fn pad(i: usize) -> [bool; PAD_SLOTS] {
        let mut d = NONE;
        d[i] = true;
        d
    }

    #[test]
    fn tap_does_not_fire() {
        let mut l = Launcher::new(DEFAULT_HOLD_MS);
        // 3 ticks = 750 ms < 1000 ms, then release
        assert_eq!(drive(&mut l, 0, 3, pad(0)), vec![]);
        assert_eq!(drive(&mut l, 750, 4, NONE), vec![]);
    }

    #[test]
    fn hold_fires_once_at_threshold_and_latches() {
        let mut l = Launcher::new(DEFAULT_HOLD_MS);
        // 4th tick accumulates 1000 ms -> fire; 20 more held ticks stay silent
        let fired = drive(&mut l, 0, 24, pad(0));
        assert_eq!(fired, vec![(4 * T, Trigger::GuideHeld { slot: 0 })]);
    }

    #[test]
    fn release_rearms() {
        let mut l = Launcher::new(DEFAULT_HOLD_MS);
        assert_eq!(drive(&mut l, 0, 4, pad(0)).len(), 1);
        l.note_launch(4 * T);
        assert_eq!(drive(&mut l, 1000, 1, NONE), vec![]);
        // hold again: fires on the 4th tick of the new hold
        let fired = drive(&mut l, 1250, 4, pad(0));
        assert_eq!(fired, vec![(1250 + 4 * T, Trigger::GuideHeld { slot: 0 })]);
        // ...and the shell-side debounce would still block a spawn this soon:
        assert!(!l.debounce_ok(1250 + 4 * T));
        assert!(l.debounce_ok(4 * T + DEBOUNCE_MS));
    }

    #[test]
    fn debounce_gates_second_trigger_not_first() {
        let mut l = Launcher::new(DEFAULT_HOLD_MS);
        assert!(l.debounce_ok(0));
        l.note_launch(1000);
        assert!(!l.debounce_ok(1000 + DEBOUNCE_MS - 1));
        assert!(l.debounce_ok(1000 + DEBOUNCE_MS));
    }

    #[test]
    fn blocked_launch_does_not_consume_debounce() {
        // The shell skips note_launch when the child is still running - the
        // debounce must not advance from the trigger alone.
        let mut l = Launcher::new(DEFAULT_HOLD_MS);
        let mut buf = [Trigger::GuideHeld { slot: 0 }; 8];
        for k in 1..=4u32 {
            l.poll(k * T, T, &pad(0), &mut buf);
        }
        assert!(l.debounce_ok(4 * T)); // no note_launch happened
    }

    #[test]
    fn multi_slot_independent_accumulation() {
        let mut l = Launcher::new(DEFAULT_HOLD_MS);
        let both = {
            let mut d = pad(0);
            d[1] = true;
            d
        };
        // pad 1 joins after 2 ticks of pad 0
        assert_eq!(drive(&mut l, 0, 2, pad(0)), vec![]);
        let fired = drive(&mut l, 500, 4, both);
        // pad 0 crosses on its 4th total tick, pad 1 two ticks later
        assert_eq!(
            fired,
            vec![
                (500 + 2 * T, Trigger::GuideHeld { slot: 0 }),
                (500 + 4 * T, Trigger::GuideHeld { slot: 1 }),
            ]
        );
    }

    #[test]
    fn release_of_one_slot_does_not_reset_another() {
        let mut l = Launcher::new(DEFAULT_HOLD_MS);
        let both = {
            let mut d = pad(0);
            d[1] = true;
            d
        };
        assert_eq!(drive(&mut l, 0, 3, both), vec![]); // both at 750 ms
                                                       // release pad 0 only; pad 1 fires on its 4th tick
        let fired = drive(&mut l, 750, 1, pad(1));
        assert_eq!(fired, vec![(1000, Trigger::GuideHeld { slot: 1 })]);
    }

    #[test]
    fn zero_hold_fires_on_first_held_poll() {
        let mut l = Launcher::new(0);
        let fired = drive(&mut l, 0, 1, pad(2));
        assert_eq!(fired, vec![(T, Trigger::GuideHeld { slot: 2 })]);
    }

    // --- keyboard Home ------------------------------------------------------

    #[test]
    fn kbm_hold_fires_and_latches() {
        let mut l = Launcher::new(DEFAULT_HOLD_MS);
        let mut buf = [Trigger::GuideHeld { slot: 0 }; 8];
        l.kbm_report(0, true, true, false, false, 100); // Home down at t=100
        assert_eq!(l.poll(1000, T, &NONE, &mut buf), 0); // 900 ms held
        assert_eq!(l.poll(1100, T, &NONE, &mut buf), 1); // 1000 ms held
        assert_eq!(buf[0], Trigger::KeyboardHome { slot: 0 });
        assert_eq!(l.poll(1350, T, &NONE, &mut buf), 0); // latched
                                                         // release resets; a new hold fires again
        l.kbm_report(0, true, false, false, false, 1400);
        l.kbm_report(0, true, true, false, false, 1500);
        assert_eq!(l.poll(2500, T, &NONE, &mut buf), 1);
    }

    #[test]
    fn kbm_page_release_isolation() {
        // A consumer-only report must not release the page-7 Home (and vice
        // versa) - reports contain changes for one page at a time.
        let mut l = Launcher::new(DEFAULT_HOLD_MS);
        let mut buf = [Trigger::GuideHeld { slot: 0 }; 8];
        l.kbm_report(0, true, true, false, false, 0); // page-7 Home down
        l.kbm_report(0, false, false, true, false, 200); // consumer-only report
        assert_eq!(l.poll(1000, T, &NONE, &mut buf), 1); // still held since t=0
                                                         // now a page-7 report without Home releases it
        l.kbm_report(0, true, false, false, false, 1100);
        l.kbm_report(0, true, true, false, false, 1200); // re-press
        assert_eq!(l.poll(2100, T, &NONE, &mut buf), 0); // 900 ms
        assert_eq!(l.poll(2200, T, &NONE, &mut buf), 1);
    }

    #[test]
    fn kbm_ac_home_alone_counts() {
        let mut l = Launcher::new(DEFAULT_HOLD_MS);
        let mut buf = [Trigger::GuideHeld { slot: 0 }; 8];
        l.kbm_report(1, false, false, true, true, 0); // AC Home down
        assert_eq!(l.poll(1000, T, &NONE, &mut buf), 1);
        assert_eq!(buf[0], Trigger::KeyboardHome { slot: 1 });
        // AC Home release via a consumer report
        l.kbm_report(1, false, false, true, false, 1100);
        assert_eq!(l.poll(3000, T, &NONE, &mut buf), 0);
    }

    #[test]
    fn kbm_one_launch_per_poll() {
        // Two keyboards holding Home past the threshold in the same poll:
        // Emit the first request now and the second on the next poll.
        let mut l = Launcher::new(DEFAULT_HOLD_MS);
        let mut buf = [Trigger::GuideHeld { slot: 0 }; 8];
        l.kbm_report(0, true, true, false, false, 0);
        l.kbm_report(1, true, true, false, false, 0);
        assert_eq!(l.poll(1500, T, &NONE, &mut buf), 1);
        assert_eq!(buf[0], Trigger::KeyboardHome { slot: 0 });
        assert_eq!(l.poll(1750, T, &NONE, &mut buf), 1);
        assert_eq!(buf[0], Trigger::KeyboardHome { slot: 1 });
    }

    #[test]
    fn kbm_reset_clears_hold() {
        let mut l = Launcher::new(DEFAULT_HOLD_MS);
        let mut buf = [Trigger::GuideHeld { slot: 0 }; 8];
        l.kbm_report(2, true, true, false, false, 0);
        l.kbm_reset(2); // disconnect
        assert_eq!(l.poll(5000, T, &NONE, &mut buf), 0);
    }

    #[test]
    fn pad_and_keyboard_same_poll_both_emitted() {
        // The shell's debounce collapses them to one spawn, but the core
        // reports both crossings (each latches its own fired flag).
        let mut l = Launcher::new(DEFAULT_HOLD_MS);
        let mut buf = [Trigger::GuideHeld { slot: 0 }; 8];
        l.kbm_report(0, true, true, false, false, 0);
        for k in 1..=3u32 {
            assert_eq!(l.poll(k * T, T, &pad(0), &mut buf), 0);
        }
        let n = l.poll(4 * T, T, &pad(0), &mut buf);
        assert_eq!(n, 2);
        assert_eq!(buf[0], Trigger::GuideHeld { slot: 0 });
        assert_eq!(buf[1], Trigger::KeyboardHome { slot: 0 });
    }
}
