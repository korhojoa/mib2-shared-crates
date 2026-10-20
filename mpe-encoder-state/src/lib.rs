// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) mib2-shared-crates contributors
//! Shared implementation of the existing native MPE encoder event observer.
//! One owned reader consumes native log events without an independent service.
//! Event fields identify encoder state and stream markers.
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

pub const MAJOR: u32 = 43690;
const MAX_TEXT: usize = 1020;
const MAX_AGE_MS: u64 = 500;
static PROCESS_START: OnceLock<u64> = OnceLock::new();
static INSTANCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, PartialEq, Eq)]
pub enum Seen<'a> {
    Mark(&'a [u8]),
    State(bool),
    Nothing,
}
/// Stop classification at the first NUL byte.
pub fn classify<'a>(text: &'a [u8], prefix: &[u8]) -> Seen<'a> {
    let text = text.split(|&b| b == 0).next().unwrap_or(&[]);
    let find = |p: &[u8]| text.windows(p.len()).position(|w| w == p);
    if !prefix.is_empty() {
        if let Some(at) = find(prefix) {
            let rest = &text[at + prefix.len()..];
            let end = rest
                .iter()
                .position(|&b| b == b' ' || b == b'\n')
                .unwrap_or(rest.len());
            return Seen::Mark(&rest[..end]);
        }
    }
    if find(b"Enabled clock to Device: MPE").is_some() {
        return Seen::State(true);
    }
    if find(b"NvVideoStreamEncoderClose: Success").is_some() {
        return Seen::State(false);
    }
    Seen::Nothing
}

#[derive(Default)]
pub struct Decoder {
    pub level: Option<bool>,
    pub mark: String,
}
impl Decoder {
    /// Decode complete native-endian events. A broken boundary invalidates all
    /// cached evidence; it is never silently skipped while retaining closed.
    pub fn feed(&mut self, bytes: &[u8], prefix: &[u8]) -> io::Result<()> {
        if !bytes.len().is_multiple_of(4) {
            return self.gap();
        }
        let mut at = 0;
        while at < bytes.len() {
            if bytes.len() - at < 12 {
                return self.gap();
            }
            let word = |n: usize| u32::from_ne_bytes(bytes[n..n + 4].try_into().unwrap());
            let head = word(at);
            let count = ((head >> 16) & 0xff) as usize;
            let size = 12 + count * 4;
            if bytes.len() - at < size {
                return self.gap();
            }
            if word(at + 4) & 0xfffff == MAJOR && head & 8 != 0 {
                let text = &bytes[at + 12..at + size];
                debug_assert!(text.len() <= MAX_TEXT);
                match classify(text, prefix) {
                    Seen::State(level) => self.level = Some(level),
                    Seen::Mark(mark) => {
                        let Ok(mark) = std::str::from_utf8(mark) else {
                            return self.gap();
                        };
                        self.mark = mark.to_owned();
                    }
                    Seen::Nothing => {}
                }
            }
            at += size;
        }
        Ok(())
    }
    fn gap(&mut self) -> io::Result<()> {
        self.level = None;
        self.mark.clear();
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "incomplete native event",
        ))
    }
}

pub struct Config {
    pub object: PathBuf,
    /// Includes the separating space, for example "encmark ".
    pub prefix: String,
    /// Absolute monotonic milliseconds, shared with the native timer origin.
    pub clock: fn() -> u64,
    pub emit_marker: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// Consumer logger must timestamp the supplied line.
    pub log: fn(&str),
}
struct State {
    decoder: Decoder,
    generation: u64,
    expected: String,
    healthy: bool,
    caught_up: bool,
    heartbeat: u64,
}
struct Inner {
    config: Config,
    pid: u32,
    nonce: String,
    stop: AtomicBool,
    state: Mutex<State>,
}
struct Service {
    inner: Arc<Inner>,
    worker: Mutex<Option<JoinHandle<()>>>,
}
impl Drop for Service {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::Release);
        if let Ok(mut worker) = self.worker.lock() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
}
#[derive(Clone)]
pub struct Observer(Arc<Service>);
impl Observer {
    pub fn start(config: Config) -> io::Result<Self> {
        #[cfg(target_os = "nto")]
        const NONBLOCK: i32 = 0o200;
        #[cfg(not(target_os = "nto"))]
        const NONBLOCK: i32 = 0o4000;
        let reader = OpenOptions::new()
            .read(true)
            .custom_flags(NONBLOCK)
            .open("/dev/slog")?;
        Self::start_reader(config, Box::new(reader))
    }
    fn start_reader(config: Config, reader: Box<dyn Read + Send>) -> io::Result<Self> {
        if config.prefix.is_empty()
            || !config.prefix.is_ascii()
            || config.prefix.contains('\n')
            || !config.object.is_absolute()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid observer configuration",
            ));
        }
        if let Some(parent) = config.object.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let start = *PROCESS_START.get_or_init(config.clock);
        let nonce = format!("{:x}.{:x}", start, INSTANCE.fetch_add(1, Ordering::Relaxed));
        let inner = Arc::new(Inner {
            config,
            pid: std::process::id(),
            nonce,
            stop: AtomicBool::new(false),
            state: Mutex::new(State {
                decoder: Decoder::default(),
                generation: 0,
                expected: String::new(),
                healthy: false,
                caught_up: false,
                heartbeat: 0,
            }),
        });
        let worker_inner = inner.clone();
        let worker = std::thread::Builder::new()
            .name("mpe-observer".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    worker_loop(&worker_inner, reader)
                }));
                if outcome.is_err() {
                    invalidate(&worker_inner, "encoder observer panicked; evidence invalid");
                }
            })?;
        Ok(Self(Arc::new(Service {
            inner,
            worker: Mutex::new(Some(worker)),
        })))
    }
    pub fn ready(&self) -> bool {
        let inner = &self.0.inner;
        let Ok(state) = inner.state.lock() else {
            return false;
        };
        state.healthy
            && state.caught_up
            && (inner.config.clock)()
                .checked_sub(state.heartbeat)
                .is_some_and(|age| age <= MAX_AGE_MS)
    }
    /// Starts an owned generation without inventing a new close edge. Known
    /// continuously observed state survives a marker; unhealthy state does not.
    pub fn begin(&self) -> bool {
        if !self.ready() {
            return false;
        }
        let inner = &self.0.inner;
        let mark = {
            let Ok(mut state) = inner.state.lock() else {
                return false;
            };
            state.generation = state.generation.wrapping_add(1);
            state.expected = format!("{}.{}.{}", inner.pid, inner.nonce, state.generation);
            state.expected.clone()
        };
        if !(inner.config.emit_marker)(&format!("{}{}", inner.config.prefix, mark)) {
            invalidate(inner, "encoder marker publication failed; evidence invalid");
            return false;
        }
        true
    }
    /// Prevent a captured previous take from reusing its already observed mark.
    pub fn invalidate_generation(&self) {
        if let Ok(mut state) = self.0.inner.state.lock() {
            state.generation = state.generation.wrapping_add(1);
            state.expected.clear();
        }
    }
    pub fn stop(&self) {
        self.0.inner.stop.store(true, Ordering::Release);
        if let Ok(mut worker) = self.0.worker.lock() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
    pub fn level(&self) -> Option<bool> {
        if !self.ready() {
            return None;
        }
        let inner = &self.0.inner;
        let state = inner.state.lock().ok()?;
        if state.expected.is_empty() {
            return None;
        }
        let text = read_object(&inner.config.object).ok()?;
        validate_record(
            &text,
            inner.pid,
            &inner.nonce,
            state.generation,
            &state.expected,
            (inner.config.clock)(),
        )
    }
}
fn read_object(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut bytes = [0; 1024];
    let n = file.read(&mut bytes)?;
    if n == bytes.len() || n == 0 || bytes[n - 1] != b'\n' {
        return Err(io::ErrorKind::InvalidData.into());
    }
    String::from_utf8(bytes[..n].to_vec()).map_err(|_| io::ErrorKind::InvalidData.into())
}
fn validate_record(
    text: &str,
    pid: u32,
    nonce: &str,
    generation: u64,
    mark: &str,
    now: u64,
) -> Option<bool> {
    if !Path::new(&format!("/proc/{pid}")).exists() {
        return None;
    }
    let mut attrs = BTreeMap::new();
    for line in text.lines() {
        let line = if line.starts_with('[') {
            line.split_once(']')?.1
        } else {
            line
        };
        if line.starts_with('@') {
            continue;
        }
        let (key, rest) = line.split_once(':')?;
        let (_, value) = rest.split_once(':')?;
        if attrs.insert(key, value).is_some() {
            return None;
        }
    }
    if attrs.get("pid")?.parse::<u32>().ok()? != pid
        || *attrs.get("start")? != nonce
        || attrs.get("generation")?.parse::<u64>().ok()? != generation
        || *attrs.get("mark")? != mark
        || *attrs.get("healthy")? != "1"
    {
        return None;
    }
    let written = attrs.get("mono_ms")?.parse::<u64>().ok()?;
    if now.checked_sub(written)? > MAX_AGE_MS {
        return None;
    }
    match *attrs.get("state")? {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}
fn record_text(inner: &Inner, state: &State) -> String {
    let level = match state.decoder.level {
        Some(false) => "0",
        Some(true) => "1",
        None => "?",
    };
    format!(
        "pid::{}\nstart::{}\ngeneration::{}\nmark::{}\nstate::{}\nhealthy::{}\nmono_ms::{}\n",
        inner.pid,
        inner.nonce,
        state.generation,
        state.decoder.mark,
        level,
        u8::from(state.healthy),
        state.heartbeat
    )
}
fn publish(inner: &Inner, state: &State) -> io::Result<()> {
    let text = record_text(inner, state);
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&inner.config.object)?;
    if file.write(text.as_bytes())? != text.len() {
        return Err(io::ErrorKind::WriteZero.into());
    }
    Ok(())
}
fn invalidate(inner: &Inner, reason: &str) {
    if let Ok(mut state) = inner.state.lock() {
        state.healthy = false;
        state.decoder.level = None;
        state.decoder.mark.clear();
        let _ = publish(inner, &state);
    }
    (inner.config.log)(reason);
}
fn worker_loop(inner: &Inner, mut reader: Box<dyn Read + Send>) {
    let mut bytes = [0; 16384];
    while !inner.stop.load(Ordering::Acquire) {
        let mut caught_up = false;
        for _ in 0..64 {
            match reader.read(&mut bytes) {
                Ok(0) => {
                    invalidate(inner, "encoder source reached EOF; evidence invalid");
                    return;
                }
                Ok(n) => {
                    let Ok(mut state) = inner.state.lock() else {
                        return;
                    };
                    if state
                        .decoder
                        .feed(&bytes[..n], inner.config.prefix.as_bytes())
                        .is_err()
                    {
                        drop(state);
                        invalidate(
                            inner,
                            "encoder source lost event boundary; evidence invalid",
                        );
                        return;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    caught_up = true;
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    invalidate(inner, "encoder source read failed; evidence invalid");
                    return;
                }
            }
            if inner.stop.load(Ordering::Acquire) {
                break;
            }
        }
        let Ok(mut state) = inner.state.lock() else {
            return;
        };
        state.caught_up = caught_up;
        state.heartbeat = (inner.config.clock)();
        state.healthy = state.caught_up;
        if publish(inner, &state).is_err() {
            drop(state);
            invalidate(inner, "encoder PPS publication failed; evidence invalid");
            return;
        }
        drop(state);
        std::thread::sleep(Duration::from_millis(20));
    }
    invalidate(inner, "encoder observer stopped; evidence invalid");
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(text: &[u8], major: u32) -> Vec<u8> {
        let count = text.len().div_ceil(4);
        let mut bytes = Vec::new();
        for word in [(count as u32) << 16 | 8, major, 0] {
            bytes.extend_from_slice(&word.to_ne_bytes());
        }
        bytes.extend_from_slice(text);
        bytes.resize(12 + count * 4, 0);
        bytes
    }
    #[test]
    fn retained_original_classifier_vectors() {
        assert_eq!(
            classify(b"galmark 1234.5\0\0", b"galmark "),
            Seen::Mark(b"1234.5")
        );
        assert_eq!(
            classify(
                b"NvClock: NvQryClockDeviceClockControl: Enabled clock to Device: MPE\0",
                b"galmark "
            ),
            Seen::State(true)
        );
        assert_eq!(
            classify(b"NvVideoStreamEncoderClose: Success\0\0\0", b"galmark "),
            Seen::State(false)
        );
        assert_eq!(
            classify(
                b"NvVideoStreamEncoderClose: Waiting for EndSem",
                b"galmark "
            ),
            Seen::Nothing
        );
        assert_eq!(classify(b"x\0galmark 1.1", b"galmark "), Seen::Nothing);
    }
    #[test]
    fn known_warm_level_survives_marker_but_not_gap_or_busy() {
        let mut d = Decoder::default();
        d.feed(
            &event(b"NvVideoStreamEncoderClose: Success", MAJOR),
            b"encmark ",
        )
        .unwrap();
        d.feed(&event(b"encmark current", MAJOR), b"encmark ")
            .unwrap();
        assert_eq!(d.level, Some(false));
        assert_eq!(d.mark, "current");
        d.feed(&event(b"Enabled clock to Device: MPE", MAJOR), b"encmark ")
            .unwrap();
        assert_eq!(d.level, Some(true));
        assert!(d.feed(&[0; 3], b"encmark ").is_err());
        assert_eq!(d.level, None);
    }
    #[test]
    fn unrelated_major_and_marker_without_known_level_never_close() {
        let mut d = Decoder::default();
        d.feed(
            &event(b"NvVideoStreamEncoderClose: Success", 123),
            b"encmark ",
        )
        .unwrap();
        d.feed(&event(b"encmark only", MAJOR), b"encmark ").unwrap();
        assert_eq!(d.level, None);
        let mut partial = event(b"NvVideoStreamEncoderClose: Success", MAJOR);
        partial.pop();
        assert!(d.feed(&partial, b"encmark ").is_err());
    }
    fn record(pid: u32, nonce: &str, generation: u64, mark: &str, state: &str) -> String {
        format!("pid::{pid}\nstart::{nonce}\ngeneration::{generation}\nmark::{mark}\nstate::{state}\nhealthy::1\nmono_ms::1000\n")
    }
    #[test]
    fn records_require_live_pid_exact_start_generation_mark_and_age() {
        let pid = std::process::id();
        let text = record(pid, "nonce", 7, "mark", "0");
        assert_eq!(
            validate_record(&text, pid, "nonce", 7, "mark", 1000),
            Some(false)
        );
        assert_eq!(validate_record(&text, pid, "nonce", 8, "mark", 1000), None);
        assert_eq!(validate_record(&text, pid, "other", 7, "mark", 1000), None);
        assert_eq!(validate_record(&text, pid, "nonce", 7, "old", 1000), None);
        assert_eq!(validate_record(&text, pid, "nonce", 7, "mark", 1501), None);
        assert_eq!(validate_record(&text, pid, "nonce", 7, "mark", 999), None);
        assert_eq!(
            validate_record(
                &record(u32::MAX, "nonce", 7, "mark", "0"),
                u32::MAX,
                "nonce",
                7,
                "mark",
                1000
            ),
            None
        );
        assert_eq!(
            validate_record(
                &record(pid, "nonce", 7, "mark", "?"),
                pid,
                "nonce",
                7,
                "mark",
                1000
            ),
            None
        );
        assert_eq!(
            validate_record(&(text + "state::1\n"), pid, "nonce", 7, "mark", 1000),
            None
        );
    }
    #[test]
    fn flagged_pps_attributes_and_duplicate_aliases() {
        let pid = std::process::id();
        let text = record(pid, "nonce", 7, "mark", "0");
        let flagged = format!(
            "[n]@mpe_state\n{}",
            text.lines()
                .map(|line| format!("[n]{line}\n"))
                .collect::<String>()
        );
        assert_eq!(
            validate_record(&flagged, pid, "nonce", 7, "mark", 1000),
            Some(false)
        );
        assert_eq!(
            validate_record(&(flagged + "[n]state::1\n"), pid, "nonce", 7, "mark", 1000),
            None
        );
    }
    #[cfg(target_os = "linux")]
    mod worker_tests {
        use super::*;
        use std::collections::VecDeque;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::Instant;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        fn clock() -> u64 {
            1000
        }
        fn log(_: &str) {}
        #[test]
        fn pps_merge_replaces_shorter_mark_using_actual_publish_payload() {
            let fixture = Fixture::new();
            wait(|| fixture.observer.ready());
            let inner = &fixture.observer.0.inner;
            let mut state = inner.state.lock().unwrap();
            state.generation = 9;
            state.decoder.mark = "old-long-marker".into();
            state.decoder.level = Some(false);
            let mut attributes = BTreeMap::new();
            let mut apply = |text: String| {
                for line in text.lines() {
                    let (key, value) = line.split_once("::").unwrap();
                    attributes.insert(key.to_owned(), value.to_owned());
                }
                format!(
                    "@mpe_state\n{}",
                    attributes
                        .iter()
                        .map(|(key, value)| format!("[n]{key}::{value}\n"))
                        .collect::<String>()
                )
            };
            apply(record_text(inner, &state));
            state.decoder.mark = "new".into();
            let shrunk = apply(record_text(inner, &state));
            assert!(!shrunk.contains("old-long-marker"));
            assert_eq!(
                validate_record(&shrunk, inner.pid, &inner.nonce, 9, "new", 1000),
                Some(false)
            );
            state.decoder.mark.clear();
            let invalidated = apply(record_text(inner, &state));
            assert_eq!(
                validate_record(&invalidated, inner.pid, &inner.nonce, 9, "new", 1000),
                None
            );
        }
        enum Input {
            Bytes(Vec<u8>),
            Error,
        }
        struct Source(Arc<Mutex<VecDeque<Input>>>);
        impl Read for Source {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                match self.0.lock().unwrap().pop_front() {
                    Some(Input::Bytes(bytes)) => {
                        assert!(bytes.len() <= out.len());
                        out[..bytes.len()].copy_from_slice(&bytes);
                        Ok(bytes.len())
                    }
                    Some(Input::Error) => Err(io::ErrorKind::BrokenPipe.into()),
                    None => Err(io::ErrorKind::WouldBlock.into()),
                }
            }
        }
        struct Fixture {
            observer: Observer,
            inputs: Arc<Mutex<VecDeque<Input>>>,
            dir: PathBuf,
        }
        impl Fixture {
            fn new() -> Self {
                let dir = std::env::temp_dir().join(format!(
                    "mpe-observer-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::create_dir(&dir).unwrap();
                let inputs = Arc::new(Mutex::new(VecDeque::new()));
                let marker_inputs = inputs.clone();
                let observer = Observer::start_reader(
                    Config {
                        object: dir.join("state"),
                        prefix: "encmark ".into(),
                        clock,
                        emit_marker: Arc::new(move |text| {
                            marker_inputs
                                .lock()
                                .unwrap()
                                .push_back(Input::Bytes(event(text.as_bytes(), MAJOR)));
                            true
                        }),
                        log,
                    },
                    Box::new(Source(inputs.clone())),
                )
                .unwrap();
                Self {
                    observer,
                    inputs,
                    dir,
                }
            }
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                self.observer.stop();
                std::fs::remove_dir_all(&self.dir).unwrap();
            }
        }
        fn wait(mut condition: impl FnMut() -> bool) {
            let start = Instant::now();
            while !condition() {
                assert!(start.elapsed() < Duration::from_secs(2));
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        #[test]
        fn actual_worker_warm_start_busy_release_and_shutdown() {
            let f = Fixture::new();
            wait(|| f.observer.ready());
            assert!(f.observer.begin());
            wait(|| {
                f.observer
                    .0
                    .inner
                    .state
                    .lock()
                    .unwrap()
                    .decoder
                    .mark
                    .contains('.')
            });
            assert_eq!(f.observer.level(), None); // heartbeat/marker alone is unknown
            f.inputs.lock().unwrap().push_back(Input::Bytes(event(
                b"NvVideoStreamEncoderClose: Success",
                MAJOR,
            )));
            wait(|| f.observer.level() == Some(false));
            assert!(f.observer.begin());
            wait(|| f.observer.level() == Some(false)); // no new close edge
            f.inputs
                .lock()
                .unwrap()
                .push_back(Input::Bytes(event(b"Enabled clock to Device: MPE", MAJOR)));
            wait(|| f.observer.level() == Some(true));
            f.observer.invalidate_generation();
            assert_eq!(f.observer.level(), None);
            f.observer.stop();
            assert!(!f.observer.ready());
        }
        #[test]
        fn actual_read_failure_invalidates_known_closed_record() {
            let f = Fixture::new();
            wait(|| f.observer.ready());
            assert!(f.observer.begin());
            f.inputs.lock().unwrap().push_back(Input::Bytes(event(
                b"NvVideoStreamEncoderClose: Success",
                MAJOR,
            )));
            wait(|| f.observer.level() == Some(false));
            f.inputs.lock().unwrap().push_back(Input::Error);
            wait(|| !f.observer.ready());
            assert_eq!(f.observer.level(), None);
        }
        #[test]
        fn actual_publication_failure_invalidates_worker() {
            let f = Fixture::new();
            wait(|| f.observer.ready());
            let object = f.dir.join("state");
            std::fs::remove_file(&object).unwrap();
            std::fs::create_dir(&object).unwrap();
            wait(|| !f.observer.ready());
            assert_eq!(f.observer.level(), None);
        }
    }
}
