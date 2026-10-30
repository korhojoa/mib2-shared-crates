// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Wi-Fi credentials a wireless-Android-Auto handshake hands to the phone
//! (WifiStartRequest ip/port, WifiInfoResponse ssid/key/bssid).
//!
//! Callers can read credentials in-process or publish them to `CREDS_PPS`.
//! A Bluetooth process can read this QNX PPS object without starting a child
//! process. One PPS write updates all attributes atomically.
//!
//! Sources, in order: environment overrides (`AAWG_IP`, `AAWG_PORT`,
//! `AAWG_SSID`, `AAWG_KEY`, `AAWG_KEY_FILE`, `AAWG_BSSID`), then the live AP
//! (`uaputl -i uap0 ...` and `ifconfig uap0`), only for fields still empty.

use std::process::{Command, Stdio};

/// The PPS directory and object the native path reads (the unit mounts PPS
/// at /ramdisk/pps). Opened with `?nopersist`: the values are live AP state.
pub const CREDS_PPS_DIR: &str = "/ramdisk/pps/aawg";
pub const CREDS_PPS: &str = "/ramdisk/pps/aawg/creds";

/// Default AA TCP port handed to the phone.
pub const DEFAULT_PORT: u32 = 5288;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Creds {
    pub ip: String,
    pub port: u32,
    pub ssid: String,
    pub key: String,
    pub bssid: String,
}

impl Creds {
    /// The overrides from the environment; empty fields stay empty.
    pub fn from_env() -> Self {
        let mut c = Creds {
            ip: std::env::var("AAWG_IP").unwrap_or_default(),
            port: std::env::var("AAWG_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_PORT),
            ssid: std::env::var("AAWG_SSID").unwrap_or_default(),
            key: std::env::var("AAWG_KEY").unwrap_or_default(),
            bssid: std::env::var("AAWG_BSSID").unwrap_or_default(),
        };
        // Shell-safe secret path: AAWG_KEY_FILE holds the raw passphrase (a key
        // with $ ` " space \ is corrupted if the launcher sources AAWG_KEY=...).
        if c.key.is_empty() {
            if let Ok(kf) = std::env::var("AAWG_KEY_FILE") {
                if let Ok(raw) = std::fs::read_to_string(kf) {
                    c.key = raw.trim_end_matches(['\n', '\r']).to_string();
                }
            }
        }
        c
    }

    /// All five connection fields are present and the TCP port is nonzero.
    pub fn complete(&self) -> bool {
        self.connection_fields().iter().all(|(_, present)| *present)
    }

    /// Missing field names in connection order, without credential values.
    pub fn missing_fields(&self) -> Vec<&'static str> {
        self.connection_fields()
            .into_iter()
            .filter_map(|(name, present)| (!present).then_some(name))
            .collect()
    }

    fn connection_fields(&self) -> [(&'static str, bool); 5] {
        [
            ("ip", valid_field(&self.ip)),
            ("port", self.port != 0),
            ("ssid", valid_field(&self.ssid)),
            ("key", valid_field(&self.key)),
            ("bssid", valid_field(&self.bssid)),
        ]
    }

    /// The key's form, for logs (never log the key itself): a 64-char all-hex
    /// value is a raw PSK, anything shorter a passphrase; the phone hashes them
    /// differently.
    pub fn key_form(&self) -> &'static str {
        let klen = self.key.len();
        let khex = klen > 0 && self.key.bytes().all(|b| b.is_ascii_hexdigit());
        if klen == 64 && khex {
            "raw-psk(64hex)"
        } else if khex {
            "hex"
        } else {
            "passphrase"
        }
    }

    /// Fill every still-empty field from the live AP. Returns log notes (the
    /// bssid source), for the caller to print with its own prefix.
    pub fn fill_from_uap(&mut self) -> Vec<String> {
        let uaputl = std::env::var("AAWG_UAPUTL").unwrap_or_else(|_| "uaputl".into());
        let iface = std::env::var("AAWG_UAP_IF").unwrap_or_else(|_| "uap0".into());
        let mut notes = Vec::new();
        if self.ssid.is_empty() {
            if let Some(v) = scrape_cmd(&uaputl, &["-i", &iface, "sys_cfg_ssid"], "SSID = ") {
                self.ssid = v;
            }
        }
        if self.key.is_empty() {
            if let Some(v) = scrape_cmd(
                &uaputl,
                &["-i", &iface, "sys_cfg_wpa_passphrase"],
                "WPA passphrase = ",
            ) {
                self.key = v;
            }
        }
        if self.bssid.is_empty() {
            // The phone pins the AP by BSSID; it MUST equal the AP's real
            // broadcast MAC. The ifconfig interface MAC is not guaranteed to be
            // the broadcast BSSID on the Marvell uAP, so prefer uaputl's own AP
            // MAC (bss_config), fall back to ifconfig, and say which was used.
            let ap_mac = scrape_mac(&uaputl, &["-i", &iface, "bss_config"], "ap mac");
            let if_mac = scrape_cmd("ifconfig", &[&iface], "address: ");
            if let Some(m) = ap_mac {
                notes.push(format!(
                    "bssid={} (uaputl bss_config AP MAC; ifconfig address={})",
                    m,
                    if_mac.as_deref().unwrap_or("<none>")
                ));
                self.bssid = m;
            } else if let Some(m) = if_mac {
                notes.push(format!(
                    "bssid={} (ifconfig address; uaputl bss_config gave no AP MAC)",
                    m
                ));
                self.bssid = m;
            }
        }
        if self.ip.is_empty() {
            // uap0's own IPv4 is the endpoint handed to the phone; "inet " with
            // the trailing space does not match "inet6".
            if let Some(v) = scrape_cmd("ifconfig", &[&iface], "inet ") {
                if let Some(ip) = v.split_whitespace().next() {
                    self.ip = ip.to_string();
                }
            }
        }
        notes
    }

    /// The PPS update: one `attr:encoding:value` line per field. Written with a
    /// single write(), so a reader sees all of it or none of it.
    /// A line break or NUL in a field produces an empty, incomplete record.
    pub fn to_pps(&self) -> String {
        if [&self.ip, &self.ssid, &self.key, &self.bssid]
            .iter()
            .any(|field| field.contains(['\n', '\r', '\0']))
        {
            return Creds::default().to_pps();
        }
        format!(
            "ip::{}\nport:n:{}\nssid::{}\nkey::{}\nbssid::{}\ncomplete:b:{}\n",
            self.ip,
            self.port,
            self.ssid,
            self.key,
            self.bssid,
            self.complete()
        )
    }

    /// Parse a PPS object as read() returns it (`@name` line, then
    /// `attr:encoding:value` lines). None without the `@` header line, i.e.
    /// for anything that is not a PPS object read.
    pub fn parse_pps(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if !btsvc_core::pps::content(lines.next()?)?.starts_with('@') {
            return None;
        }
        let mut c = Creds::default();
        for line in lines {
            let line = btsvc_core::pps::content(line)?;
            let Some((name, rest)) = line.split_once(':') else {
                continue;
            };
            let Some((_enc, v)) = rest.split_once(':') else {
                continue;
            };
            match name {
                "ip" => c.ip = v.to_string(),
                "port" => c.port = v.parse().unwrap_or_default(),
                "ssid" => c.ssid = v.to_string(),
                "key" => c.key = v.to_string(),
                "bssid" => c.bssid = v.to_string(),
                _ => {}
            }
        }
        Some(c)
    }
}

fn valid_field(value: &str) -> bool {
    !value.is_empty() && !value.contains(['\n', '\r', '\0'])
}

// std::process::Command uses posix_spawn on QNX.
// Do not fork from a multithreaded process.
fn scrape_cmd(program: &str, args: &[&str], marker: &str) -> Option<String> {
    let out = Command::new(program)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    first_marked(&String::from_utf8_lossy(&out.stdout), marker)
}

fn first_marked(text: &str, marker: &str) -> Option<String> {
    for line in text.lines() {
        if let Some(pos) = line.find(marker) {
            let v = line[pos + marker.len()..].trim();
            if !v.is_empty() && v != "None" {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// True if `w` is exactly `xx:xx:xx:xx:xx:xx` (17 ASCII bytes, hex + colons).
fn is_mac_bytes(w: &[u8]) -> bool {
    if w.len() != 17 {
        return false;
    }
    for (i, &c) in w.iter().enumerate() {
        if i % 3 == 2 {
            if c != b':' {
                return false;
            }
        } else if !c.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

/// First real MAC (not the all-zero placeholder) in a line.
fn first_mac(line: &str) -> Option<String> {
    let b = line.as_bytes();
    if b.len() < 17 {
        return None;
    }
    for i in 0..=b.len() - 17 {
        let w = &b[i..i + 17];
        if is_mac_bytes(w) && w != b"00:00:00:00:00:00" {
            return Some(String::from_utf8_lossy(w).into_owned());
        }
    }
    None
}

/// First real MAC in a command's output. Lines containing `label` (matched
/// case-insensitively) are searched first, then any line. Robust to the exact
/// uaputl label wording, which is not pinned down.
fn scrape_mac(program: &str, args: &[&str], label: &str) -> Option<String> {
    let out = Command::new(program)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    mac_in_text(&String::from_utf8_lossy(&out.stdout), label)
}

fn mac_in_text(text: &str, label: &str) -> Option<String> {
    for pass in 0..2 {
        for line in text.lines() {
            if pass == 0 && !line.to_ascii_lowercase().contains(label) {
                continue;
            }
            if let Some(m) = first_mac(line) {
                return Some(m);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete_creds() -> Creds {
        Creds {
            ip: "192.0.2.1".into(),
            port: 5288,
            ssid: "test-ap".into(),
            key: "test-password".into(),
            bssid: "02:00:00:00:00:01".into(),
        }
    }

    #[test]
    fn every_connection_field_is_required() {
        let complete = complete_creds();
        assert!(complete.complete());
        assert!(complete.missing_fields().is_empty());
        for field in ["ip", "port", "ssid", "key", "bssid"] {
            let mut creds = complete.clone();
            match field {
                "ip" => creds.ip.clear(),
                "port" => creds.port = 0,
                "ssid" => creds.ssid.clear(),
                "key" => creds.key.clear(),
                "bssid" => creds.bssid.clear(),
                _ => unreachable!(),
            }
            assert!(!creds.complete(), "{field}");
            assert_eq!(creds.missing_fields(), [field]);
            assert!(creds.to_pps().contains("complete:b:false\n"));
        }
        assert_eq!(
            Creds::default().missing_fields(),
            ["ip", "port", "ssid", "key", "bssid"]
        );
    }

    #[test]
    fn pps_requires_each_field_without_trusting_the_complete_flag() {
        let text = format!("@creds\n{}", complete_creds().to_pps());
        for field in ["ip", "port", "ssid", "key", "bssid"] {
            let partial = text
                .lines()
                .filter(|line| !line.starts_with(&format!("{field}:")))
                .collect::<Vec<_>>()
                .join("\n");
            let creds = Creds::parse_pps(&partial).unwrap();
            assert!(!creds.complete(), "{field}");
            assert_eq!(creds.missing_fields(), [field]);
        }
        for port in ["0", "invalid"] {
            let partial = text.replace("port:n:5288", &format!("port:n:{port}"));
            assert_eq!(
                Creds::parse_pps(&partial).unwrap().missing_fields(),
                ["port"]
            );
        }
    }

    #[test]
    fn qualified_object_and_attributes_keep_literal_credentials() {
        let text = "[n]@creds\nbssid::02:00:00:00:00:01\ncomplete:b:true\nip::198.51.100.1\n[n]key::fake-passphrase\nport:n:5288\nssid::test-ap\n";
        let creds = Creds::parse_pps(text).unwrap();
        assert!(creds.complete());
        assert_eq!(creds.key, "fake-passphrase");
        assert_eq!(creds.port, 5288);
        assert!(Creds::parse_pps("[n@creds\nkey::invalid\n").is_none());
    }
    #[test]
    fn pps_round_trips_and_needs_header() {
        let c = Creds {
            ip: "192.0.2.1".into(),
            port: 5288,
            ssid: "MIB".into(),
            key: "pa:ss word".into(),
            bssid: "02:00:00:00:00:01".into(),
        };
        // read() of a PPS object prepends the "@name" line to the attributes.
        let read_back = format!("@creds\n{}", c.to_pps());
        assert_eq!(Creds::parse_pps(&read_back), Some(c));
        assert_eq!(Creds::parse_pps("ssid::x\n"), None);
    }

    #[test]
    fn mac_prefers_labelled_line_and_skips_zero() {
        let text =
            "x 00:00:00:00:00:00\nfoo 11:22:33:44:55:66\nAP MAC address = aa:bb:cc:dd:ee:ff\n";
        assert_eq!(
            mac_in_text(text, "ap mac").as_deref(),
            Some("aa:bb:cc:dd:ee:ff")
        );
        assert_eq!(
            mac_in_text("foo 11:22:33:44:55:66", "ap mac").as_deref(),
            Some("11:22:33:44:55:66")
        );
    }

    #[test]
    fn marker_skips_none() {
        assert_eq!(
            first_marked("SSID = None\nSSID = car\n", "SSID = ").as_deref(),
            Some("car")
        );
    }

    #[test]
    fn key_form() {
        let mut c = Creds {
            key: "abcd".into(),
            ..Default::default()
        };
        assert_eq!(c.key_form(), "hex");
        c.key = "a".repeat(64);
        assert_eq!(c.key_form(), "raw-psk(64hex)");
        c.key = "hello world".into();
        assert_eq!(c.key_form(), "passphrase");
    }
    #[test]
    fn pps_credentials_cannot_add_attributes_with_line_breaks() {
        for newline in ["\n", "\r", "\0"] {
            let mut creds = complete_creds();
            creds.ssid = format!("name{newline}complete:b:true");
            let encoded = creds.to_pps();
            let parsed = Creds::parse_pps(&format!("@creds\n{encoded}")).unwrap();
            assert!(!parsed.complete());
            assert_eq!(
                encoded
                    .lines()
                    .filter(|line| line.starts_with("complete:"))
                    .count(),
                1
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn uap_command_preserves_literal_program_and_interface_arguments() {
        use std::os::unix::fs::PermissionsExt;
        struct Fixture {
            dir: std::path::PathBuf,
            env: Vec<(&'static str, Option<std::ffi::OsString>)>,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                for (key, value) in &self.env {
                    if let Some(value) = value {
                        std::env::set_var(key, value);
                    } else {
                        std::env::remove_var(key);
                    }
                }
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }
        let dir = std::env::temp_dir().join(format!(
            "aawg-args-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let fixture = Fixture {
            dir,
            env: ["AAWG_UAPUTL", "AAWG_UAP_IF"]
                .into_iter()
                .map(|key| (key, std::env::var_os(key)))
                .collect(),
        };
        let program = fixture.dir.join("uap tool");
        std::fs::write(&program, b"#!/bin/sh\n[ \"$1\" = -i ] || exit 3\n[ \"$3\" = sys_cfg_ssid ] || exit 4\necho \"SSID = $2\"\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::env::set_var("AAWG_UAPUTL", &program);
        std::env::set_var("AAWG_UAP_IF", "uap0; exit 7");
        let mut creds = complete_creds();
        creds.ssid.clear();
        creds.fill_from_uap();
        assert_eq!(creds.ssid, "uap0; exit 7");
    }
}
