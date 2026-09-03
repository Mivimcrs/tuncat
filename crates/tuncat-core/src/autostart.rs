//! Autostart via a scheduled task running with highest privileges
//! (`schtasks /SC ONLOGON /RL HIGHEST`). The registry Run key cannot elevate,
//! so a task is required for silent no-UAC startup.

use anyhow::{Context, Result};
use std::process::Command;

const TASK_NAME: &str = "TunCat";

fn schtasks(args: &[&str]) -> Result<std::process::Output> {
    Command::new("schtasks")
        .args(args)
        .output()
        .context("failed to launch schtasks")
}

/// Create (or overwrite) the logon task pointing at the current exe.
pub fn install() -> Result<()> {
    let exe = std::env::current_exe().context("cannot resolve current exe path")?;
    let tr = format!("\"{}\" --silent", exe.display());
    let out = schtasks(&[
        "/Create", "/F", "/TN", TASK_NAME, "/TR", &tr, "/SC", "ONLOGON", "/RL", "HIGHEST",
    ])?;
    if !out.status.success() {
        anyhow::bail!(
            "schtasks /Create failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Delete the logon task. Missing task is treated as success.
pub fn uninstall() -> Result<()> {
    let out = schtasks(&["/Delete", "/F", "/TN", TASK_NAME])?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success()
        && !stderr.contains("无法找到")
        && !stderr.to_lowercase().contains("cannot find")
    {
        anyhow::bail!("schtasks /Delete failed: {}", stderr.trim());
    }
    Ok(())
}

/// Whether the logon task currently exists.
pub fn is_installed() -> bool {
    schtasks(&["/Query", "/TN", TASK_NAME])
        .map(|o| o.status.success())
        .unwrap_or(false)
}
