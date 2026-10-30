// SPDX-License-Identifier: MIT OR Apache-2.0
//! Settings that can change without a new phone negotiation.

use crate::{config::Snapshot, schema};

/// These controls are read by the current writer and recovery loops.
pub const CLUSTER_KEYS: &[&str] = &[
    "cluster_cb_cooldown_secs",
    "cluster_cb_enable",
    "cluster_cb_hard_stall_ms",
    "cluster_cb_max_trips",
    "cluster_cb_max_werr",
    "cluster_cb_stall_count",
    "cluster_cb_stall_ms",
    "cluster_flush_max_hold_ms",
    "cluster_focus_kick",
    "cluster_focus_kick_grace_ms",
    "cluster_focus_kick_max",
    "cluster_focus_kick_min_ms",
    "cluster_focus_kick_settle_ms",
    "cluster_focus_kick_wq",
    "cluster_ui_pace_kbps",
    "cluster_write_pace_max_wait_ms",
    "cluster_write_pace_pct",
    "cluster_sink_feedback",
];

/// The module that must already be loaded. None means the loader owns the control.
pub fn module(key: &str) -> Option<&'static str> {
    if CLUSTER_KEYS.contains(&key) {
        Some("cluster")
    } else if key == "sensor_ev_capacity_wh"
        || key == "sensor_ev_send"
        || key.starts_with("sensor_send_")
    {
        Some("sensor")
    } else {
        None
    }
}

/// The discovery offer is fixed for a session. A previously offered sensor can
/// stop and resume reports; an unoffered sensor needs a new session.
pub fn eligible(startup: &Snapshot, key: &str) -> bool {
    if CLUSTER_KEYS.contains(&key)
        || matches!(
            key,
            "aa_suppress_navigation_active" | "sensor_ev_capacity_wh" | "sensor_send_parking_brake"
        )
    {
        return true;
    }
    if key == "sensor_ev_send" {
        return startup.get("sensor_ev_advertise", 0) != 0;
    }
    key.starts_with("sensor_send_")
        && schema::find(key).is_some_and(|s| startup.get(key, s.default) != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn discovery_and_geometry_cannot_be_made_live() {
        let startup = Snapshot {
            base: "sensor\nsensor_send_compass=1\nsensor_ev_advertise=1\n".into(),
            values: BTreeMap::new(),
            revision: 0,
        };
        assert!(eligible(&startup, "sensor_send_compass"));
        assert!(eligible(&startup, "sensor_ev_send"));
        assert!(!eligible(&startup, "sensor_send_rpm"));
        for key in [
            "feature.sensor",
            "feature.cluster",
            "ev_routing",
            "sensor_ev_advertise",
            "main_dpi",
            "media_audio_codec",
            "cluster_density",
            "cluster_output",
            "cluster_codec_resolution",
        ] {
            assert!(!eligible(&startup, key), "{key}");
        }
    }
}
