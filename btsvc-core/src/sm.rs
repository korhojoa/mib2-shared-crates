// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bluetooth HID connection and input state machine.
//! Consume explicit events and emit commands for the caller to execute.
//! Timers are inputs. This module performs no I/O.

use crate::descriptor::{app_usages, AppUsages};
use crate::report::{
    classify, parse_for_type, parse_xbox_guide, PadState, PadType, XboxLayout, BTN_PS,
};

/// A 48-bit Bluetooth address (display order).
pub type BdAddr = [u8; 6];

/// Concurrent pad slots - matches `BTGP_MAX_PADS` / the shm array (v5 "BTG5").
///
/// 8 is the ABI ceiling, not a radio promise: a Bluetooth Classic piconet
/// addresses at most SEVEN active slaves (3-bit `LT_ADDR`, 0 reserved for the
/// master's broadcast), and that budget is shared with any Classic kbd/mouse
/// this daemon holds. BLE connections are a separate pool, so 8 concurrent
/// pads is reachable only as a Classic + BLE mix (e.g. 6 + 2).
pub const MAX_PADS: usize = 8;

/// Max remembered LE pad bonds (for accept-list reconnect). One per slot: an
/// all-BLE fill must still be able to reconnect every pad unaided.
pub const LE_MAX_BONDS: usize = MAX_PADS;

/// What the LE side should do when idle. The host-like reconnect model:
/// bonded devices ride the controller accept-list (one outstanding
/// connect-with-whitelist covers ALL of them - the controller completes it
/// when ANY listed device wakes and advertises); scanning happens only to
/// pair a new device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeAction {
    /// Nothing to do (busy, or every bonded device is connected).
    Idle,
    /// Bonded devices are missing - put each ([`PadCore::missing_le_bonds`]
    /// plus the shell's missing kbm bonds) on the accept-list and keep one
    /// connect-with-whitelist outstanding.
    WhitelistConnect,
    /// Scan for something new. `pairing` = a fresh-pairing session (100%
    /// duty) vs a low-duty scan.
    Scan { pairing: bool },
}

#[derive(Clone, Copy)]
struct LeBond {
    addr: BdAddr,
    atype: u8,
}

/// Bounded DS3 "set operational" nudge retries. C: `DS3_START_MAX_TRIES`. A
/// silent Xbox must not be poked with 0xF4 forever.
pub const DS3_START_MAX_TRIES: u8 = 10;

const SETUP_FIRST_MS: u32 = 400;
const SETUP_NUDGE_MS: u32 = 700;
const SETUP_DS4_FULL_MS: u32 = 2000;

/// Per-run configuration (the subset the pad machine reads).
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub kbm_enabled: bool,
    pub xbox_layout: XboxLayout,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            kbm_enabled: true,
            xbox_layout: XboxLayout::Auto,
        }
    }
}

/// Inputs to the machine - `BTstack` packets decoded to plain data + metadata.
/// `'a` borrows the report/descriptor bytes from the caller's packet buffer.
#[derive(Debug, Clone, Copy)]
pub enum Event<'a> {
    /// Incoming HID connection request (accept + arm redial).
    HidIncoming { cid: u16, addr: BdAddr },
    /// HID connection open result. `descriptor` is the SDP HID report
    /// descriptor if already fetched (often empty on an incoming reconnect).
    HidOpened {
        cid: u16,
        addr: BdAddr,
        status: u8,
        descriptor: &'a [u8],
    },
    /// The deferred SDP descriptor is now available (final routing).
    HidDescriptorAvailable { cid: u16, descriptor: &'a [u8] },
    /// One HID input report (INCLUDING the 0xa1 header, as the parsers expect).
    HidReport { cid: u16, data: &'a [u8] },
    /// HID connection closed.
    HidClosed { cid: u16 },
    /// A per-slot setup timer fired.
    SetupTick { slot: u8 },

    // --- LE / HOGP (the shell drives scan/connect/pair/hids I/O; these events
    // carry the results so slot management + classify/publish stay unified with
    // the Classic path). ---
    /// An LE pad ACL connection completed (the shell only sends this after its
    /// own outgoing connect - the ghost-slot guard lives shell-side).
    LeConnected { le_handle: u16, addr: BdAddr },
    /// The HOGP client cid became known for an LE slot (after `hids_client_connect`).
    LeHidsCid { le_handle: u16, hids_cid: u16 },
    /// One HOGP report from `hids_client` (payload WITHOUT the 0xa1/report-id
    /// header; the core frames it). Delivery here makes the raw path stand down.
    LeReport {
        hids_cid: u16,
        report_id: u8,
        data: &'a [u8],
    },
    /// A raw GATT notification (the fallback for pads whose `hids_client`
    /// report-matching drops input - e.g. Xbox-BLE). Ignored once `LeReport`
    /// has delivered for that slot.
    LeRawReport {
        hids_cid: u16,
        report_id: u8,
        data: &'a [u8],
    },
    /// An LE ACL disconnected.
    LeClosed { le_handle: u16 },
}

/// HID connection open status: success. `BTstack` `ERROR_CODE_SUCCESS`.
pub const STATUS_SUCCESS: u8 = 0;

/// Outputs - `BTstack` calls and shm writes the shell performs. No formatted
/// strings: logging is the shell's job, driven off these structured commands.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Accept an incoming HID connection (report protocol mode).
    HidAccept { cid: u16 },
    /// Dial a pad (redial after a failed incoming, or a targeted connect).
    HidConnect { addr: BdAddr },
    /// Disconnect a HID channel by cid (slot-full drop).
    HidDisconnect { cid: u16 },
    /// Drop a half-open ACL by address (shell resolves the handle).
    GapDisconnectAcl { addr: BdAddr },
    /// DS3 "set operational" feature nudge (0xF4).
    Ds3SendStart { cid: u16 },
    /// Light the DS3 slot LED (also stops the 4-LED blink).
    Ds3SetLed { cid: u16, slot: u8 },
    /// DS4 full-report enable (`get_report` feature 0x02).
    Ds4EnableFullReport { cid: u16 },
    /// Arm the per-slot setup timer.
    SetTimer { slot: u8, ms: u32 },
    /// Cancel the per-slot setup timer.
    ClearTimer { slot: u8 },
    /// Route this connection to the kbm module (out of this machine's scope).
    RouteKbm {
        cid: u16,
        addr: BdAddr,
        kbd: bool,
        mouse: bool,
    },
    /// Mark a shm slot connected with its classified type (`pad_type` before
    /// connected - the classify-before-connected invariant).
    ShmConnected { slot: u8, pad_type: PadType },
    /// Publish normalized input into a shm slot.
    ShmPublish { slot: u8, state: PadState },
    /// Mark a shm slot disconnected and cleared.
    ShmDisconnected { slot: u8 },
    /// Classic pad: run the SDP `DeviceID` query (log vendor/product/fw).
    DidQuery { addr: BdAddr },
    /// The pad table filled up - quiesce the LE side (it shares slots).
    LeSuspend,
    /// A pad slot freed / a safety net - re-arm the LE side.
    LeReArm,
}

/// The transport an output report (rumble) rides for a slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RumbleRoute {
    /// Classic HID: interrupt channel via `hid_host_send_report` (DS4/Xbox)
    /// or control-channel `SET_REPORT` (DS3).
    Classic { cid: u16 },
    /// BLE/HOGP: `hids_host_send_write_report` on the HOGP client cid.
    Le { hids_cid: u16 },
}

/// Sink for emitted commands. A blanket impl covers `FnMut(Command)` and a
/// `Vec<Command>`-backed collector in tests, so the core needs no allocator.
pub trait Emit {
    fn emit(&mut self, cmd: Command);
}

impl<F: FnMut(Command)> Emit for F {
    fn emit(&mut self, cmd: Command) {
        self(cmd);
    }
}

#[derive(Clone, Copy)]
// These independent native link and report flags preserve the existing state API.
#[allow(clippy::struct_excessive_bools)]
struct PadSlot {
    used: bool,
    is_le: bool,
    cid: u16,       // Classic HID cid (Classic slots)
    le_handle: u16, // LE ACL connection handle (LE slots)
    hids_cid: u16,  // HOGP client cid (LE slots, set after connect)
    addr: BdAddr,
    pad_type: Option<PadType>, // None = UNKNOWN (classification pending)
    full_report_active: bool,
    ds3_start_tries: u8,
    xbox_guide: bool,
    hids_delivers: bool, // hids_client's own HID_REPORT works -> raw stands down
    last_state: Option<PadState>,
}

impl PadSlot {
    const EMPTY: PadSlot = PadSlot {
        used: false,
        is_le: false,
        cid: 0,
        le_handle: 0,
        hids_cid: 0,
        addr: [0; 6],
        pad_type: None,
        full_report_active: false,
        ds3_start_tries: 0,
        xbox_guide: false,
        hids_delivers: false,
        last_state: None,
    };
}

/// The Classic pad-lifecycle core.
pub struct PadCore {
    pads: [PadSlot; MAX_PADS],
    config: Config,
    connecting: bool,
    redial: bool,
    target: Option<BdAddr>,
    pending: Option<(u16, BdAddr)>, // deferred descriptor routing
    le_bonds: [LeBond; LE_MAX_BONDS],
    le_num_bonds: usize,
}

impl PadCore {
    #[must_use]
    pub fn new(config: Config) -> Self {
        PadCore {
            pads: [PadSlot::EMPTY; MAX_PADS],
            config,
            connecting: false,
            redial: false,
            target: None,
            pending: None,
            le_bonds: [LeBond {
                addr: [0; 6],
                atype: 0,
            }; LE_MAX_BONDS],
            le_num_bonds: 0,
        }
    }

    /// Remember a bonded LE pad address (loaded from the persisted device DB at
    /// startup, or added when a pairing completes). Idempotent.
    pub fn add_le_bond(&mut self, addr: BdAddr, atype: u8) {
        if self.le_bonds[..self.le_num_bonds]
            .iter()
            .any(|b| b.addr == addr)
        {
            return;
        }
        if self.le_num_bonds < LE_MAX_BONDS {
            self.le_bonds[self.le_num_bonds] = LeBond { addr, atype };
            self.le_num_bonds += 1;
        }
    }

    /// Is this address a remembered LE bond? (For the adv bonded-match path.)
    #[must_use]
    pub fn is_le_bond(&self, addr: &BdAddr) -> bool {
        self.le_bonds[..self.le_num_bonds]
            .iter()
            .any(|b| b.addr == *addr)
    }

    /// The bonded address+type at a bond index (for the shell to connect it).
    #[must_use]
    pub fn le_bond(&self, addr: &BdAddr) -> Option<u8> {
        self.le_bonds[..self.le_num_bonds]
            .iter()
            .find(|b| b.addr == *addr)
            .map(|b| b.atype)
    }

    /// Decide the LE side's action when idle. `busy` = a directed LE connect
    /// is already in flight. `kbm_bonds` / `kbm_missing` describe the shell's
    /// kbm bond table (bonded count / any bonded-but-disconnected), so one
    /// decision covers both sides of the radio. Priority: a pairing session
    /// scans; any missing bond arms the accept-list; a box with no bonds at
    /// all bootstraps with a pairing scan (the OUI gate still applies).
    #[must_use]
    pub fn le_decide(
        &self,
        busy: bool,
        force_pair: bool,
        kbm_bonds: usize,
        kbm_missing: bool,
    ) -> LeAction {
        if busy {
            return LeAction::Idle;
        }
        if force_pair {
            return LeAction::Scan { pairing: true };
        }
        let pad_missing = !self.pads_full()
            && self.le_bonds[..self.le_num_bonds]
                .iter()
                .any(|b| !self.le_addr_connected(&b.addr));
        if pad_missing || kbm_missing {
            return LeAction::WhitelistConnect;
        }
        if self.le_num_bonds == 0 && kbm_bonds == 0 && !self.pads_full() {
            return LeAction::Scan { pairing: true };
        }
        LeAction::Idle
    }

    /// Bonded-but-disconnected LE pads - the pad half of the accept-list.
    /// Empty while the pad table is full (no slot to land a reconnect in).
    pub fn missing_le_bonds(&self) -> impl Iterator<Item = (BdAddr, u8)> + '_ {
        self.le_bonds[..self.le_num_bonds]
            .iter()
            .filter(move |b| !self.pads_full() && !self.le_addr_connected(&b.addr))
            .map(|b| (b.addr, b.atype))
    }

    /// Number of allocated slots.
    #[must_use]
    pub fn pad_count(&self) -> usize {
        self.pads.iter().filter(|p| p.used).count()
    }

    /// True once this connection has been classified as a known pad family.
    /// The shell uses it to hex-dump reports from pads we do NOT recognise
    /// (`BTGP_PADPROBE`) - `classify()` covers only DS4/DS3/Xbox and everything
    /// else is dropped silently, which leaves a new model with no trace to
    /// write a parser from.
    #[must_use]
    pub fn cid_is_classified(&self, cid: u16) -> bool {
        self.pads
            .iter()
            .any(|p| p.used && p.cid == cid && p.pad_type.is_some())
    }

    /// Where (and as what) to send an output report for a slot's pad -
    /// the rumble path. `None` until the pad is classified and its transport
    /// is fully up (LE: the HOGP client connected).
    #[must_use]
    pub fn rumble_route(&self, slot: usize) -> Option<(PadType, RumbleRoute)> {
        let p = self.pads.get(slot)?;
        if !p.used {
            return None;
        }
        let t = p.pad_type?;
        if p.is_le {
            if p.hids_cid == 0 {
                return None;
            }
            Some((
                t,
                RumbleRoute::Le {
                    hids_cid: p.hids_cid,
                },
            ))
        } else {
            if p.cid == 0 {
                return None;
            }
            Some((t, RumbleRoute::Classic { cid: p.cid }))
        }
    }

    fn pads_full(&self) -> bool {
        self.pads.iter().all(|p| p.used)
    }

    fn slot_by_cid(&self, cid: u16) -> Option<usize> {
        self.pads.iter().position(|p| p.used && p.cid == cid)
    }

    fn alloc(&mut self) -> Option<usize> {
        self.pads.iter().position(|p| !p.used)
    }

    /// Feed one event, emitting zero or more commands.
    /// # Panics
    /// Panics if an internal slot index exceeds the fixed pad table.
    pub fn on_event(&mut self, ev: Event, out: &mut impl Emit) {
        match ev {
            Event::HidIncoming { cid, addr } => {
                // Accept everything; sort keyboard/mouse/pad at OPENED. Arm a
                // single dial-back in case the incoming handshake times out.
                self.connecting = true;
                self.target = Some(addr);
                self.redial = true;
                out.emit(Command::HidAccept { cid });
            }
            Event::HidOpened {
                cid,
                addr,
                status,
                descriptor,
            } => self.on_opened(cid, addr, status, descriptor, out),
            Event::HidDescriptorAvailable { cid, descriptor } => {
                if self.pending.map(|(c, _)| c) == Some(cid) {
                    let (_, addr) = self.pending.take().unwrap();
                    self.route(cid, addr, descriptor, true, out);
                }
            }
            Event::HidReport { cid, data } => self.on_report(cid, data, out),
            Event::HidClosed { cid } => self.on_closed(cid, out),
            Event::SetupTick { slot } => self.on_setup_tick(slot, out),
            Event::LeConnected { le_handle, addr } => self.on_le_connected(le_handle, addr),
            Event::LeHidsCid {
                le_handle,
                hids_cid,
            } => self.on_le_hids_cid(le_handle, hids_cid),
            Event::LeReport {
                hids_cid,
                report_id,
                data,
            } => self.on_le_report(hids_cid, report_id, data, false, out),
            Event::LeRawReport {
                hids_cid,
                report_id,
                data,
            } => self.on_le_report(hids_cid, report_id, data, true, out),
            Event::LeClosed { le_handle } => self.on_le_closed(le_handle, out),
        }
    }

    /// The HOGP client cid for an LE connection handle (for the shell's raw
    /// notification path). None if the handle isn't a known LE pad slot.
    #[must_use]
    pub fn le_hids_cid_for_handle(&self, le_handle: u16) -> Option<u16> {
        self.slot_by_le_handle(le_handle)
            .map(|s| self.pads[s].hids_cid)
    }

    /// Is this address already occupying an LE slot? (Guards the shell against
    /// reconnecting a pad that is already connected but still advertising.)
    #[must_use]
    pub fn le_addr_connected(&self, addr: &BdAddr) -> bool {
        self.pads
            .iter()
            .any(|p| p.used && p.is_le && p.addr == *addr)
    }

    fn slot_by_le_handle(&self, h: u16) -> Option<usize> {
        self.pads
            .iter()
            .position(|p| p.used && p.is_le && p.le_handle == h)
    }

    fn slot_by_hids_cid(&self, c: u16) -> Option<usize> {
        self.pads
            .iter()
            .position(|p| p.used && p.is_le && p.hids_cid == c)
    }

    fn on_le_connected(&mut self, le_handle: u16, addr: BdAddr) {
        // The shell already applied the ghost-slot guard (only connects it
        // initiated). Reserve a slot; classification waits for the first
        // report. LE pads need no setup-tick nudge (that is Classic DS3/DS4).
        if self.slot_by_le_handle(le_handle).is_some() {
            return;
        }
        if let Some(slot) = self.alloc() {
            self.pads[slot] = PadSlot {
                used: true,
                is_le: true,
                le_handle,
                addr,
                ..PadSlot::EMPTY
            };
        }
    }

    fn on_le_hids_cid(&mut self, le_handle: u16, hids_cid: u16) {
        if let Some(slot) = self.slot_by_le_handle(le_handle) {
            self.pads[slot].hids_cid = hids_cid;
        }
    }

    fn on_le_report(
        &mut self,
        hids_cid: u16,
        report_id: u8,
        data: &[u8],
        raw: bool,
        out: &mut impl Emit,
    ) {
        let Some(slot) = self.slot_by_hids_cid(hids_cid) else {
            return;
        };
        if raw {
            // Raw GATT fallback: stand down once hids_client's own delivery works.
            if self.pads[slot].hids_delivers {
                return;
            }
        } else {
            self.pads[slot].hids_delivers = true;
        }
        // Frame as the parsers expect: 0xa1, report_id, payload. C:
        // le_publish_report.
        let mut buf = [0u8; 66];
        let n = data.len().min(buf.len() - 2);
        buf[0] = 0xa1;
        buf[1] = report_id;
        buf[2..2 + n].copy_from_slice(&data[..n]);
        self.ingest_report(slot, &buf[..2 + n], out);
    }

    fn on_le_closed(&mut self, le_handle: u16, out: &mut impl Emit) {
        if let Some(slot) = self.slot_by_le_handle(le_handle) {
            out.emit(Command::ShmDisconnected {
                slot: u8::try_from(slot).expect("bounded pad slot"),
            });
            self.pads[slot] = PadSlot::EMPTY;
            out.emit(Command::LeReArm);
        }
    }

    fn on_opened(
        &mut self,
        cid: u16,
        addr: BdAddr,
        status: u8,
        descriptor: &[u8],
        out: &mut impl Emit,
    ) {
        self.connecting = false;
        if status != STATUS_SUCCESS {
            if self.redial {
                self.redial = false; // one dial-back per attempt
                self.target = Some(addr);
                out.emit(Command::HidConnect { addr });
                return;
            }
            // Give up - never leave a half-open ACL (the pad thinks it is
            // connected, solid light, and won't re-page). Drop it by address.
            out.emit(Command::GapDisconnectAcl { addr });
            self.target = None;
            return;
        }
        self.redial = false;
        // Route by descriptor. Empty on an incoming reconnect (SDP not done
        // yet) -> defer to HidDescriptorAvailable.
        self.route(cid, addr, descriptor, false, out);
    }

    fn route(
        &mut self,
        cid: u16,
        addr: BdAddr,
        descriptor: &[u8],
        final_: bool,
        out: &mut impl Emit,
    ) {
        let app = if descriptor.is_empty() {
            AppUsages::default()
        } else {
            app_usages(descriptor)
        };
        // kbm path: a keyboard/mouse that is not also a pad. (The C also
        // reroutes a descriptor-less reconnect of a remembered kbm address;
        // that address memory belongs to the kbm module, added with it.)
        if self.config.kbm_enabled && (app.kbd || app.mouse) && !app.pad {
            out.emit(Command::RouteKbm {
                cid,
                addr,
                kbd: app.kbd,
                mouse: app.mouse,
            });
            return;
        }
        if !final_ && descriptor.is_empty() {
            self.pending = Some((cid, addr));
            return;
        }
        // pad path
        match self.alloc() {
            None => out.emit(Command::HidDisconnect { cid }),
            Some(slot) => {
                self.pads[slot] = PadSlot {
                    used: true,
                    cid,
                    addr,
                    ..PadSlot::EMPTY
                };
                if self.pads_full() {
                    out.emit(Command::LeSuspend); // pad table full -> quiesce LE
                }
                out.emit(Command::SetTimer {
                    slot: u8::try_from(slot).expect("bounded pad slot"),
                    ms: SETUP_FIRST_MS,
                });
            }
        }
    }

    fn on_setup_tick(&mut self, slot: u8, out: &mut impl Emit) {
        let p = match self.pads.get_mut(slot as usize) {
            Some(p) if p.used && !p.is_le => p, // LE pads need no nudge/full-report poke
            _ => return,
        };
        let cid = p.cid;
        if p.pad_type.is_none() {
            // Silent link -> almost certainly a DS3 waiting for the 0xF4 nudge.
            // Alternate enable / LED across ticks (one SET_REPORT handshake per
            // round-trip; a second in the same tick is refused 0x0c). Bounded.
            if p.ds3_start_tries < DS3_START_MAX_TRIES {
                p.ds3_start_tries += 1;
                if p.ds3_start_tries & 1 == 1 {
                    out.emit(Command::Ds3SendStart { cid });
                } else {
                    out.emit(Command::Ds3SetLed { cid, slot });
                }
            }
            out.emit(Command::SetTimer {
                slot,
                ms: SETUP_NUDGE_MS,
            });
            return;
        }
        if p.pad_type == Some(PadType::Ds4) && !p.full_report_active {
            out.emit(Command::Ds4EnableFullReport { cid });
            out.emit(Command::SetTimer {
                slot,
                ms: SETUP_DS4_FULL_MS,
            });
        }
        // Classified DS3/Xbox: nothing left; the timer stops (no re-arm).
    }

    fn on_report(&mut self, cid: u16, data: &[u8], out: &mut impl Emit) {
        // Reports for a still-deferred connection are dropped (we route on the
        // descriptor event, not the first report, to avoid misrouting a
        // keyboard to the pad path).
        if self.pending.map(|(c, _)| c) == Some(cid) {
            return;
        }
        let Some(slot) = self.slot_by_cid(cid) else {
            return;
        };
        self.ingest_report(slot, data, out);
    }

    /// Shared report path: classify on first report, then parse + publish.
    /// Used by both Classic (`on_report`) and LE (`on_le_report`) - the report
    /// bytes are already framed as `0xa1, id, payload` in both cases.
    fn ingest_report(&mut self, slot: usize, data: &[u8], out: &mut impl Emit) {
        let layout = self.config.xbox_layout;
        let is_le = self.pads[slot].is_le;
        let cid = self.pads[slot].cid;

        // Classify on the first report.
        if self.pads[slot].pad_type.is_none() {
            match classify(data) {
                None => return, // unclassified: ignore, stay UNKNOWN (never connect)
                Some(t) => {
                    self.pads[slot].pad_type = Some(t);
                    // pad_type before connected: the classify-before-connected
                    // invariant.
                    out.emit(Command::ShmConnected {
                        slot: u8::try_from(slot).expect("bounded pad slot"),
                        pad_type: t,
                    });
                    // Classic-only side effects (SDP DeviceID query; DS3 LED).
                    if !is_le {
                        out.emit(Command::DidQuery {
                            addr: self.pads[slot].addr,
                        });
                        if t == PadType::Ds3 {
                            out.emit(Command::Ds3SetLed {
                                cid,
                                slot: u8::try_from(slot).expect("bounded pad slot"),
                            });
                        }
                    }
                }
            }
        }
        let t = self.pads[slot].pad_type.unwrap();

        // Xbox old-fw guide report (0x02): update the flag and re-publish the
        // last state with its Guide button bit updated.
        if t == PadType::Xbox {
            if let Some(g) = parse_xbox_guide(data) {
                self.pads[slot].xbox_guide = g;
                if let Some(mut st) = self.pads[slot].last_state {
                    if g {
                        st.buttons |= 1 << BTN_PS;
                    } else {
                        st.buttons &= !(1 << BTN_PS);
                    }
                    self.pads[slot].last_state = Some(st);
                    out.emit(Command::ShmPublish {
                        slot: u8::try_from(slot).expect("bounded pad slot"),
                        state: st,
                    });
                }
                return;
            }
        }

        let guide = self.pads[slot].xbox_guide;
        if let Some(st) = parse_for_type(data, t, layout, guide) {
            if t == PadType::Ds4 && st.full_report {
                self.pads[slot].full_report_active = true;
            }
            self.pads[slot].last_state = Some(st);
            out.emit(Command::ShmPublish {
                slot: u8::try_from(slot).expect("bounded pad slot"),
                state: st,
            });
        }
    }

    fn on_closed(&mut self, cid: u16, out: &mut impl Emit) {
        if self.pending.map(|(c, _)| c) == Some(cid) {
            self.pending = None;
        }
        if let Some(slot) = self.slot_by_cid(cid) {
            out.emit(Command::ClearTimer {
                slot: u8::try_from(slot).expect("bounded pad slot"),
            });
            out.emit(Command::ShmDisconnected {
                slot: u8::try_from(slot).expect("bounded pad slot"),
            });
            self.pads[slot] = PadSlot::EMPTY;
            self.connecting = false;
            self.target = None;
            out.emit(Command::LeReArm);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive(core: &mut PadCore, ev: Event) -> Vec<Command> {
        let mut out = Vec::new();
        core.on_event(ev, &mut |c: Command| out.push(c));
        out
    }

    fn ds4_simple_neutral() -> [u8; 9] {
        [0xa1, 0x01, 0x80, 0x80, 0x80, 0x80, 0x08, 0x00, 0x00]
    }

    fn open_a_pad(core: &mut PadCore, cid: u16, addr: BdAddr) -> Vec<Command> {
        // Faithful reconnect flow: OPENED with no descriptor yet -> defer ->
        // DESCRIPTOR_AVAILABLE (empty, final) -> pad path allocates. Returns
        // the routing commands (SetTimer etc.).
        drive(
            core,
            Event::HidOpened {
                cid,
                addr,
                status: STATUS_SUCCESS,
                descriptor: &[],
            },
        );
        drive(
            core,
            Event::HidDescriptorAvailable {
                cid,
                descriptor: &[],
            },
        )
    }

    #[test]
    fn classify_before_connected() {
        let mut core = PadCore::new(Config::default());
        let addr = [1, 2, 3, 4, 5, 6];
        let cmds = open_a_pad(&mut core, 0x40, addr);
        // pad allocated, setup timer armed
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::SetTimer { slot: 0, .. })));

        // first DS4 report: ShmConnected(DS4) MUST precede ShmPublish
        let cmds = drive(
            &mut core,
            Event::HidReport {
                cid: 0x40,
                data: &ds4_simple_neutral(),
            },
        );
        let conn = cmds.iter().position(|c| {
            matches!(
                c,
                Command::ShmConnected {
                    pad_type: PadType::Ds4,
                    ..
                }
            )
        });
        let publ = cmds
            .iter()
            .position(|c| matches!(c, Command::ShmPublish { .. }));
        assert!(conn.is_some(), "expected ShmConnected");
        assert!(publ.is_some(), "expected ShmPublish");
        assert!(
            conn.unwrap() < publ.unwrap(),
            "connected must precede publish"
        );
    }

    #[test]
    fn unclassified_report_never_connects() {
        let mut core = PadCore::new(Config::default());
        open_a_pad(&mut core, 0x40, [1, 2, 3, 4, 5, 6]);
        // id 0x01 len 30 -> unclassified (between Xbox<=20 and DS3>=40)
        let mut junk = vec![0u8; 30];
        junk[0] = 0xa1;
        junk[1] = 0x01;
        let cmds = drive(
            &mut core,
            Event::HidReport {
                cid: 0x40,
                data: &junk,
            },
        );
        assert!(!cmds
            .iter()
            .any(|c| matches!(c, Command::ShmConnected { .. })));
        assert!(!cmds.iter().any(|c| matches!(c, Command::ShmPublish { .. })));
    }

    #[test]
    fn half_open_acl_redial_then_disconnect() {
        let mut core = PadCore::new(Config::default());
        let addr = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];

        // incoming -> accept + arm redial
        let cmds = drive(&mut core, Event::HidIncoming { cid: 0x50, addr });
        assert_eq!(cmds, vec![Command::HidAccept { cid: 0x50 }]);

        // first open fails -> exactly one dial-back
        let cmds = drive(
            &mut core,
            Event::HidOpened {
                cid: 0x50,
                addr,
                status: 0x11,
                descriptor: &[],
            },
        );
        assert_eq!(cmds, vec![Command::HidConnect { addr }]);

        // second failure -> drop the half-open ACL, no more redials
        let cmds = drive(
            &mut core,
            Event::HidOpened {
                cid: 0x50,
                addr,
                status: 0x69,
                descriptor: &[],
            },
        );
        assert_eq!(cmds, vec![Command::GapDisconnectAcl { addr }]);
    }

    #[test]
    fn ds3_nudge_alternates_and_is_bounded() {
        let mut core = PadCore::new(Config::default());
        open_a_pad(&mut core, 0x40, [1, 2, 3, 4, 5, 6]);

        let mut starts = 0;
        let mut leds = 0;
        // Fire many ticks while the pad stays UNKNOWN (never sends a report).
        for _ in 0..40 {
            let cmds = drive(&mut core, Event::SetupTick { slot: 0 });
            for c in &cmds {
                match c {
                    Command::Ds3SendStart { .. } => starts += 1,
                    Command::Ds3SetLed { .. } => leds += 1,
                    _ => {}
                }
            }
            // always re-arms while unknown
            assert!(cmds.iter().any(|c| matches!(c, Command::SetTimer { .. })));
        }
        // bounded to DS3_START_MAX_TRIES total pokes, alternating start/led
        assert_eq!(starts + leds, DS3_START_MAX_TRIES as usize);
        assert_eq!(starts, 5); // odd tries -> start
        assert_eq!(leds, 5); // even tries -> led
    }

    #[test]
    fn ds4_full_report_enable_then_quiesces() {
        let mut core = PadCore::new(Config::default());
        open_a_pad(&mut core, 0x40, [1, 2, 3, 4, 5, 6]);
        // classify as DS4
        drive(
            &mut core,
            Event::HidReport {
                cid: 0x40,
                data: &ds4_simple_neutral(),
            },
        );
        // tick -> ask for full report + re-arm
        let cmds = drive(&mut core, Event::SetupTick { slot: 0 });
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::Ds4EnableFullReport { .. })));
        assert!(cmds.iter().any(|c| matches!(c, Command::SetTimer { .. })));

        // a full (0x11) report arrives -> full_report_active
        let mut full = vec![0u8; 4 + 46];
        full[0] = 0xa1;
        full[1] = 0x11;
        full[4] = 0x80;
        full[5] = 0x80;
        full[6] = 0x80;
        full[7] = 0x80;
        full[8] = 0x08;
        drive(
            &mut core,
            Event::HidReport {
                cid: 0x40,
                data: &full,
            },
        );

        // now a tick does nothing (no enable, no re-arm) - timer stops
        let cmds = drive(&mut core, Event::SetupTick { slot: 0 });
        assert_eq!(cmds, vec![]);
    }

    #[test]
    fn slot_full_drops_new_connection() {
        let mut core = PadCore::new(Config::default());
        for i in 0..u16::try_from(MAX_PADS).unwrap() {
            open_a_pad(
                &mut core,
                0x40 + i,
                [u8::try_from(i).unwrap(), 0, 0, 0, 0, 0],
            );
        }
        assert_eq!(core.pad_count(), MAX_PADS);
        // one more -> HidDisconnect, no slot
        let cmds = open_a_pad(&mut core, 0x99, [9, 9, 9, 9, 9, 9]);
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::HidDisconnect { cid: 0x99 })));
    }

    #[test]
    fn close_frees_slot_and_rearms_le() {
        let mut core = PadCore::new(Config::default());
        open_a_pad(&mut core, 0x40, [1, 2, 3, 4, 5, 6]);
        assert_eq!(core.pad_count(), 1);
        let cmds = drive(&mut core, Event::HidClosed { cid: 0x40 });
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::ClearTimer { slot: 0 })));
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::ShmDisconnected { slot: 0 })));
        assert!(cmds.iter().any(|c| matches!(c, Command::LeReArm)));
        assert_eq!(core.pad_count(), 0);
    }

    // ---- LE / HOGP ---------------------------------------------------------
    fn xbox_le_report_payload() -> Vec<u8> {
        // 15-byte Series-style payload (no 0xa1/id header - LE strips it), A held
        let mut b = vec![0u8; 15];
        for off in [0usize, 2, 4, 6] {
            b[off + 1] = 0x80; // center sticks (u16 LE, high byte 0x80)
        }
        b[13] = 0x01; // A
        b
    }

    #[test]
    fn le_connect_report_classifies_xbox_and_publishes() {
        let mut core = PadCore::new(Config::default());
        // shell-driven LE lifecycle
        let c = drive(
            &mut core,
            Event::LeConnected {
                le_handle: 0x40,
                addr: [1, 2, 3, 4, 5, 6],
            },
        );
        assert_eq!(c, vec![]); // slot reserved, no commands yet
        assert_eq!(core.pad_count(), 1);
        drive(
            &mut core,
            Event::LeHidsCid {
                le_handle: 0x40,
                hids_cid: 0x0111,
            },
        );

        // first HOGP report (id 0x01) -> classify Xbox, connected precedes publish
        let cmds = drive(
            &mut core,
            Event::LeReport {
                hids_cid: 0x0111,
                report_id: 0x01,
                data: &xbox_le_report_payload(),
            },
        );
        let conn = cmds.iter().position(|c| {
            matches!(
                c,
                Command::ShmConnected {
                    pad_type: PadType::Xbox,
                    ..
                }
            )
        });
        let publ = cmds
            .iter()
            .position(|c| matches!(c, Command::ShmPublish { .. }));
        assert!(conn.is_some() && publ.is_some());
        assert!(conn.unwrap() < publ.unwrap());
        // LE classify must NOT emit Classic-only DidQuery / Ds3SetLed
        assert!(!cmds
            .iter()
            .any(|c| matches!(c, Command::DidQuery { .. } | Command::Ds3SetLed { .. })));
        // the published state has A -> CROSS
        let st = cmds
            .iter()
            .find_map(|c| match c {
                Command::ShmPublish { state, .. } => Some(state),
                _ => None,
            })
            .unwrap();
        assert!(st.button(crate::report::BTN_CROSS));
    }

    #[test]
    fn le_raw_report_publishes_then_stands_down() {
        let mut core = PadCore::new(Config::default());
        drive(
            &mut core,
            Event::LeConnected {
                le_handle: 0x40,
                addr: [1, 2, 3, 4, 5, 6],
            },
        );
        drive(
            &mut core,
            Event::LeHidsCid {
                le_handle: 0x40,
                hids_cid: 0x0111,
            },
        );
        // raw notification classifies + publishes (the Xbox-BLE path)
        let cmds = drive(
            &mut core,
            Event::LeRawReport {
                hids_cid: 0x0111,
                report_id: 0x01,
                data: &xbox_le_report_payload(),
            },
        );
        assert!(cmds.iter().any(|c| matches!(c, Command::ShmPublish { .. })));
        // once hids_client's own delivery is seen, raw stands down
        drive(
            &mut core,
            Event::LeReport {
                hids_cid: 0x0111,
                report_id: 0x01,
                data: &xbox_le_report_payload(),
            },
        );
        let cmds = drive(
            &mut core,
            Event::LeRawReport {
                hids_cid: 0x0111,
                report_id: 0x01,
                data: &xbox_le_report_payload(),
            },
        );
        assert!(!cmds.iter().any(|c| matches!(c, Command::ShmPublish { .. })));
    }

    #[test]
    fn le_decide_policy() {
        let a1 = [1, 1, 1, 1, 1, 1];
        let a2 = [2, 2, 2, 2, 2, 2];

        // no bonds anywhere -> bootstrap pairing scan
        let mut core = PadCore::new(Config::default());
        assert_eq!(
            core.le_decide(false, false, 0, false),
            LeAction::Scan { pairing: true }
        );

        // one bond, not connected -> accept-list connect, and the bond is
        // what goes on the list
        core.add_le_bond(a1, 0);
        assert_eq!(
            core.le_decide(false, false, 0, false),
            LeAction::WhitelistConnect
        );
        assert_eq!(core.missing_le_bonds().collect::<Vec<_>>(), [(a1, 0)]);

        // busy (a directed connect in flight) -> idle regardless
        assert_eq!(core.le_decide(true, false, 0, false), LeAction::Idle);

        // that bond now connected -> all on -> idle (bonds exist, no scan)
        drive(
            &mut core,
            Event::LeConnected {
                le_handle: 0x40,
                addr: a1,
            },
        );
        assert_eq!(core.le_decide(false, false, 0, false), LeAction::Idle);
        assert_eq!(core.missing_le_bonds().count(), 0);

        // a second bond missing -> accept-list holds exactly the missing one
        core.add_le_bond(a2, 1);
        assert_eq!(
            core.le_decide(false, false, 0, false),
            LeAction::WhitelistConnect
        );
        assert_eq!(core.missing_le_bonds().collect::<Vec<_>>(), [(a2, 1)]);

        // both missing -> both on the list, still one accept-list connect
        drive(&mut core, Event::LeClosed { le_handle: 0x40 });
        assert_eq!(
            core.le_decide(false, false, 0, false),
            LeAction::WhitelistConnect
        );
        assert_eq!(
            core.missing_le_bonds().collect::<Vec<_>>(),
            [(a1, 0), (a2, 1)]
        );

        // pads all connected but a kbm bond is missing -> still accept-list
        drive(
            &mut core,
            Event::LeConnected {
                le_handle: 0x40,
                addr: a1,
            },
        );
        drive(
            &mut core,
            Event::LeConnected {
                le_handle: 0x41,
                addr: a2,
            },
        );
        assert_eq!(
            core.le_decide(false, false, 1, true),
            LeAction::WhitelistConnect
        );
        // ... but with every bond (pad + kbm) connected -> idle
        assert_eq!(core.le_decide(false, false, 1, false), LeAction::Idle);

        // force_pair always scans (pairing), bonds or not
        assert_eq!(
            core.le_decide(false, true, 1, true),
            LeAction::Scan { pairing: true }
        );

        assert!(core.is_le_bond(&a1));
        assert!(!core.is_le_bond(&[9, 9, 9, 9, 9, 9]));
    }

    #[test]
    fn le_missing_bonds_empty_when_pads_full() {
        // A full pad table has no slot to land a reconnect in - the pad half
        // of the accept-list must go empty (kbm reconnect stays the shell's).
        let mut core = PadCore::new(Config::default());
        let bonded = [7, 7, 7, 7, 7, 7];
        core.add_le_bond(bonded, 0);
        for h in 0..u16::try_from(MAX_PADS).unwrap() {
            drive(
                &mut core,
                Event::LeConnected {
                    le_handle: 0x50 + h,
                    addr: [8, 8, 8, 8, 8, u8::try_from(h).unwrap()],
                },
            );
        }
        assert_eq!(core.missing_le_bonds().count(), 0);
        // kbm side still drives an accept-list connect
        assert_eq!(
            core.le_decide(false, false, 1, true),
            LeAction::WhitelistConnect
        );
        // and with the kbm side whole, a full table idles
        assert_eq!(core.le_decide(false, false, 1, false), LeAction::Idle);
    }

    #[test]
    fn le_pad_no_setup_tick() {
        let mut core = PadCore::new(Config::default());
        drive(
            &mut core,
            Event::LeConnected {
                le_handle: 0x40,
                addr: [1, 2, 3, 4, 5, 6],
            },
        );
        // a stray setup tick on an LE slot does nothing (no DS3 nudge)
        let cmds = drive(&mut core, Event::SetupTick { slot: 0 });
        assert_eq!(cmds, vec![]);
    }

    #[test]
    fn le_close_frees_slot() {
        let mut core = PadCore::new(Config::default());
        drive(
            &mut core,
            Event::LeConnected {
                le_handle: 0x40,
                addr: [1, 2, 3, 4, 5, 6],
            },
        );
        assert_eq!(core.pad_count(), 1);
        let cmds = drive(&mut core, Event::LeClosed { le_handle: 0x40 });
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::ShmDisconnected { slot: 0 })));
        assert!(cmds.iter().any(|c| matches!(c, Command::LeReArm)));
        assert_eq!(core.pad_count(), 0);
    }

    #[test]
    fn le_and_classic_share_the_slot_table() {
        let mut core = PadCore::new(Config::default());
        open_a_pad(&mut core, 0x40, [1, 1, 1, 1, 1, 1]); // Classic in slot 0
        drive(
            &mut core,
            Event::LeConnected {
                le_handle: 0x50,
                addr: [2, 2, 2, 2, 2, 2],
            },
        ); // LE in slot 1
        assert_eq!(core.pad_count(), 2);
    }

    #[test]
    fn keyboard_descriptor_routes_to_kbm_not_pad() {
        let mut core = PadCore::new(Config::default());
        // keyboard app-collection descriptor
        let kbd = &[0x05u8, 0x01, 0x09, 0x06, 0xA1, 0x01, 0xC0];
        let cmds = drive(
            &mut core,
            Event::HidOpened {
                cid: 0x40,
                addr: [1, 2, 3, 4, 5, 6],
                status: STATUS_SUCCESS,
                descriptor: kbd,
            },
        );
        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::RouteKbm { kbd: true, .. })));
        assert!(!cmds.iter().any(|c| matches!(c, Command::SetTimer { .. })));
        assert_eq!(core.pad_count(), 0);
    }

    #[test]
    fn xbox_guide_report_republishes_with_ps_bit() {
        let mut core = PadCore::new(Config::default());
        open_a_pad(&mut core, 0x40, [1, 2, 3, 4, 5, 6]);
        // classify as Xbox with a 16-byte 0x01 report, no buttons
        let mut xr = vec![0u8; 17];
        xr[0] = 0xa1;
        xr[1] = 0x01;
        for off in [2usize, 4, 6, 8] {
            xr[off + 1] = 0x80; // center sticks
        }
        drive(
            &mut core,
            Event::HidReport {
                cid: 0x40,
                data: &xr,
            },
        );
        // now an old-fw guide report 0x02 with guide down
        let cmds = drive(
            &mut core,
            Event::HidReport {
                cid: 0x40,
                data: &[0xa1, 0x02, 0x01],
            },
        );
        // re-publish with PS/guide bit set
        let published = cmds.iter().find_map(|c| match c {
            Command::ShmPublish { state, .. } => Some(state),
            _ => None,
        });
        assert!(published.is_some());
        assert!(published.unwrap().button(BTN_PS));
    }

    #[test]
    fn rumble_route_classic_after_classify() {
        let mut core = PadCore::new(Config::default());
        assert_eq!(core.rumble_route(0), None); // empty slot
        open_a_pad(&mut core, 0x40, [1, 2, 3, 4, 5, 6]);
        assert_eq!(core.rumble_route(0), None); // unclassified: no route yet
        drive(
            &mut core,
            Event::HidReport {
                cid: 0x40,
                data: &ds4_simple_neutral(),
            },
        );
        assert_eq!(
            core.rumble_route(0),
            Some((PadType::Ds4, RumbleRoute::Classic { cid: 0x40 }))
        );
        // slot close drops the route
        drive(&mut core, Event::HidClosed { cid: 0x40 });
        assert_eq!(core.rumble_route(0), None);
    }

    #[test]
    fn rumble_route_le_needs_hids_cid() {
        let mut core = PadCore::new(Config::default());
        drive(
            &mut core,
            Event::LeConnected {
                le_handle: 0x40,
                addr: [1, 2, 3, 4, 5, 6],
            },
        );
        assert_eq!(core.rumble_route(0), None); // no HOGP client yet
        drive(
            &mut core,
            Event::LeHidsCid {
                le_handle: 0x40,
                hids_cid: 0x0111,
            },
        );
        assert_eq!(core.rumble_route(0), None); // still unclassified
        drive(
            &mut core,
            Event::LeRawReport {
                hids_cid: 0x0111,
                report_id: 0x01,
                data: &xbox_le_report_payload(),
            },
        );
        assert_eq!(
            core.rumble_route(0),
            Some((PadType::Xbox, RumbleRoute::Le { hids_cid: 0x0111 }))
        );
    }
}
