//! Main window: three tabs (status / logs / settings), light & dark themes.

use eframe::egui;
use egui::{Color32, RichText, Sense};
use std::collections::VecDeque;

use tuncat_core::autostart;
use tuncat_core::config::{Config, PulseDirection, ThemeMode};
use tuncat_core::state::{CoreCommand, CoreEvent, CoreHandle, CoreState, LogLevel, StatusSnapshot};

use crate::platform;
use crate::tray::{Tray, TrayAction};

const BRAND: Color32 = Color32::from_rgb(0x2F, 0x7D, 0xD6);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Status,
    Logs,
    Settings,
}

pub struct TunCatApp {
    pub core: CoreHandle,
    pub tray: Tray,
    pub config: Config,
    pub config_dirty: bool,
    pub status: StatusSnapshot,
    pub logs: VecDeque<(String, LogLevel, String)>,
    pub tab: Tab,
    pub window_visible: bool,
    pub elevated: bool,
    pub shutdown: bool,
    pub ctx: Option<egui::Context>,
    pub pending_config: Config,
}

impl TunCatApp {
    pub fn new(core: CoreHandle, tray: Tray, config: Config, elevated: bool) -> Self {
        Self {
            core,
            tray,
            config: config.clone(),
            config_dirty: false,
            pending_config: config,
            status: StatusSnapshot::default(),
            logs: VecDeque::new(),
            tab: Tab::Status,
            window_visible: true,
            elevated,
            shutdown: false,
            ctx: None,
        }
    }

    /// Drain core events and tray events. Called every frame.
    pub fn pump_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.core.event_rx.try_recv() {
            match event {
                CoreEvent::Status(s) => {
                    self.tray.set_state(s.state, s.paused);
                    self.status = s;
                }
                CoreEvent::Log(entry) => {
                    if self.logs.len() >= 500 {
                        self.logs.pop_front();
                    }
                    self.logs
                        .push_back((entry.timestamp, entry.level, entry.message));
                }
            }
        }

        for action in crate::tray::poll_tray_events() {
            match action {
                TrayAction::ShowToggle => self.toggle_window(ctx),
                TrayAction::Check => {
                    let _ = self.core.cmd_tx.send(CoreCommand::DetectNow);
                }
                TrayAction::Fix => {
                    let _ = self.core.cmd_tx.send(CoreCommand::FixNow);
                }
                TrayAction::PauseToggle => {
                    let paused = !self.status.paused;
                    let _ = self.core.cmd_tx.send(CoreCommand::SetPaused(paused));
                    self.status.paused = paused;
                    self.tray.set_state(self.status.state, paused);
                }
                TrayAction::Quit => {
                    self.shutdown = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }

        // Intercept the close button: minimize to tray or allow quit.
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested && !self.shutdown {
            if self.config.close_to_tray {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                if self.window_visible {
                    self.window_visible = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                }
            }
        }

        // Keep the loop alive in the background so the tray keeps working.
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }

    fn toggle_window(&mut self, ctx: &egui::Context) {
        self.window_visible = !self.window_visible;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(self.window_visible));
    }

    fn apply_config(&mut self) {
        if self.config_dirty {
            self.config = self.pending_config.clone();
            if let Err(e) = self.config.save() {
                self.logs
                    .push_back((String::new(), LogLevel::Error, format!("配置保存失败: {e}")));
            }
            let _ = self
                .core
                .cmd_tx
                .send(CoreCommand::SetConfig(self.config.clone()));
            self.config_dirty = false;
        }
    }

    pub fn theme_visuals(mode: ThemeMode) -> egui::Visuals {
        match mode {
            ThemeMode::Light => egui::Visuals::light(),
            ThemeMode::Dark => egui::Visuals::dark(),
            ThemeMode::System => {
                if platform::system_prefers_light() {
                    egui::Visuals::light()
                } else {
                    egui::Visuals::dark()
                }
            }
        }
    }
}

impl eframe::App for TunCatApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ctx = Some(ctx.clone());
        self.pump_events(ctx);

        egui::TopBottomPanel::top("topbar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("🐱 TunCat").size(18.0).color(BRAND));
                ui.separator();
                ui.selectable_value(&mut self.tab, Tab::Status, "状态");
                ui.selectable_value(&mut self.tab, Tab::Logs, "日志");
                ui.selectable_value(&mut self.tab, Tab::Settings, "设置");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if self.status.paused {
                        "已暂停"
                    } else {
                        self.status.state.label()
                    };
                    let color = state_color(self.status.state);
                    ui.label(RichText::new(label).color(color).strong());
                });
            });
            ui.add_space(2.0);
        });

        if !self.elevated {
            egui::TopBottomPanel::top("elev_warn").show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(
                        Color32::from_rgb(0xC0, 0x39, 0x2B),
                        RichText::new("⚠ 当前未以管理员身份运行，修复功能不可用。").strong(),
                    );
                    if ui.button("以管理员身份重启").clicked() {
                        let _ = platform::restart_elevated();
                    }
                });
            });
        }

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Status => self.ui_status(ui),
            Tab::Logs => self.ui_logs(ui),
            Tab::Settings => self.ui_settings(ui),
        });
    }
}

impl TunCatApp {
    fn ui_status(&mut self, ui: &mut egui::Ui) {
        egui::Frame::group(ui.style())
            .fill(ui.visuals().faint_bg_color)
            .show(ui, |ui| {
                ui.set_min_height(90.0);
                ui.vertical_centered(|ui| {
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(self.status.state.label())
                            .size(26.0)
                            .color(state_color(self.status.state))
                            .strong(),
                    );
                    ui.add_space(4.0);
                    match (&self.status.tun_adapter, &self.status.last_probe_detail) {
                        (Some(tun), Some(detail)) => {
                            ui.label(format!("TUN 网卡: {tun}"));
                            let ok = self.status.last_probe_ok == Some(true);
                            let sym = if ok { "✓" } else { "✗" };
                            let color = if ok {
                                Color32::from_rgb(0x2F, 0xA4, 0x6A)
                            } else {
                                Color32::from_rgb(0xD2, 0x54, 0x46)
                            };
                            ui.label(RichText::new(format!("{sym} 探测: {detail}")).color(color));
                        }
                        (Some(tun), None) => {
                            ui.label(format!("TUN 网卡: {tun}"));
                        }
                        _ => {
                            ui.label("未发现 TUN 网卡（Clash TUN 未开启时属正常）");
                        }
                    }
                    if let Some(err) = &self.status.last_error {
                        ui.label(
                            RichText::new(format!("最近错误: {err}"))
                                .color(Color32::from_rgb(0xC0, 0x39, 0x2B)),
                        );
                    }
                    ui.add_space(10.0);
                });
            });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("立即检测").clicked() {
                let _ = self.core.cmd_tx.send(CoreCommand::DetectNow);
            }
            if ui.button("立即修复").clicked() {
                let _ = self.core.cmd_tx.send(CoreCommand::FixNow);
            }
            let paused = self.status.paused;
            if ui
                .button(if paused {
                    "恢复自动修复"
                } else {
                    "暂停自动修复"
                })
                .clicked()
            {
                let _ = self.core.cmd_tx.send(CoreCommand::SetPaused(!paused));
                self.status.paused = !paused;
                self.tray.set_state(self.status.state, !paused);
            }
        });

        ui.add_space(12.0);
        ui.heading("统计");
        let (checks, fixes, fix_ok) = (
            self.status.checks,
            self.status.fixes,
            self.status.fix_successes,
        );
        let last_fix = self.status.last_fix_time.clone();
        egui::Grid::new("stats").num_columns(2).show(ui, |ui| {
            ui.label("本次运行检测次数");
            ui.label(checks.to_string());
            ui.end_row();
            ui.label("修复尝试");
            ui.label(format!("{fixes} 次（成功 {fix_ok} 次）"));
            ui.end_row();
            ui.label("上次修复时间");
            ui.label(last_fix.unwrap_or_else(|| "—".into()));
            ui.end_row();
        });
        ui.add_space(6.0);
        let _ = ui.add(
            egui::Label::new("提示：关闭窗口将最小化到托盘，程序在后台继续守护。")
                .sense(Sense::click()),
        );
    }

    fn ui_logs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("清空显示").clicked() {
                self.logs.clear();
            }
            if ui.button("打开日志目录").clicked() {
                if let Ok(dir) = Config::config_dir().map(|d| d.join("logs")) {
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = open_path(&dir);
                }
            }
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.logs.is_empty() {
                    ui.weak("暂无日志");
                }
                for (ts, level, msg) in &self.logs {
                    let color = match level {
                        LogLevel::Info => ui.visuals().text_color(),
                        LogLevel::Warn => Color32::from_rgb(0xB8, 0x74, 0x0B),
                        LogLevel::Error => Color32::from_rgb(0xC0, 0x39, 0x2B),
                    };
                    ui.label(
                        RichText::new(format!("[{ts}] {msg}"))
                            .monospace()
                            .color(color),
                    );
                }
            });
    }

    fn ui_settings(&mut self, ui: &mut egui::Ui) {
        let cfg = &mut self.pending_config;
        let mut changed = false;

        egui::CollapsingHeader::new("启动")
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    changed |= ui
                        .checkbox(&mut cfg.autostart, "开机自动启动（计划任务，静默无弹窗）")
                        .changed();
                });
                if cfg.autostart && ui.button("立即安装开机自启任务").clicked() {
                    match autostart::install() {
                        Ok(()) => self.logs.push_back((
                            String::new(),
                            LogLevel::Info,
                            "开机自启计划任务已安装".into(),
                        )),
                        Err(e) => self.logs.push_back((
                            String::new(),
                            LogLevel::Error,
                            format!("自启任务安装失败: {e:#}"),
                        )),
                    }
                }
                changed |=
                    number_field(ui, "启动后延迟（秒）", &mut cfg.autostart_delay_sec, 1, 600);
            });

        ui.add_space(4.0);
        egui::CollapsingHeader::new("检测")
            .default_open(true)
            .show(ui, |ui| {
                let mut interval = cfg.check_interval_sec as i32;
                changed |= int_slider(ui, "检测间隔（秒）", &mut interval, 10, 600);
                cfg.check_interval_sec = interval.max(5) as u64;
                ui.horizontal(|ui| {
                    ui.label("探测地址:");
                    changed |= ui
                        .add(egui::TextEdit::singleline(&mut cfg.probe_url).desired_width(320.0))
                        .changed();
                });
                changed |= number_field(ui, "探测超时（秒）", &mut cfg.probe_timeout_sec, 1, 30);
                let mut threshold = cfg.fail_threshold as i32;
                changed |= int_slider(ui, "连续失败几次触发修复", &mut threshold, 1, 10);
                cfg.fail_threshold = threshold.max(1) as u32;
            });

        ui.add_space(4.0);
        egui::CollapsingHeader::new("修复")
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("脉冲方向:");
                    let mut dir = matches!(cfg.pulse_direction, PulseDirection::PublicToTun);
                    if ui
                        .checkbox(&mut dir, "物理网卡 → TUN（推荐，只临时改 TUN 的 IP）")
                        .changed()
                    {
                        cfg.pulse_direction = if dir {
                            PulseDirection::PublicToTun
                        } else {
                            PulseDirection::TunToPublic
                        };
                        changed = true;
                    }
                });
                changed |= number_field(ui, "ICS 保持时长（秒）", &mut cfg.pulse_hold_sec, 1, 10);
                changed |= number_field(ui, "修复后冷却（秒）", &mut cfg.cooldown_sec, 5, 300);
                let mut maxfix = cfg.max_consecutive_fixes as i32;
                changed |= int_slider(ui, "连续失败几次后停止自动修复", &mut maxfix, 1, 10);
                cfg.max_consecutive_fixes = maxfix.max(1) as u32;
                changed |= ui
                    .checkbox(&mut cfg.restore_sharing, "修复后恢复原有 ICS 共享设置")
                    .changed();
            });

        ui.add_space(4.0);
        egui::CollapsingHeader::new("网卡识别关键词")
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("TUN 网卡关键词:");
                    changed |= keywords_field(ui, &mut cfg.tun_keywords);
                });
                ui.horizontal(|ui| {
                    ui.label("物理网卡关键词:");
                    changed |= keywords_field(ui, &mut cfg.public_keywords);
                });
            });

        ui.add_space(4.0);
        egui::CollapsingHeader::new("外观")
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("主题:");
                    let modes = [
                        (ThemeMode::System, "跟随系统"),
                        (ThemeMode::Light, "浅色"),
                        (ThemeMode::Dark, "深色"),
                    ];
                    ui.horizontal(|ui| {
                        for (mode, label) in modes {
                            if ui.selectable_label(cfg.theme == mode, label).clicked() {
                                cfg.theme = mode;
                                changed = true;
                            }
                        }
                    });
                });
                changed |= ui
                    .checkbox(&mut cfg.close_to_tray, "关闭窗口时最小化到托盘（不退出）")
                    .changed();
                changed |= ui
                    .checkbox(&mut cfg.notify_on_fix, "修复发生时弹气泡通知")
                    .changed();
            });

        if changed {
            self.config_dirty = true;
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.config_dirty, egui::Button::new("保存并应用"))
                .clicked()
            {
                self.apply_config();
                ctx_set_theme(ui.ctx(), self.config.theme);
            }
            if ui.button("恢复默认").clicked() {
                self.pending_config = Config::default();
                self.config_dirty = true;
            }
        });
    }
}

fn ctx_set_theme(ctx: &egui::Context, mode: ThemeMode) {
    ctx.set_visuals(TunCatApp::theme_visuals(mode));
}

fn state_color(state: CoreState) -> Color32 {
    match state {
        CoreState::Idle | CoreState::Cooldown => Color32::from_rgb(0x2F, 0xA4, 0x6A),
        CoreState::Pulsing | CoreState::Detecting | CoreState::BootDelay => {
            Color32::from_rgb(0xB8, 0x74, 0x0B)
        }
        CoreState::Error | CoreState::GiveUp => Color32::from_rgb(0xC0, 0x39, 0x2B),
        CoreState::NoTun => Color32::from_rgb(0x80, 0x80, 0x80),
    }
}

fn number_field(ui: &mut egui::Ui, label: &str, value: &mut u64, min: u64, max: u64) -> bool {
    let mut v = *value as i32;
    let changed = int_slider(ui, label, &mut v, min as i32, max as i32);
    *value = v.max(min as i32) as u64;
    changed
}

fn int_slider(ui: &mut egui::Ui, label: &str, value: &mut i32, min: i32, max: i32) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(format!("{label}:"));
        changed |= ui
            .add(egui::Slider::new(value, min..=max).show_value(true))
            .changed();
    });
    changed
}

fn keywords_field(ui: &mut egui::Ui, keywords: &mut Vec<String>) -> bool {
    let mut text = keywords.join(", ");
    let changed = ui
        .add(egui::TextEdit::singleline(&mut text).desired_width(320.0))
        .changed();
    if changed {
        *keywords = text
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    changed
}

fn open_path(path: &std::path::Path) -> anyhow::Result<()> {
    std::process::Command::new("explorer")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(Into::into)
}
