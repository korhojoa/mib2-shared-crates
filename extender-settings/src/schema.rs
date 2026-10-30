// SPDX-License-Identifier: MIT OR Apache-2.0
// Settings descriptors and defaults shared by the reader and helper commands.
#[derive(Clone, Copy)]
pub struct Setting {
    pub key: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub boolean: bool,
    pub default: i32,
    pub min: i32,
    pub max: i32,
    pub step: i32,
    pub readonly: bool,
}

#[rustfmt::skip]
pub const FEATURES: &[&str] = &["cluster", "sensor", "wireless", "ui", "input", "launcher", "aauplink"];

#[rustfmt::skip]
pub const SETTINGS: &[Setting] = &[
    Setting { key: "ev_routing", group: "EV", label: "EV routing|0=Off|1=On", boolean: false, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "feature.sensor", group: "EV", label: "Vehicle sensor integration", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "identify_as_dhu", group: "Displays", label: "Developer head-unit identity", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "sensor_ev_advertise", group: "EV", label: "Advertise EV capability", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "sensor_ev_send", group: "EV", label: "Send EV energy model", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "sensor_ev_capacity_wh", group: "EV", label: "Usable battery capacity (Wh)", boolean: false, default: 30000, min: 1, max: 1000000, step: 100, readonly: false },
    Setting { key: "sensor_send_compass", group: "Sensors", label: "Compass", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "sensor_send_rpm", group: "Sensors", label: "Engine speed", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "sensor_send_parking_brake", group: "Sensors", label: "Parking brake (car data)", boolean: true, default: 1, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "sensor_send_environment", group: "Sensors", label: "Outside temperature", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "sensor_send_hvac", group: "Sensors", label: "Climate", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "sensor_send_dead_reckoning", group: "Sensors", label: "Wheel speeds and steering", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "sensor_send_passenger", group: "Sensors", label: "Passenger present", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "sensor_send_door", group: "Sensors", label: "Doors", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "sensor_send_light", group: "Sensors", label: "Lights", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "sensor_send_tire", group: "Sensors", label: "Tyre pressure", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "sensor_send_accel", group: "Sensors", label: "Accelerometer", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "sensor_send_gyro", group: "Sensors", label: "Gyroscope", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "feature.cluster", group: "Displays", label: "Cluster map", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "main_dpi", group: "Displays", label: "Main screen density (dpi)", boolean: false, default: 0, min: 0, max: 640, step: 1, readonly: false },
    Setting { key: "cluster_density", group: "Displays", label: "Cluster density (dpi)", boolean: false, default: 180, min: 60, max: 640, step: 1, readonly: false },
    Setting { key: "cluster_viewing_distance", group: "Displays", label: "Viewing distance (mm)", boolean: false, default: 100, min: 1, max: 10000, step: 10, readonly: false },
    Setting { key: "aa_suppress_navigation_active", group: "Navigation", label: "Suppress phone navigation activity", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "aauplink_flush_on_close", group: "Advanced", label: "Flush tunnel queues on close", boolean: true, default: 1, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "aauplink_flush_on_open", group: "Advanced", label: "Flush tunnel queues on open", boolean: true, default: 1, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "aauplink_router_preflight", group: "Advanced", label: "Check the tunnel router before start", boolean: true, default: 1, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "aauplink_shm_recheck_ms", group: "Advanced", label: "Tunnel buffer check interval (ms)", boolean: false, default: 5000, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_boot_settle_secs", group: "Advanced", label: "Boot settling interval (s)", boolean: false, default: 90, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_cb_cooldown_secs", group: "Advanced", label: "Circuit breaker cooldown (s)", boolean: false, default: 60, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_cb_enable", group: "Advanced", label: "Enable the circuit breaker", boolean: true, default: 1, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "cluster_cb_hard_stall_ms", group: "Advanced", label: "Hard write stall threshold (ms)", boolean: false, default: 1000, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_cb_max_trips", group: "Advanced", label: "Maximum circuit breaker trips", boolean: false, default: 3, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_cb_max_werr", group: "Advanced", label: "Maximum consecutive write errors", boolean: false, default: 6, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_cb_stall_count", group: "Advanced", label: "Stall count before a trip", boolean: false, default: 3, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_cb_stall_ms", group: "Advanced", label: "Write stall threshold (ms)", boolean: false, default: 400, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_codec_resolution", group: "Displays", label: "Cluster video size|1=800x480|2=1280x720", boolean: false, default: 1, min: 1, max: 2, step: 1, readonly: false },
    Setting { key: "cluster_content_inset_bottom", group: "Displays", label: "Content inset bottom (px)", boolean: false, default: 0, min: 0, max: 479, step: 1, readonly: false },
    Setting { key: "cluster_content_inset_left", group: "Displays", label: "Content inset left (px)", boolean: false, default: 64, min: 0, max: 799, step: 1, readonly: false },
    Setting { key: "cluster_content_inset_right", group: "Displays", label: "Content inset right (px)", boolean: false, default: 64, min: 0, max: 799, step: 1, readonly: false },
    Setting { key: "cluster_content_inset_top", group: "Displays", label: "Content inset top (px)", boolean: false, default: 0, min: 0, max: 479, step: 1, readonly: false },
    Setting { key: "cluster_dm_display", group: "Advanced", label: "Display manager display ID", boolean: false, default: 4, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_dm_hide_context", group: "Advanced", label: "Display manager context to hide", boolean: false, default: -9, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_dm_table", group: "Advanced", label: "Display manager table ID", boolean: false, default: 70, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_flush_max_hold_ms", group: "Advanced", label: "Maximum frame hold before flush (ms)", boolean: false, default: 100, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_focus_kick", group: "Advanced", label: "Retry cluster display focus", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "cluster_focus_kick_grace_ms", group: "Advanced", label: "Focus retry grace interval (ms)", boolean: false, default: 15000, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_focus_kick_max", group: "Advanced", label: "Maximum focus retries", boolean: false, default: 3, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_focus_kick_min_ms", group: "Advanced", label: "Minimum focus retry interval (ms)", boolean: false, default: 20000, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_focus_kick_settle_ms", group: "Advanced", label: "Focus retry settling interval (ms)", boolean: false, default: 1500, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_focus_kick_wq", group: "Advanced", label: "Minimum writes before focus retry", boolean: false, default: 16, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_gate_enable", group: "Advanced", label: "Enable the cluster admission gate", boolean: true, default: 1, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "cluster_handover_settle_ms", group: "Advanced", label: "Display handover settling interval (ms)", boolean: false, default: 500, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_height_margin", group: "Displays", label: "Total height margin (px)", boolean: false, default: 344, min: 0, max: 479, step: 1, readonly: false },
    Setting { key: "cluster_main_focus_extra_ms", group: "Advanced", label: "Additional main focus wait (ms)", boolean: false, default: 750, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_main_focus_wait_ms", group: "Advanced", label: "Main focus wait limit (ms)", boolean: false, default: 8000, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_margin_bottom", group: "Displays", label: "Bottom margin (px)", boolean: false, default: 96, min: -1, max: 479, step: 1, readonly: false },
    Setting { key: "cluster_margin_left", group: "Displays", label: "Left margin (px)", boolean: false, default: 48, min: -1, max: 799, step: 1, readonly: false },
    Setting { key: "cluster_margin_right", group: "Displays", label: "Right margin (px)", boolean: false, default: 48, min: -1, max: 799, step: 1, readonly: false },
    Setting { key: "cluster_margin_top", group: "Displays", label: "Top margin (px)", boolean: false, default: 60, min: -1, max: 479, step: 1, readonly: false },
    Setting { key: "cluster_max_unacked", group: "Advanced", label: "Maximum unacknowledged frames", boolean: false, default: 1, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_output", group: "Advanced", label: "Cluster video output selection", boolean: false, default: 0, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_quiet_driver", group: "Advanced", label: "Reduce driver log output", boolean: true, default: 1, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "cluster_ready_settle_ms", group: "Advanced", label: "Encoder ready settling interval (ms)", boolean: false, default: 5000, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_sink_feedback", group: "Advanced", label: "Use cluster receiver feedback", boolean: true, default: 1, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "cluster_sps_crop", group: "Displays", label: "Crop cluster video to the margins", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "cluster_stable_inset_bottom", group: "Displays", label: "Stable inset bottom (px)", boolean: false, default: 0, min: 0, max: 479, step: 1, readonly: false },
    Setting { key: "cluster_stable_inset_left", group: "Displays", label: "Stable inset left (px)", boolean: false, default: 0, min: 0, max: 799, step: 1, readonly: false },
    Setting { key: "cluster_stable_inset_right", group: "Displays", label: "Stable inset right (px)", boolean: false, default: 0, min: 0, max: 799, step: 1, readonly: false },
    Setting { key: "cluster_stable_inset_top", group: "Displays", label: "Stable inset top (px)", boolean: false, default: 0, min: 0, max: 479, step: 1, readonly: false },
    Setting { key: "cluster_ui_pace_kbps", group: "Advanced", label: "Menu video rate (kbit/s)", boolean: false, default: 9600, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_width_margin", group: "Displays", label: "Total width margin (px)", boolean: false, default: 270, min: 0, max: 799, step: 1, readonly: false },
    Setting { key: "cluster_write_pace_max_wait_ms", group: "Advanced", label: "Maximum write pacing wait (ms)", boolean: false, default: 1000, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "cluster_write_pace_pct", group: "Advanced", label: "Write pacing rate (%)", boolean: false, default: 150, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "feature.aauplink", group: "Features", label: "AA data tunnel", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "feature.input", group: "Features", label: "Input integration", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "feature.launcher", group: "Features", label: "Startup launcher", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "feature.ui", group: "Features", label: "Cluster settings menu", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: true },
    Setting { key: "feature.wireless", group: "Features", label: "Wireless TCP transport", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "main_fps", group: "Displays", label: "Frame rate|0=Automatic|30=30 fps", boolean: false, default: 0, min: 0, max: 30, step: 30, readonly: false },
    Setting { key: "media_audio_codec", group: "Advanced", label: "Music codec|0=PCM|2=AAC-LC|4=AAC-LC ADTS", boolean: false, default: 0, min: 0, max: 4, step: 2, readonly: false },
    Setting { key: "sensor_cred", group: "Advanced", label: "EXLAP credential selection", boolean: false, default: 2, min: -2147483648, max: 2147483647, step: 1, readonly: false },
    Setting { key: "sensor_raw_ev_trip", group: "Advanced", label: "Read the raw EV trip file", boolean: true, default: 0, min: 0, max: 1, step: 1, readonly: false },
    Setting { key: "ui_encode_h", group: "Advanced", label: "Menu video height (px)", boolean: false, default: 480, min: -2147483648, max: 2147483647, step: 1, readonly: true },
    Setting { key: "ui_encode_w", group: "Advanced", label: "Menu video width (px)", boolean: false, default: 800, min: -2147483648, max: 2147483647, step: 1, readonly: true },
    Setting { key: "ui_idle_timeout_s", group: "Advanced", label: "Menu idle timeout (s)", boolean: false, default: 45, min: -2147483648, max: 2147483647, step: 1, readonly: true },
    Setting { key: "ui_keepalive_ms", group: "Advanced", label: "Menu refresh interval (ms)", boolean: false, default: 1500, min: -2147483648, max: 2147483647, step: 1, readonly: true },
    Setting { key: "ui_trigger_keycode", group: "Advanced", label: "Menu trigger key code", boolean: false, default: 84, min: -2147483648, max: 2147483647, step: 1, readonly: true },
    Setting { key: "wireless_tcp_port", group: "Advanced", label: "Wireless TCP listen port", boolean: false, default: 0, min: -2147483648, max: 2147483647, step: 1, readonly: false },
];

#[rustfmt::skip]
pub const VISIBLE_KEYS: &[&str] = &["ev_routing", "sensor_ev_capacity_wh", "feature.cluster", "main_dpi", "cluster_density", "main_fps", "aa_suppress_navigation_active"];

pub fn visible(key: &str) -> bool {
    VISIBLE_KEYS.contains(&key)
}

pub fn find(key: &str) -> Option<&'static Setting> {
    SETTINGS.iter().find(|s| s.key == key)
}

#[cfg(test)]
mod parked_catalog_tests {
    use super::*;
    #[test]
    fn parked_features_keep_their_settings_without_a_remount_option() {
        for feature in ["wireless", "launcher", "aauplink"] {
            assert!(FEATURES.contains(&feature));
            let key = format!("feature.{feature}");
            assert!(SETTINGS.iter().any(|s| s.key == key && s.default == 0));
        }
        for key in [
            "sensor_send_passenger",
            "wireless_tcp_port",
            "aauplink_router_preflight",
            "aauplink_shm_recheck_ms",
            "aauplink_flush_on_open",
            "aauplink_flush_on_close",
        ] {
            assert!(SETTINGS.iter().any(|s| s.key == key));
        }
        assert!(!SETTINGS.iter().any(|s| s.key == "ui_remount"));
    }
}

#[cfg(test)]
mod driver_visibility_tests {
    use super::*;

    #[test]
    fn driver_pages_exclude_internal_configuration() {
        assert_eq!(
            VISIBLE_KEYS,
            &[
                "ev_routing",
                "sensor_ev_capacity_wh",
                "feature.cluster",
                "main_dpi",
                "cluster_density",
                "main_fps",
                "aa_suppress_navigation_active",
            ]
        );
        for key in VISIBLE_KEYS {
            assert!(!find(key).unwrap().readonly);
        }
        for key in [
            "sensor_cred",
            "identify_as_dhu",
            "cluster_focus_kick_grace_ms",
            "cluster_ready_settle_ms",
            "feature.sensor",
            "feature.input",
        ] {
            assert!(find(key).is_some());
            assert!(!visible(key));
        }
    }
}
