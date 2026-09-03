//! Configuration: load/save `%APPDATA%\TunCat\config.json`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Start automatically at logon (Windows scheduled task, highest privileges).
    pub autostart: bool,
    /// Seconds to wait after startup before the first detection.
    pub autostart_delay_sec: u64,
    /// Seconds between detection cycles.
    pub check_interval_sec: u64,
    /// URL probed to decide whether TUN networking works.
    pub probe_url: String,
    /// Probe timeout in seconds.
    pub probe_timeout_sec: u64,
    /// Consecutive failures required before a repair is triggered.
    pub fail_threshold: u32,
    /// How long the ICS pulse stays enabled, in seconds.
    pub pulse_hold_sec: u64,
    /// Cooldown after a repair before detection resumes.
    pub cooldown_sec: u64,
    /// Stop auto-repairing after this many consecutive failed repairs.
    pub max_consecutive_fixes: u32,
    /// Adapter-name keywords that identify the TUN adapter.
    pub tun_keywords: Vec<String>,
    /// Adapter-name keywords that identify the public (physical) adapter,
    /// ordered by priority.
    pub public_keywords: Vec<String>,
    /// Pulse direction: `public_to_tun` (recommended) or `tun_to_public`.
    pub pulse_direction: PulseDirection,
    /// Restore pre-existing ICS sharing after the pulse.
    pub restore_sharing: bool,
    /// UI theme: `system`, `light` or `dark`.
    pub theme: ThemeMode,
    /// Closing the window minimizes to tray instead of exiting.
    pub close_to_tray: bool,
    /// Show a balloon notification when a repair happens.
    pub notify_on_fix: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PulseDirection {
    /// Physical adapter shares TO the TUN adapter (TUN gets rewritten IP; safer).
    #[serde(rename = "public_to_tun")]
    PublicToTun,
    /// TUN adapter shares to the physical adapter (physical IP rewritten; risky).
    #[serde(rename = "tun_to_public")]
    TunToPublic,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ThemeMode {
    #[serde(rename = "system")]
    System,
    #[serde(rename = "light")]
    Light,
    #[serde(rename = "dark")]
    Dark,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            autostart: false,
            autostart_delay_sec: 10,
            check_interval_sec: 30,
            probe_url: "http://www.gstatic.com/generate_204".to_string(),
            probe_timeout_sec: 3,
            fail_threshold: 2,
            pulse_hold_sec: 2,
            cooldown_sec: 15,
            max_consecutive_fixes: 3,
            tun_keywords: ["mihomo", "Meta Tunnel", "wintun", "vgate0", "clash", "utun"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            public_keywords: ["以太网", "Ethernet", "WLAN", "Wi-Fi", "以太网 "]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            pulse_direction: PulseDirection::PublicToTun,
            restore_sharing: true,
            theme: ThemeMode::System,
            close_to_tray: true,
            notify_on_fix: true,
        }
    }
}

impl Config {
    pub fn config_dir() -> Result<PathBuf> {
        let base =
            std::env::var("APPDATA").context("APPDATA not set, cannot locate config directory")?;
        Ok(PathBuf::from(base).join("TunCat"))
    }

    fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.json"))
    }

    /// Load config, creating the default file on first run.
    pub fn load_or_default() -> Result<Self> {
        let path = Self::config_path()?;
        if !path.exists() {
            let cfg = Self::default();
            cfg.save()?;
            return Ok(cfg);
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let cfg: Config = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let dir = Self::config_dir()?;
        std::fs::create_dir_all(&dir)?;
        let path = Self::config_path()?;
        let raw = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, raw)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_serializes_roundtrip() {
        let cfg = Config::default();
        let raw = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&raw).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn legacy_direction_strings_parse() {
        let raw = r#"{"pulse_direction":"public_to_tun"}"#;
        let cfg: Config = serde_json::from_str(raw).unwrap();
        assert_eq!(cfg.pulse_direction, PulseDirection::PublicToTun);
        assert_eq!(cfg.check_interval_sec, 30); // defaults filled in
    }
}
