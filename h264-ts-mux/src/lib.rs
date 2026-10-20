// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) mib2-shared-crates contributors

//! H.264 and MPEG-TS format code for the MOST video channel.
//!
//! This crate reads H.264 parameter sets and makes skip slices and transport
//! stream packets. It uses fixed buffers supplied by the caller. It has no
//! heap allocation, clock, file access or logging.
//!
//! The caller supplies PCR and PTS values and receives complete blocks through
//! `ts::MuxIo`. The caller controls transmission, retries and error reporting.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod h264;
pub mod ts;
