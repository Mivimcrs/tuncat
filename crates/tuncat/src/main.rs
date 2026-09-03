#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! TunCat — automatic ICS-pulse repair for TUN adapters conflicting with
//! OPPO Connect. Tray-resident, auto-starts hidden with `--silent`.

mod app;
mod platform;
mod tray;

use eframe::egui;

fn main() -> eframe::Result<()> {
    // Require administrator (ICS needs it); UAC prompt on manual launch.
    #[cfg(windows)]
    {
        use embed_manifest::manifest::ExecutionLevel;
        let _ = embed_manifest::embed_manifest(
            embed_manifest::new_manifest("TunCat")
                .requested_execution_level(ExecutionLevel::RequireAdministrator),
        );
    }

    // Single instance guard.
    if !platform::acquire_single_instance() {
        return Ok(());
    }

    let silent = std::env::args().any(|a| a == "--silent");

    let config = tuncat_core::config::Config::load_or_default().unwrap_or_default();
    let _log_guard = tuncat_core::init_logging().ok();
    tuncat_core::prune_old_logs();

    let elevated = platform::is_elevated();
    let core = tuncat_core::state::CoreHandle::spawn(config.clone());

    tray::install_tray_handler();
    let tray = tray::Tray::new(false).expect("failed to create tray icon");

    // Window icon from the same cat artwork.
    let icon = tray::decode_png(tray::TRAY_OK)
        .ok()
        .map(|(rgba, width, height)| egui::IconData {
            width,
            height,
            rgba,
        });

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([560.0, 620.0])
        .with_min_inner_size([460.0, 480.0])
        .with_visible(!silent);
    if let Some(data) = icon {
        viewport = viewport.with_icon(std::sync::Arc::new(data));
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "TunCat",
        options,
        Box::new(move |cc| {
            cc.egui_ctx
                .set_visuals(app::TunCatApp::theme_visuals(config.theme));
            Ok(Box::new(app::TunCatApp::new(core, tray, config, elevated)))
        }),
    )
}
