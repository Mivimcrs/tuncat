//! tuncat-core: detection, ICS pulse repair and background worker,
//! independent of any UI.

pub mod autostart;
pub mod config;
pub mod detector;
pub mod ics;
pub mod state;

/// Initialize file logging under `%APPDATA%\TunCat\logs\`, daily rolling,
/// 7 days retained. Returns the tracing guard that must be kept alive.
pub fn init_logging() -> anyhow::Result<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let dir = config::Config::config_dir()?.join("logs");
    std::fs::create_dir_all(&dir)?;
    let appender = tracing_appender::rolling::daily(&dir, "tuncat.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(false)
                .with_target(false),
        )
        .with(tracing_subscriber::EnvFilter::try_new("info").unwrap())
        .init();
    Ok(guard)
}

/// Best-effort cleanup of log files older than 7 days.
pub fn prune_old_logs() {
    let Ok(dir) = config::Config::config_dir().map(|d| d.join("logs")) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let cutoff = std::time::SystemTime::now()
        - std::time::Duration::from_secs(7 * 24 * 3600);
    for e in entries.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_file()
            && meta.modified().map(|m| m < cutoff).unwrap_or(false)
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
}
