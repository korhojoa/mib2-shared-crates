// SPDX-License-Identifier: MIT OR Apache-2.0
//! Allocation-free wireless Android Auto policy parser.
//! Missing, malformed, expired or future-dated leases never grant access.
//! The RFCOMM grant authorizes a BT peer; it does not identify a TCP socket.

pub type Address = [u8; 6];
pub const LEASE_MS: u64 = 10_000;
pub const MAX_PHONES: usize = 64;

#[derive(Clone, Debug)]
pub struct Policy {
    pub revision: u64,
    pub epoch: u64,
    pub updated_ms: u64,
    pub enabled: bool,
    pub selected: Option<Address>,
    allowed: [Address; MAX_PHONES],
    len: usize,
}

#[must_use]
pub fn address(text: &str) -> Option<Address> {
    if text.len() != 17 {
        return None;
    }
    let b = text.as_bytes();
    let mut a = [0; 6];
    for (i, byte) in a.iter_mut().enumerate() {
        if i > 0 && b[i * 3 - 1] != b':' {
            return None;
        }
        *byte = hex(b[i * 3])? * 16 + hex(b[i * 3 + 1])?;
    }
    Some(a)
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn number(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[must_use]
pub fn fresh(updated: u64, now: u64) -> bool {
    now.checked_sub(updated).is_some_and(|age| age <= LEASE_MS)
}

impl Policy {
    #[must_use]
    pub fn parse(text: &str, now: u64) -> Option<Self> {
        if text.len() > 4096 || !text.ends_with("\nend=1\n") {
            return None;
        }
        let mut lines = text.split_terminator('\n');
        if lines.next()? != "version=1" {
            return None;
        }
        let revision = number(lines.next()?.strip_prefix("revision=")?)?;
        let epoch = number(lines.next()?.strip_prefix("epoch=")?)?;
        let updated_ms = number(lines.next()?.strip_prefix("updated_ms=")?)?;
        if revision == 0 || epoch == 0 || !fresh(updated_ms, now) {
            return None;
        }
        let enabled = match lines.next()? {
            "enabled=0" => false,
            "enabled=1" => true,
            _ => return None,
        };
        let selected = match lines.next()?.strip_prefix("selected=")? {
            "-" => None,
            value => Some(address(value)?),
        };
        let mut p = Self {
            revision,
            epoch,
            updated_ms,
            enabled,
            selected,
            allowed: [[0; 6]; MAX_PHONES],
            len: 0,
        };
        while let Some(line) = lines.next() {
            if line == "end=1" {
                if lines.next().is_some() || (p.selected.is_some() && p.candidate().is_none()) {
                    return None;
                }
                return Some(p);
            }
            let a = address(line.strip_prefix("allow=")?)?;
            if p.len == MAX_PHONES || p.allowed[..p.len].contains(&a) {
                return None;
            }
            p.allowed[p.len] = a;
            p.len += 1;
        }
        None
    }

    #[must_use]
    pub fn permits(&self, peer: &Address) -> bool {
        self.enabled && self.allowed[..self.len].contains(peer)
    }

    #[must_use]
    pub fn candidate(&self) -> Option<Address> {
        self.selected.filter(|a| self.permits(a))
    }

    #[must_use]
    pub fn accepts_bluetooth(&self, peer: &Address) -> bool {
        self.candidate().as_ref() == Some(peer)
    }

    #[must_use]
    pub fn accepts_session(&self, session: &Session) -> bool {
        self.epoch == session.epoch && self.accepts_bluetooth(&session.peer)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Session {
    pub epoch: u64,
    pub peer: Address,
    pub id: u64,
}

impl Session {
    #[must_use]
    pub fn parse(text: &str, now: u64) -> Option<Self> {
        if text.len() > 256 {
            return None;
        }
        let mut lines = text.split_terminator('\n');
        if lines.next()? != "version=1" {
            return None;
        }
        let epoch = number(lines.next()?.strip_prefix("epoch=")?)?;
        let updated_ms = number(lines.next()?.strip_prefix("updated_ms=")?)?;
        let peer = address(lines.next()?.strip_prefix("peer=")?)?;
        let id = number(lines.next()?.strip_prefix("session=")?)?;
        if id == 0
            || !fresh(updated_ms, now)
            || lines.next()? != "end=1"
            || lines.next().is_some()
            || !text.ends_with('\n')
        {
            return None;
        }
        Some(Self { epoch, peer, id })
    }
}

/// `BTstack` serializes `bd_addr_t` backwards in HCI events.
#[must_use]
pub fn event_address(event: &[u8], offset: usize) -> Option<Address> {
    let bytes = event.get(offset..offset.checked_add(6)?)?;
    Some([bytes[5], bytes[4], bytes[3], bytes[2], bytes[1], bytes[0]])
}

/// Why the AAWG RFCOMM server refuses an incoming channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decline {
    /// An AAWG channel is already open.
    SessionUp,
    /// An earlier incoming channel is accepted and not yet open.
    AcceptPending,
    /// The event carries no remote address.
    NoPeer,
    /// No fresh policy lease is published.
    NoPolicy,
    /// The policy does not select and allow this peer.
    NotPermitted,
    /// Too many failed handshakes; quiet for this many more seconds.
    Cooldown { quiet_s: u64 },
}

impl Decline {
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Decline::SessionUp => "busy: an AAWG channel is already up",
            Decline::AcceptPending => "busy: another AAWG channel is being accepted",
            Decline::NoPeer => "no peer address in the event",
            Decline::NoPolicy => "no live wireless policy",
            Decline::NotPermitted => "peer is not the permitted phone",
            Decline::Cooldown { .. } => "cooldown after failed handshakes",
        }
    }
}

/// Decision for an incoming AAWG channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admit {
    /// Accept. `after_cooldown` is true when this is the one attempt allowed
    /// after a cooldown ended.
    Accept {
        after_cooldown: bool,
    },
    Decline(Decline),
}

/// State the AAWG server has when an incoming channel arrives.
#[derive(Clone, Copy, Debug)]
pub struct Incoming<'a> {
    pub channel_up: bool,
    pub accept_pending: bool,
    pub peer: Option<Address>,
    pub policy: Option<&'a Policy>,
    pub fail_streak: u32,
    pub fail_max: u32,
    pub now_ms: u64,
    pub cooldown_until_ms: u64,
}

/// Accept or decline an incoming AAWG channel. The order of the checks is
/// the order the server used before the reasons were logged.
#[must_use]
pub fn admit(i: &Incoming) -> Admit {
    if i.channel_up {
        return Admit::Decline(Decline::SessionUp);
    }
    if i.accept_pending {
        return Admit::Decline(Decline::AcceptPending);
    }
    let Some(peer) = i.peer else {
        return Admit::Decline(Decline::NoPeer);
    };
    let Some(policy) = i.policy else {
        return Admit::Decline(Decline::NoPolicy);
    };
    if !policy.accepts_bluetooth(&peer) {
        return Admit::Decline(Decline::NotPermitted);
    }
    if i.fail_streak >= i.fail_max {
        if i.now_ms < i.cooldown_until_ms {
            let quiet_s = (i.cooldown_until_ms - i.now_ms).div_ceil(1000);
            return Admit::Decline(Decline::Cooldown { quiet_s });
        }
        return Admit::Accept {
            after_cooldown: true,
        };
    }
    Admit::Accept {
        after_cooldown: false,
    }
}

/// A wireless AA attempt: a nudge round started for a phone that has not yet
/// reached the end of the AAWG handshake. A Bluetooth controller recovery
/// closes every link, so an attempt that is still open at that time is lost
/// unless it is started again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Attempt {
    peer: Option<Address>,
}

/// What to do with an open attempt after Bluetooth is back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resume {
    /// Start one new nudge round for this phone.
    Nudge(Address),
    /// Do nothing, for this reason.
    Skip(&'static str),
}

impl Attempt {
    #[must_use]
    pub const fn new() -> Self {
        Self { peer: None }
    }

    /// A nudge round started for `peer`.
    pub fn start(&mut self, peer: Address) {
        self.peer = Some(peer);
    }

    /// The handshake ended (pass or fail), the phone closed the channel, or
    /// the target phone changed.
    pub fn end(&mut self) {
        self.peer = None;
    }

    #[must_use]
    pub fn peer(&self) -> Option<Address> {
        self.peer
    }

    /// Called once per completed controller recovery. `target` is the phone
    /// the live policy selects and permits now. `channel_up` is true while an
    /// AAWG channel is open or being accepted.
    #[must_use]
    pub fn after_recovery(&self, target: Option<Address>, channel_up: bool) -> Resume {
        let Some(peer) = self.peer else {
            return Resume::Skip("no attempt in progress");
        };
        if channel_up {
            return Resume::Skip("an AAWG channel is already up");
        }
        if target != Some(peer) {
            return Resume::Skip("the phone is no longer the permitted one");
        }
        Resume::Nudge(peer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: &str = "01:23:45:67:89:AB";
    const B: &str = "12:34:56:78:9A:BC";
    fn policy(enabled: bool, selected: &str, allowed: &str) -> String {
        format!(
            "version=1\nrevision=8\nepoch=3\nupdated_ms=10000\nenabled={}\nselected={}\n{}end=1\n",
            u8::from(enabled),
            selected,
            allowed
        )
    }
    fn allowed() -> String {
        format!("allow={A}\nallow={B}\n")
    }

    #[test]
    fn explicit_permission_and_selected_phone_required() {
        let p = Policy::parse(&policy(true, A, &allowed()), 11000).unwrap();
        assert!(p.accepts_bluetooth(&address(A).unwrap()));
        assert!(!p.accepts_bluetooth(&address(B).unwrap()));
        assert!(!p.accepts_bluetooth(&[0; 6]));
        let off = Policy::parse(&policy(false, "-", &allowed()), 11000).unwrap();
        assert_eq!(off.candidate(), None);
        assert!(!off.permits(&address(A).unwrap()));
    }

    #[test]
    fn expiry_clock_rollback_malformed_and_partial_files_deny() {
        let text = policy(true, A, &allowed());
        assert!(Policy::parse(&text, 20000).is_some());
        assert!(Policy::parse(&text, 20001).is_none());
        assert!(Policy::parse(&text, 9999).is_none());
        for i in 0..text.len() {
            assert!(Policy::parse(&text[..i], 11000).is_none());
        }
        for from in ["version=1", "revision=8", "enabled=1", "end=1"] {
            assert!(Policy::parse(&text.replace(from, "unknown=1"), 11000).is_none());
        }
        assert!(Policy::parse(&(text.clone() + "garbage\n"), 11000).is_none());
        assert!(Policy::parse(&text.replace("revision=8", "revision=+8"), 11000).is_none());
    }

    #[test]
    fn duplicates_and_unlisted_selection_deny() {
        assert!(
            Policy::parse(&policy(true, A, &format!("allow={A}\nallow={A}\n")), 11000).is_none()
        );
        assert!(Policy::parse(&policy(true, A, &format!("allow={B}\n")), 11000).is_none());
        assert!(Policy::parse(&policy(false, A, &allowed()), 11000).is_none());
        assert!(address("01:23:45:67:89:ab").is_none());
        assert!(address("1:23:45:67:89:AB").is_none());
    }

    #[test]
    fn session_must_be_fresh_and_match_current_policy_and_peer() {
        let p = Policy::parse(&policy(true, A, &allowed()), 11000).unwrap();
        let text = format!("version=1\nepoch=3\nupdated_ms=10000\npeer={A}\nsession=123\nend=1\n");
        let s = Session::parse(&text, 11000).unwrap();
        assert!(p.accepts_session(&s));
        assert!(!p.accepts_session(&Session { epoch: 2, ..s }));
        let mut changed_other_phone = p.clone();
        changed_other_phone.revision += 1;
        assert!(changed_other_phone.accepts_session(&s));
        assert!(!p.accepts_session(&Session {
            peer: address(B).unwrap(),
            ..s
        }));
        assert!(Session::parse(&text, 20001).is_none());
        assert!(Session::parse(&text, 9999).is_none());
        for i in 0..text.len() {
            assert!(Session::parse(&text[..i], 11000).is_none());
        }
    }

    #[test]
    fn event_identity_uses_real_remote_address_and_checks_bounds() {
        let incoming = [0x81, 9, 0xAB, 0x89, 0x67, 0x45, 0x23, 0x01, 1, 9, 0];
        assert_eq!(event_address(&incoming, 2), address(A));
        assert_eq!(event_address(&incoming[..7], 2), None);
        assert_eq!(event_address(&incoming, usize::MAX), None);
    }

    fn incoming<'a>(p: Option<&'a Policy>, peer: &str) -> Incoming<'a> {
        Incoming {
            channel_up: false,
            accept_pending: false,
            peer: address(peer),
            policy: p,
            fail_streak: 0,
            fail_max: 3,
            now_ms: 1000,
            cooldown_until_ms: 0,
        }
    }

    #[test]
    fn admit_gives_a_reason_for_every_decline() {
        let p = Policy::parse(&policy(true, A, &allowed()), 11000).unwrap();
        let ok = incoming(Some(&p), A);
        assert_eq!(
            admit(&ok),
            Admit::Accept {
                after_cooldown: false
            }
        );
        assert_eq!(
            admit(&Incoming {
                channel_up: true,
                ..ok
            }),
            Admit::Decline(Decline::SessionUp)
        );
        assert_eq!(
            admit(&Incoming {
                accept_pending: true,
                ..ok
            }),
            Admit::Decline(Decline::AcceptPending)
        );
        assert_eq!(
            admit(&Incoming { peer: None, ..ok }),
            Admit::Decline(Decline::NoPeer)
        );
        assert_eq!(admit(&incoming(None, A)), Admit::Decline(Decline::NoPolicy));
        assert_eq!(
            admit(&incoming(Some(&p), B)),
            Admit::Decline(Decline::NotPermitted)
        );
        let cool = Incoming {
            fail_streak: 3,
            cooldown_until_ms: 2001,
            ..ok
        };
        assert_eq!(
            admit(&cool),
            Admit::Decline(Decline::Cooldown { quiet_s: 2 })
        );
        assert_eq!(
            admit(&Incoming {
                now_ms: 2001,
                ..cool
            }),
            Admit::Accept {
                after_cooldown: true
            }
        );
        assert_eq!(
            admit(&Incoming {
                fail_streak: 2,
                ..cool
            }),
            Admit::Accept {
                after_cooldown: false
            }
        );
        // Busy wins over permission, as before the reasons were logged.
        assert_eq!(
            admit(&Incoming {
                channel_up: true,
                ..incoming(None, B)
            }),
            Admit::Decline(Decline::SessionUp)
        );
    }

    #[test]
    fn recovery_restarts_only_an_open_attempt_for_the_permitted_phone() {
        let a = address(A).unwrap();
        let b = address(B).unwrap();
        let mut t = Attempt::new();
        assert!(matches!(t.after_recovery(Some(a), false), Resume::Skip(_)));
        t.start(a);
        assert_eq!(t.after_recovery(Some(a), false), Resume::Nudge(a));
        // One decision per recovery; the attempt stays open until it ends.
        assert_eq!(t.after_recovery(Some(a), false), Resume::Nudge(a));
        assert!(matches!(t.after_recovery(Some(a), true), Resume::Skip(_)));
        assert!(matches!(t.after_recovery(Some(b), false), Resume::Skip(_)));
        assert!(matches!(t.after_recovery(None, false), Resume::Skip(_)));
        t.end();
        assert_eq!(t.peer(), None);
        assert!(matches!(t.after_recovery(Some(a), false), Resume::Skip(_)));
    }
}
