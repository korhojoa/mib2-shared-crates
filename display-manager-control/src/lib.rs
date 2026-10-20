// SPDX-License-Identifier: MIT OR Apache-2.0
//! In-process control of the MIB2 DisplayManager: declare a context, switch a
//! display to it. The same DSI events the HMI sends
//! (declareContext/switchContext), through libdmdt_core's CDMDTInterface.
//!
//! Initialization creates ErrorStorage, Osal and a DMRCClient AgentStarter,
//! then connects the display interface. Initialization runs once per process.
//!
//! Display zero is the head-unit panel. Display four is the instrument cluster.
//! Navigation uses cluster context 70. Callers must not redeclare that context.
//! Index zero in a declared displayable list selects the top layer.
//!
//! The return values of declareContext/switchContext are what the vendor
//! functions leave in r0; they are not a status.

use std::cell::UnsafeCell;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::OnceLock;

extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}
const RTLD_NOW: c_int = 0x0002;
const RTLD_GLOBAL: c_int = 0x0100;
#[cfg(target_os = "nto")]
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
#[cfg(not(target_os = "nto"))]
const RTLD_DEFAULT: *mut c_void = std::ptr::null_mut();

/// The cluster display.
pub const DISPLAY_CLUSTER: i32 = 4;
/// Nav's own context on the cluster display.
pub const CONTEXT_CLUSTER_NAV: i32 = 70;

/// Eight-byte-aligned storage for placement-constructed native objects.
struct Scratch<const N: usize>(UnsafeCell<[f64; N]>);
// SAFETY: written only inside the one-time init below.
unsafe impl<const N: usize> Sync for Scratch<N> {}
impl<const N: usize> Scratch<N> {
    const fn new() -> Self {
        Self(UnsafeCell::new([0.0; N]))
    }
    fn p(&self) -> *mut c_void {
        self.0.get().cast()
    }
}

static ERRSTORE: Scratch<300> = Scratch::new();
static OSAL: Scratch<8> = Scratch::new();
static AGENT: Scratch<8> = Scratch::new();
static STR1: Scratch<16> = Scratch::new();
static STR2: Scratch<16> = Scratch::new();
static ALLOC: Scratch<8> = Scratch::new();
static IFACE: Scratch<16> = Scratch::new();

type Handler = extern "C" fn(*mut c_void, *const c_char, *const c_void, u32);
type ErrCtor = extern "C" fn(*mut c_void, Handler);
type OsalCtor = extern "C" fn(*mut c_void, c_int, c_int);
type UtilInit = extern "C" fn(*const c_char, c_int, c_int, c_int, c_int);
type StrCtor = extern "C" fn(*mut c_void, *const c_char, *const c_void);
type AgentCtor = extern "C" fn(*mut c_void, *const c_void, *const c_void);
type AgentStart = extern "C" fn(*mut c_void, *mut c_void, c_int);
type Ctor = extern "C" fn(*mut c_void);
type Declare = extern "C" fn(*mut c_void, c_int, *mut c_int, c_int) -> c_int;
type Switch = extern "C" fn(*mut c_void, c_int, c_int) -> c_int;
#[derive(Clone, Copy)]
struct Dm {
    declare: Declare,
    switch: Switch,
}

static DM: OnceLock<Result<Dm, String>> = OnceLock::new();

const NAMES: [&CStr; 12] = [
    c"_ZN3ipl12ErrorStorage21action_dump_to_stdoutEPvPKcPKNS_11ErrorObjectEj",
    c"_ZN3ipl12ErrorStorageC1EPFvPvPKcPKNS_11ErrorObjectEjE",
    c"_ZN4osal4OsalC1Ebb",
    c"_ZN4util4Util4initEPKcbbbb",
    c"_ZN3ipl12basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEEC1EPKcRKS4_",
    c"_ZN4comm12AgentStarterC1ERKN3ipl12basic_stringIcNS1_11char_traitsIcEENS1_9allocatorIcEEEES9_",
    c"_ZN4comm12AgentStarter5startEPNS_25AgentStarterEventReceiverEb",
    c"_ZN14CDMDTInterfaceC1Ev",
    c"_ZN14CDMDTInterface7connectEv",
    c"_ZN14CDMDTInterface14declareContextEiPii",
    c"_ZN14CDMDTInterface13switchContextEii",
    c"_ZN14CDMDTInterface10disconnectEv",
];

/// # Safety
/// `a` must be a non-zero address of a function with signature `T`.
unsafe fn func<T: Copy>(a: usize) -> T {
    // SAFETY: forwarded to the caller.
    unsafe { std::mem::transmute_copy::<usize, T>(&a) }
}

fn bootstrap() -> Result<Dm, String> {
    // SAFETY: valid path; RTLD_GLOBAL so the symbols below resolve.
    let h = unsafe { dlopen(c"/eso/lib/libdmdt_core.so".as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
    if h.is_null() {
        return Err("libdmdt_core.so not loadable".into());
    }
    // Search the library and its dependencies before the global namespace.
    // A standalone process can have fewer global symbols than dio_manager.
    let a: Vec<usize> = NAMES
        .iter()
        .map(|n| {
            // SAFETY: valid handles and NUL-terminated names.
            let v = unsafe { dlsym(h, n.as_ptr()) } as usize;
            if v != 0 {
                v
            } else {
                // SAFETY: as above.
                unsafe { dlsym(RTLD_DEFAULT, n.as_ptr()) as usize }
            }
        })
        .collect();
    if let Some(i) = a.iter().position(|&v| v == 0) {
        return Err(format!("missing symbol {}", NAMES[i].to_string_lossy()));
    }
    // SAFETY: each exported symbol has the function signature declared above.
    unsafe {
        func::<ErrCtor>(a[1])(ERRSTORE.p(), func::<Handler>(a[0]));
        func::<OsalCtor>(a[2])(OSAL.p(), 1, 0);
        func::<UtilInit>(a[3])(c"DMRCClient".as_ptr(), 1, 1, 1, 1);
        func::<StrCtor>(a[4])(STR1.p(), c"DMRCClient".as_ptr(), ALLOC.p());
        func::<StrCtor>(a[4])(STR2.p(), c"local".as_ptr(), ALLOC.p());
        func::<AgentCtor>(a[5])(AGENT.p(), STR1.p(), STR2.p());
        func::<AgentStart>(a[6])(AGENT.p(), std::ptr::null_mut(), 1);
        func::<Ctor>(a[7])(IFACE.p());
        func::<Ctor>(a[8])(IFACE.p());
        Ok(Dm {
            declare: func(a[9]),
            switch: func(a[10]),
        })
    }
}

/// Bootstrap once. The error says what was missing; later calls return the
/// first result.
pub fn init() -> Result<(), String> {
    DM.get_or_init(bootstrap)
        .as_ref()
        .map(|_| ())
        .map_err(Clone::clone)
}

fn dm() -> Option<Dm> {
    DM.get().and_then(|r| r.as_ref().ok().copied())
}

/// declareContext(ctx, ids): index 0 is the top layer. -1 without init.
pub fn declare(ctx: i32, ids: &[i32]) -> i32 {
    let Some(d) = dm() else { return -1 };
    let mut v = ids.to_vec();
    (d.declare)(IFACE.p(), ctx, v.as_mut_ptr(), v.len() as c_int)
}

/// switchContext(display, ctx). -1 without init.
pub fn switch(display: i32, ctx: i32) -> i32 {
    let Some(d) = dm() else { return -1 };
    (d.switch)(IFACE.p(), display, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Debug, PartialEq, Eq)]
    enum Call {
        Declare(usize, i32, Vec<i32>),
        Switch(usize, i32, i32),
    }

    static CALLS: Mutex<Vec<Call>> = Mutex::new(Vec::new());

    extern "C" fn fake_declare(
        iface: *mut c_void,
        ctx: c_int,
        ids: *mut c_int,
        count: c_int,
    ) -> c_int {
        // SAFETY: declare passes its live Vec, including an aligned pointer
        // for the zero-length case. These callbacks are private to this test.
        let ids = unsafe { std::slice::from_raw_parts_mut(ids, count as usize) };
        CALLS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Call::Declare(iface as usize, ctx, ids.to_vec()));
        if let Some(first) = ids.first_mut() {
            *first = -123;
        }
        -37
    }

    extern "C" fn fake_switch(iface: *mut c_void, display: c_int, ctx: c_int) -> c_int {
        CALLS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Call::Switch(iface as usize, display, ctx));
        23
    }

    #[test]
    fn initialization_guard_and_native_argument_forwarding() {
        // One sequential test owns the fake global state. It never calls
        // init/bootstrap or loads any native library.
        assert_eq!(switch(DISPLAY_CLUSTER, CONTEXT_CLUSTER_NAV), -1);
        assert_eq!(declare(97, &[12]), -1);
        assert_eq!(*CALLS.lock().unwrap(), []);
        assert!(DM
            .set(Ok(Dm {
                declare: fake_declare,
                switch: fake_switch,
            }))
            .is_ok());

        let ids = [16, 33, 59];
        assert_eq!(declare(97, &ids), -37);
        assert_eq!(ids, [16, 33, 59]);
        assert_eq!(declare(-9, &[]), -37);
        assert_eq!(switch(DISPLAY_CLUSTER, CONTEXT_CLUSTER_NAV), 23);
        assert_eq!(switch(0, -9), 23);
        assert_eq!(
            *CALLS.lock().unwrap(),
            [
                Call::Declare(IFACE.p() as usize, 97, vec![16, 33, 59]),
                Call::Declare(IFACE.p() as usize, -9, vec![]),
                Call::Switch(IFACE.p() as usize, DISPLAY_CLUSTER, CONTEXT_CLUSTER_NAV),
                Call::Switch(IFACE.p() as usize, 0, -9),
            ]
        );
    }
}
