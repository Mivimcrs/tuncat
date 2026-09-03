//! Core worker: state machine running on a background thread, communicating
//! with the UI over channels. The UI is just a subscriber — the core keeps
//! working with the window closed.

use anyhow::Result;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

use crate::config::{Config, PulseDirection};
use crate::detector::{self, ProbeResult};
use crate::ics::IcsPulser;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreState {
    /// Waiting out the startup delay.
    BootDelay,
    /// Healthy / nothing to do.
    Idle,
    /// A detection cycle is running.
    Detecting,
    /// ICS pulse in progress.
    Pulsing,
    /// Cooling down after a repair.
    Cooldown,
    /// TUN adapter absent — nothing to do.
    NoTun,
    /// Repair failed; will retry next cycle.
    Error,
    /// Too many consecutive failed repairs; auto-repair suspended.
    GiveUp,
}

impl CoreState {
    pub fn label(self) -> &'static str {
        match self {
            CoreState::BootDelay => "启动延迟中",
            CoreState::Idle => "运行正常",
            CoreState::Detecting => "检测中",
            CoreState::Pulsing => "修复中",
            CoreState::Cooldown => "修复后冷却",
            CoreState::NoTun => "未发现 TUN 网卡",
            CoreState::Error => "上次修复失败",
            CoreState::GiveUp => "已停止自动修复",
        }
    }
}

#[derive(Debug, Clone)]
pub struct StatusSnapshot {
    pub state: CoreState,
    pub tun_adapter: Option<String>,
    pub paused: bool,
    pub checks: u64,
    pub fixes: u64,
    pub fix_successes: u64,
    pub last_probe_ok: Option<bool>,
    pub last_probe_detail: Option<String>,
    pub last_fix_time: Option<String>,
    pub last_error: Option<String>,
    pub next_check_in: Option<u64>,
}

impl Default for StatusSnapshot {
    fn default() -> Self {
        Self {
            state: CoreState::BootDelay,
            tun_adapter: None,
            paused: false,
            checks: 0,
            fixes: 0,
            fix_successes: 0,
            last_probe_ok: None,
            last_probe_detail: None,
            last_fix_time: None,
            last_error: None,
            next_check_in: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: LogLevel,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// Commands from UI to core.
#[derive(Debug)]
pub enum CoreCommand {
    Shutdown,
    DetectNow,
    FixNow,
    SetPaused(bool),
    SetConfig(Config),
}

/// Events from core to UI.
#[derive(Debug)]
pub enum CoreEvent {
    Status(StatusSnapshot),
    Log(LogEntry),
}

/// Handle held by the UI.
pub struct CoreHandle {
    pub cmd_tx: Sender<CoreCommand>,
    pub event_rx: Receiver<CoreEvent>,
}

impl CoreHandle {
    pub fn spawn(config: Config) -> Self {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("tuncat-core".into())
            .spawn(move || {
                let mut worker = Worker {
                    config,
                    cmd_rx,
                    event_tx: event_tx.clone(),
                    status: StatusSnapshot::default(),
                    fail_count: 0,
                    consecutive_fixes: 0,
                    force_fix: false,
                };
                worker.run();
            })
            .expect("failed to spawn core worker thread");
        Self { cmd_tx, event_rx }
    }
}

struct Worker {
    config: Config,
    cmd_rx: Receiver<CoreCommand>,
    event_tx: Sender<CoreEvent>,
    status: StatusSnapshot,
    fail_count: u32,
    consecutive_fixes: u32,
    force_fix: bool,
}

impl Worker {
    fn emit_status(&mut self, state: CoreState) {
        self.status.state = state;
        let _ = self.event_tx.send(CoreEvent::Status(self.status.clone()));
    }

    fn emit_log(&self, level: LogLevel, message: impl Into<String>) {
        let msg: String = message.into();
        match level {
            LogLevel::Info => info!("{}", msg),
            LogLevel::Warn => warn!("{}", msg),
            LogLevel::Error => error!("{}", msg),
        }
        let timestamp = chrono_like_now();
        let _ = self.event_tx.send(CoreEvent::Log(LogEntry {
            timestamp,
            level,
            message: msg,
        }));
    }

    fn run(&mut self) {
        let mut next_action =
            Instant::now() + Duration::from_secs(self.config.autostart_delay_sec.max(1));
        self.emit_status(CoreState::BootDelay);
        self.emit_log(
            LogLevel::Info,
            format!(
                "TunCat core 已启动，{} 秒后开始首次检测（间隔 {} 秒）",
                self.config.autostart_delay_sec, self.config.check_interval_sec
            ),
        );

        loop {
            let now = Instant::now();
            let wait = if next_action > now {
                (next_action - now).min(Duration::from_millis(1000))
            } else {
                Duration::from_millis(1)
            };
            let fired = match self.cmd_rx.recv_timeout(wait) {
                Ok(cmd) => {
                    if self.handle_command(cmd, &mut next_action) {
                        return; // Shutdown
                    }
                    false
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => true,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    // UI gone; keep running for the tray until process exit.
                    true
                }
            };

            let now = Instant::now();
            if fired && now >= next_action {
                if self.status.paused {
                    next_action = now + Duration::from_secs(self.config.check_interval_sec.max(5));
                    self.status.next_check_in = Some(self.config.check_interval_sec.max(5));
                    continue;
                }
                self.detect_cycle(&mut next_action);
            }
        }
    }

    /// Returns true when the worker should exit.
    fn handle_command(&mut self, cmd: CoreCommand, next_action: &mut Instant) -> bool {
        match cmd {
            CoreCommand::Shutdown => return true,
            CoreCommand::DetectNow => {
                *next_action = Instant::now();
            }
            CoreCommand::FixNow => {
                self.force_fix = true;
                *next_action = Instant::now();
                self.emit_log(LogLevel::Info, "收到手动修复指令");
            }
            CoreCommand::SetPaused(paused) => {
                self.status.paused = paused;
                self.emit_log(
                    LogLevel::Info,
                    if paused {
                        "自动修复已暂停"
                    } else {
                        "自动修复已恢复"
                    },
                );
                let _ = self.event_tx.send(CoreEvent::Status(self.status.clone()));
            }
            CoreCommand::SetConfig(cfg) => {
                self.config = cfg;
                self.emit_log(LogLevel::Info, "配置已更新");
            }
        }
        false
    }

    fn detect_cycle(&mut self, next_action: &mut Instant) {
        let interval = Duration::from_secs(self.config.check_interval_sec.max(5));
        *next_action = Instant::now() + interval;
        self.status.next_check_in = Some(interval.as_secs());

        self.emit_status(CoreState::Detecting);
        self.status.checks += 1;

        let adapters = match detector::list_adapters() {
            Ok(a) => a,
            Err(e) => {
                self.status.last_error = Some(e.to_string());
                self.emit_log(LogLevel::Error, format!("网卡枚举失败: {e}"));
                self.emit_status(CoreState::Error);
                return;
            }
        };
        let tun = detector::find_tun(&adapters, &self.config.tun_keywords);
        self.status.tun_adapter = tun.map(|t| t.friendly_name.clone());

        let Some(tun) = tun else {
            // No TUN adapter: not an error, just idle.
            self.fail_count = 0;
            self.status.last_probe_ok = None;
            self.status.last_probe_detail = Some("TUN 网卡不存在或未启用".into());
            self.emit_status(CoreState::NoTun);
            return;
        };

        let probe = detector::probe(&self.config.probe_url, self.config.probe_timeout_sec);
        match &probe {
            ProbeResult::Healthy(latency) => {
                self.fail_count = 0;
                self.consecutive_fixes = 0;
                self.status.last_probe_ok = Some(true);
                self.status.last_probe_detail = Some(format!("{} ms", latency.as_millis()));
            }
            ProbeResult::Unhealthy(reason) => {
                self.status.last_probe_ok = Some(false);
                self.status.last_probe_detail = Some(reason.clone());
                self.fail_count += 1;
                self.emit_log(
                    LogLevel::Warn,
                    format!(
                        "探测失败（第 {}/{} 次）：{} [TUN={}]",
                        self.fail_count, self.config.fail_threshold, reason, tun.friendly_name
                    ),
                );
            }
        }

        let need_fix = self.force_fix
            || (matches!(probe, ProbeResult::Unhealthy(_))
                && self.fail_count >= self.config.fail_threshold);
        self.force_fix = false;

        if !need_fix {
            self.emit_status(CoreState::Idle);
            return;
        }

        // --- repair ---
        let public = detector::find_public(&adapters, &self.config.public_keywords);
        let Some(public) = public else {
            self.status.last_error = Some("找不到可用的物理网卡".into());
            self.emit_log(LogLevel::Error, "修复中止：找不到带网关的活动物理网卡");
            self.emit_status(CoreState::Error);
            return;
        };

        self.emit_status(CoreState::Pulsing);
        self.status.fixes += 1;
        let started = Instant::now();
        let result = self.run_pulse(&tun.friendly_name, &public.friendly_name);
        match result {
            Ok(report) => {
                self.status.last_fix_time = Some(local_time_string());
                self.status.fix_successes += 1;
                self.emit_log(
                    LogLevel::Info,
                    format!(
                        "ICS 脉冲完成（{}），冷却 {} 秒",
                        started.elapsed().as_secs(),
                        self.config.cooldown_sec
                    ),
                );
                if !report.restore_failures.is_empty() {
                    self.emit_log(
                        LogLevel::Warn,
                        format!(
                            "以下网卡的共享状态未能恢复: {}",
                            report.restore_failures.join(", ")
                        ),
                    );
                }
            }
            Err(e) => {
                self.consecutive_fixes += 1;
                self.status.last_error = Some(e.to_string());
                self.emit_log(LogLevel::Error, format!("ICS 脉冲失败: {e:#}"));
                if self.consecutive_fixes >= self.config.max_consecutive_fixes {
                    self.emit_log(
                        LogLevel::Error,
                        format!(
                            "连续 {} 次修复失败，停止自动修复（手动修复仍可用）",
                            self.consecutive_fixes
                        ),
                    );
                    self.emit_status(CoreState::GiveUp);
                    *next_action = Instant::now()
                        + Duration::from_secs(self.config.check_interval_sec.max(5) * 4);
                    return;
                }
            }
        }

        // Cooldown, then resume normal detection.
        *next_action = Instant::now() + Duration::from_secs(self.config.cooldown_sec.max(5));
        self.status.next_check_in = Some(self.config.cooldown_sec.max(5));
        self.fail_count = 0;
        self.emit_status(CoreState::Cooldown);
    }

    fn run_pulse(&mut self, tun_name: &str, public_name: &str) -> Result<crate::ics::PulseReport> {
        let pulser = IcsPulser::new()?;
        let direction: PulseDirection = self.config.pulse_direction;
        let hold = self.config.pulse_hold_sec;
        let restore = self.config.restore_sharing;
        pulser.pulse(tun_name, public_name, direction, hold, restore)
    }
}

/// Local time as `HH:MM:SS` without pulling in a datetime crate.
fn local_time_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // UTC+8 assumption is fine for log timestamps; not used for logic.
    let local = secs + 8 * 3600;
    let day_secs = local % 86400;
    format!(
        "{:02}:{:02}:{:02}",
        day_secs / 3600,
        (day_secs % 3600) / 60,
        day_secs % 60
    )
}

fn chrono_like_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let local = secs + 8 * 3600;
    let days = local / 86400;
    let day_secs = local % 86400;
    // Civil-from-days algorithm (Howard Hinnant), valid for current era.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        y,
        m,
        d,
        day_secs / 3600,
        (day_secs % 3600) / 60,
        day_secs % 60
    )
}
