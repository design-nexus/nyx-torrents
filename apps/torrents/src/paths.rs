//! Well-known locations. Every path honours the XDG overrides so the whole app
//! can be pointed at a scratch copy of `~/.config` for testing.

pub use nyx_core::paths::*;

use std::path::PathBuf;

pub fn categories_file() -> PathBuf {
    app_dir().join("categories.toml")
}

pub fn resume_dir() -> PathBuf {
    data_dir().join("resume")
}

pub fn session_file() -> PathBuf {
    data_dir().join("session.dat")
}

pub fn queue_file() -> PathBuf {
    data_dir().join("queue")
}

/// Per-torrent app data: category, tags, where it should end up.
pub fn torrents_file() -> PathBuf {
    data_dir().join("torrents.json")
}

pub fn search_dir() -> PathBuf {
    data_dir().join("search")
}

pub fn engines_dir() -> PathBuf {
    search_dir().join("engines")
}

pub fn qbittorrent_engines() -> PathBuf {
    data_home().join("qBittorrent/nova3/engines")
}

pub fn downloads() -> PathBuf {
    let dirs = config_home().join("user-dirs.dirs");
    if let Ok(text) = std::fs::read_to_string(dirs) {
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("XDG_DOWNLOAD_DIR=") {
                let v = v.trim().trim_matches('"').replace("$HOME", &home().to_string_lossy());
                if !v.is_empty() {
                    return PathBuf::from(v);
                }
            }
        }
    }
    home().join("Downloads")
}
