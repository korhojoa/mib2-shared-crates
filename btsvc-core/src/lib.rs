// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bluetooth report parsing and service state.
//!
//! This crate has no I/O. It converts HID, RFCOMM, SM and GATT events into
//! state changes and command requests. The caller supplies transport and logs.
//! Host tests check parser output and the state transitions.

#![cfg_attr(not(test), no_std)]

pub mod aawg;
pub mod descriptor;
pub mod ifcfg;
pub mod kbm;
pub mod launcher;
pub mod mux;
pub mod pairing;
pub mod pan;
pub mod report;
pub mod rumble;
pub mod sm;
pub mod socks;
pub mod tofu;

pub use report::{Battery, Finger, PadState, PadType, Sensors, NAXES, NBUTTONS};

pub mod pps;
pub mod wireless_launch;
pub mod wireless_policy;
