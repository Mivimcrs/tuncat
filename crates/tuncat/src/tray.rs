//! Tray icon: pure-color rounded-square background with a white line-art
//! cat, in four state colors. PNGs are embedded at compile time.

use anyhow::{Context, Result};
use std::sync::{Mutex, OnceLock};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

use tuncat_core::state::CoreState;

pub const TRAY_OK: &[u8] = include_bytes!("../../../assets/tray_ok.png");
pub const TRAY_BUSY: &[u8] = include_bytes!("../../../assets/tray_busy.png");
pub const TRAY_ERR: &[u8] = include_bytes!("../../../assets/tray_err.png");
pub const TRAY_PAUSED: &[u8] = include_bytes!("../../../assets/tray_paused.png");

/// Decode an embedded PNG into raw RGBA plus dimensions.
pub fn decode_png(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().context("png read_info failed")?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).context("png decode failed")?;
    buf.truncate(info.buffer_size());
    Ok((buf, info.width, info.height))
}

/// Decode an embedded PNG into a tray Icon.
pub fn icon_from_png(bytes: &[u8]) -> Result<Icon> {
    let (rgba, width, height) = decode_png(bytes)?;
    Icon::from_rgba(rgba, width, height).context("Icon::from_rgba failed")
}

/// Menu item handles.
#[allow(dead_code)]
pub struct TrayMenu {
    pub show: MenuItem,
    pub check: MenuItem,
    pub fix: MenuItem,
    pub pause: MenuItem,
    pub quit: MenuItem,
}

pub struct Tray {
    icon: Mutex<TrayIcon>,
    menu: TrayMenu,
    state_icon: Mutex<CoreState>,
}

impl Tray {
    pub fn new(start_paused: bool) -> Result<Self> {
        let show = MenuItem::with_id("show", "显示主界面", true, None);
        let check = MenuItem::with_id("check", "立即检测", true, None);
        let fix = MenuItem::with_id("fix", "立即修复", true, None);
        let pause = MenuItem::with_id("pause", "暂停自动修复", true, None);
        let quit = MenuItem::with_id("quit", "退出", true, None);

        let menu = Menu::new();
        let _ = menu.append_items(&[
            &show,
            &check,
            &fix,
            &PredefinedMenuItem::separator(),
            &pause,
            &PredefinedMenuItem::separator(),
            &quit,
        ]);

        let initial = if start_paused { TRAY_PAUSED } else { TRAY_OK };
        let icon = icon_from_png(initial)?;

        let tray = TrayIconBuilder::new()
            .with_tooltip("TunCat")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .build()
            .context("failed to build tray icon")?;

        Ok(Self {
            icon: Mutex::new(tray),
            menu: TrayMenu {
                show,
                check,
                fix,
                pause,
                quit,
            },
            state_icon: Mutex::new(CoreState::BootDelay),
        })
    }

    /// Update the tray icon color to match the core state.
    pub fn set_state(&self, state: CoreState, paused: bool) {
        {
            let mut cur = self.state_icon.lock().unwrap();
            if *cur == state {
                return;
            }
            *cur = state;
        }
        let bytes = if paused {
            TRAY_PAUSED
        } else {
            match state {
                CoreState::Pulsing | CoreState::Cooldown | CoreState::Detecting => TRAY_BUSY,
                CoreState::Error | CoreState::GiveUp => TRAY_ERR,
                _ => TRAY_OK,
            }
        };
        if let Ok(icon) = icon_from_png(bytes) {
            if let Ok(tray) = self.icon.lock() {
                let _ = tray.set_icon(Some(icon));
            }
        }
        let _ = self.menu.pause.set_text(if paused {
            "恢复自动修复"
        } else {
            "暂停自动修复"
        });
    }
}

/// Actions requested via tray menu / icon click.
#[derive(Debug)]
pub enum TrayAction {
    ShowToggle,
    Check,
    Fix,
    PauseToggle,
    Quit,
}

static TRAY_EVENTS: OnceLock<Mutex<std::sync::mpsc::Receiver<TrayIconEvent>>> = OnceLock::new();

/// Install the global tray click handler. Call exactly once before the
/// event loop starts.
pub fn install_tray_handler() {
    TRAY_EVENTS.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            let _ = tx.send(event);
        }));
        Mutex::new(rx)
    });
}

/// Drain pending tray menu selections and clicks into actions.
/// Call every UI frame.
pub fn poll_tray_events() -> Vec<TrayAction> {
    let mut actions = Vec::new();

    if let Ok(event) = MenuEvent::receiver().try_recv() {
        let action = match event.id().0.as_str() {
            "show" => Some(TrayAction::ShowToggle),
            "check" => Some(TrayAction::Check),
            "fix" => Some(TrayAction::Fix),
            "pause" => Some(TrayAction::PauseToggle),
            "quit" => Some(TrayAction::Quit),
            _ => None,
        };
        if let Some(a) = action {
            actions.push(a);
        }
    }

    if let Some(rx) = TRAY_EVENTS.get() {
        while let Ok(event) = rx.lock().unwrap().try_recv() {
            if let TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                ..
            } = event
            {
                actions.push(TrayAction::ShowToggle);
            }
        }
    }

    actions
}
