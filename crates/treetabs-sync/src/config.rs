//! Where the store lives.
//!
//! Order: `TREETABS_DIR`; for the native host, `treetabs-host.json` next to
//! its binary (each registered host has its own, and it must win, or a test
//! host would write to the user's real store); then `store_dir` from
//! `~/.config/treetabs/config.json`; then `~/.local/share/treetabs`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_dir: Option<PathBuf>,
}

impl Config {
    pub fn read(path: &Path) -> Option<Config> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| ".".into())
}

pub fn user_config_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("treetabs/config.json")
}

/// Config file read by the native host, next to its binary.
pub fn host_config_path(exe: &Path) -> PathBuf {
    exe.with_file_name("treetabs-host.json")
}

pub fn store_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("TREETABS_DIR") {
        return PathBuf::from(d);
    }
    if let Ok(exe) = std::env::current_exe()
        && exe.file_stem().is_some_and(|n| n == "treetabs-host")
        && let Some(d) = Config::read(&host_config_path(&exe)).and_then(|c| c.store_dir)
    {
        return d;
    }
    if let Some(d) = Config::read(&user_config_path()).and_then(|c| c.store_dir) {
        return d;
    }
    treetabs_core::Store::default_dir()
}
