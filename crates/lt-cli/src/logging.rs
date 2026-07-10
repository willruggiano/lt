//! Structured logging setup using `tracing` + `tracing-subscriber` + `tracing-appender`.
//!
//! The log directory is `$XDG_STATE_DIR/lt/logs/`.
//! Log files are rotated daily.

use std::time::{Duration, SystemTime};

use anyhow::Result;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

fn prune_old_logs(dir: &std::path::Path, days: u64) {
    let Some(threshold) = SystemTime::now().checked_sub(Duration::from_hours(days * 24)) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if mtime < threshold
            && let Err(e) = std::fs::remove_file(&path)
        {
            tracing::warn!(error = %e, path = %path.display(), "failed to remove old log file");
        }
    }
}

fn file_env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn,lt=debug"))
}

#[must_use = "dropping the guard flushes and closes the background logging thread"]
pub fn init(stdout: bool) -> Result<WorkerGuard> {
    let dir = lt_config::log_dir()?;
    prune_old_logs(&dir, 7);
    let file_appender = tracing_appender::rolling::daily(&dir, "lt.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = file_env_filter();
    let file_layer = fmt::layer()
        .with_ansi(false)
        .with_writer(non_blocking)
        .with_filter(filter.clone());

    if stdout {
        let stdout_layer = fmt::layer()
            .with_ansi(false)
            .with_writer(std::io::stdout)
            .with_filter(filter);

        tracing_subscriber::registry()
            .with(file_layer)
            .with(stdout_layer)
            .init();
    } else {
        tracing_subscriber::registry().with(file_layer).init();
    }

    Ok(guard)
}
