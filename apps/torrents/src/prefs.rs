//! This app's own preferences (`~/.config/torrents/settings.toml`): the window's
//! look and every torrent setting.

pub use nyx_theme::ThemeMode;

use crate::paths;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Prefs {
    // ----- This window -----
    pub mode: ThemeMode,
    pub theme: String,
    pub reduce_motion: bool,
    /// The sidebar shows only icons, whatever the window width.
    pub sidebar_collapsed: bool,
    pub glow: bool,
    pub last_section: String,
    pub hidden_columns: Vec<String>,
    /// Height of the details pane under the torrent list, in pixels.
    pub details_height: i32,
    pub details_tab: String,
    /// How much history the speed graphs keep, in seconds.
    pub history_secs: u64,
    /// Fixed; the graphs read it to label time.
    pub interval_ms: u64,
    /// Closing the window keeps torrents going; `torrents` brings it back.
    pub keep_running: bool,
    pub notify_finished: bool,
    pub net_bits: bool,

    // ----- Downloads -----
    pub save_path: String,
    pub use_incomplete: bool,
    pub incomplete_path: String,
    pub show_add_dialog: bool,
    pub start_paused: bool,
    pub preallocate: bool,
    pub delete_torrent_file: bool,
    /// original, subfolder or none.
    pub content_layout: String,
    /// Categories without their own folder save into a subfolder named after them.
    pub category_subfolders: bool,
    pub on_finish: String,
    /// Trackers added to every new torrent, one per line.
    pub extra_trackers: String,

    // ----- Connection -----
    pub port: u16,
    pub upnp: bool,
    /// Network interface to bind to ("" for any), e.g. a VPN's `wg0`.
    pub interface: String,
    /// both, tcp or utp.
    pub protocol: String,
    pub max_connections: i32,
    pub max_upload_slots: i32,
    /// none, socks5, socks4 or http.
    pub proxy_type: String,
    pub proxy_host: String,
    pub proxy_port: u16,
    pub proxy_user: String,
    pub proxy_password: String,
    pub proxy_peers: bool,
    pub proxy_hostnames: bool,

    // ----- Speed (KiB/s, 0 = unlimited) -----
    pub dl_limit: i32,
    pub ul_limit: i32,
    pub alt_dl_limit: i32,
    pub alt_ul_limit: i32,
    pub alt_enabled: bool,
    pub schedule: bool,
    pub schedule_from: String,
    pub schedule_to: String,
    /// every, weekdays or weekends.
    pub schedule_days: String,
    pub limit_overhead: bool,

    // ----- BitTorrent -----
    pub dht: bool,
    pub pex: bool,
    pub lsd: bool,
    /// allow, prefer, require or disable.
    pub encryption: String,
    pub anonymous: bool,
    pub queueing: bool,
    pub max_active_downloads: i32,
    pub max_active_uploads: i32,
    pub max_active_torrents: i32,
    pub ignore_slow: bool,
    pub slow_dl: i32,
    pub slow_ul: i32,
    /// 0 = no limit.
    pub ratio_limit: f64,
    /// Minutes; 0 = no limit.
    pub seed_time_limit: i64,
    /// pause, remove, remove-files or superseed.
    pub limit_action: String,

    // ----- Search -----
    pub disabled_engines: Vec<String>,
    pub search_category: String,
    /// Queries kept in the sidebar, newest last.
    pub saved_searches: Vec<String>,

    // ----- Home -----
    /// How far back Home's speed graph reaches: 5min, hour or today.
    pub home_span: String,
}

impl Default for Prefs {
    fn default() -> Self {
        let downloads = paths::downloads();
        Self {
            mode: ThemeMode::Omarchy,
            theme: "tokyo-night".into(),
            reduce_motion: false,
            sidebar_collapsed: false,
            glow: true,
            last_section: "home".into(),
            hidden_columns: vec!["added".into(), "tags".into(), "availability".into()],
            details_height: 280,
            details_tab: "general".into(),
            history_secs: 300,
            interval_ms: 1000,
            keep_running: false,
            notify_finished: true,
            net_bits: false,

            save_path: downloads.to_string_lossy().into(),
            use_incomplete: false,
            incomplete_path: downloads.join("Incomplete").to_string_lossy().into(),
            show_add_dialog: true,
            start_paused: false,
            preallocate: false,
            delete_torrent_file: false,
            content_layout: "original".into(),
            category_subfolders: true,
            on_finish: String::new(),
            extra_trackers: String::new(),

            port: 0,
            upnp: true,
            interface: String::new(),
            protocol: "both".into(),
            max_connections: 500,
            max_upload_slots: 20,
            proxy_type: "none".into(),
            proxy_host: String::new(),
            proxy_port: 1080,
            proxy_user: String::new(),
            proxy_password: String::new(),
            proxy_peers: true,
            proxy_hostnames: true,

            dl_limit: 0,
            ul_limit: 0,
            alt_dl_limit: 1024,
            alt_ul_limit: 256,
            alt_enabled: false,
            schedule: false,
            schedule_from: "08:00".into(),
            schedule_to: "20:00".into(),
            schedule_days: "every".into(),
            limit_overhead: false,

            dht: true,
            pex: true,
            lsd: true,
            encryption: "allow".into(),
            anonymous: false,
            queueing: true,
            max_active_downloads: 3,
            max_active_uploads: 3,
            max_active_torrents: 5,
            ignore_slow: true,
            slow_dl: 2,
            slow_ul: 2,
            ratio_limit: 0.0,
            seed_time_limit: 0,
            limit_action: "pause".into(),

            disabled_engines: Vec::new(),
            search_category: "all".into(),
            saved_searches: Vec::new(),

            home_span: "hour".into(),
        }
    }
}

nyx_core::prefs_store!(Prefs, nyx_core::prefs::Store::standard_with(fix));

/// Like qBittorrent, pick a random listening port on first run (and keep it).
fn fix(p: &mut Prefs) -> bool {
    if p.port == 0 {
        p.port = random_port();
        return true;
    }
    false
}

pub fn random_port() -> u16 {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let seed = nanos ^ std::process::id().wrapping_mul(2_654_435_761);
    10_000 + (seed % 50_000) as u16
}
