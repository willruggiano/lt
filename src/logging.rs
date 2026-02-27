//! Structured logging setup using `tracing` + `tracing-subscriber` + `tracing-appender`.
//!
//! Two modes are supported:
//!
//! - TUI mode  -- all log output goes to the rotating file log only.
//!   Nothing is printed to stdout/stderr so the TUI is not corrupted.
//!
//! - CLI mode  -- INFO-level messages are also written to stdout so the user can
//!   see progress.  Everything (DEBUG and above) goes to the file log.
//!
//! The log directory is `~/.local/share/lt/log/`.
//! Log files are rotated daily by `tracing-appender`.
//!
//! The caller must keep the `WorkerGuard` returned by each init function alive
//! for the duration of the program.  Dropping the guard flushes and closes the
//! background logging thread.
//!
//! ## Log level policy
//!
//! External library log events at DEBUG are noisy (e.g. ureq logs raw HTTP
//! headers with CRLF sequences that produce `^M` in log files).  The default
//! filter therefore sets DEBUG for the `lt` crate and WARN for everything else.
//! Pass `RUST_LOG` to override.

use anyhow::{Context, Result};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::{EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

// -- CR-stripping writer -----------------------------------------------------

/// A `MakeWriter` wrapper whose produced writers strip `\r` bytes before
/// forwarding output to the inner writer.  This prevents `^M` artifacts caused
/// by libraries (e.g. ureq) that log raw HTTP/1.1 headers containing CRLF.
struct StripCrMakeWriter(NonBlocking);

impl<'a> MakeWriter<'a> for StripCrMakeWriter {
    type Writer = StripCrWriter<<NonBlocking as MakeWriter<'a>>::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        StripCrWriter(self.0.make_writer())
    }
}

struct StripCrWriter<W: Write>(W);

impl<W: Write> Write for StripCrWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let stripped: Vec<u8> = buf.iter().copied().filter(|&b| b != b'\r').collect();
        self.0.write_all(&stripped)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

// -- helpers -----------------------------------------------------------------

/// Returns `~/.local/share/lt/log`, creating the directory if necessary.
fn log_dir() -> Result<PathBuf> {
    let dir = dirs::data_local_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine local data directory"))?
        .join("lt")
        .join("log");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating log directory {}", dir.display()))?;
    Ok(dir)
}

/// Removes log files from `dir` whose modification time is older than `days` days.
///
/// All errors are silently ignored -- this is a best-effort cleanup step.
fn prune_old_logs(dir: &PathBuf, days: u64) {
    let threshold = match SystemTime::now().checked_sub(Duration::from_secs(days * 24 * 60 * 60)) {
        Some(t) => t,
        None => return,
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let mtime = match entry.metadata().and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => continue,
        };
        if mtime < threshold {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Build the default `EnvFilter` for file logging.
///
/// External libraries are set to WARN so their verbose DEBUG output (e.g.
/// ureq HTTP prelude logs) does not clutter the log file.  The `lt` crate
/// itself is set to DEBUG.  `RUST_LOG` overrides everything.
fn file_env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn,lt=debug"))
}

// -- public init functions ---------------------------------------------------

/// Initialise logging for **TUI mode**.
///
/// All output goes to the rotating file log; nothing is written to stdout or
/// stderr so the terminal UI is not corrupted.
///
/// Returns a `WorkerGuard` that must be kept alive for the duration of the
/// program to ensure all buffered log records are flushed.
pub fn init_tui() -> Result<WorkerGuard> {
    let dir = log_dir()?;
    prune_old_logs(&dir, 7);
    let file_appender = tracing_appender::rolling::daily(&dir, "lt.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let file_layer = fmt::layer()
        .with_ansi(false)
        .with_writer(StripCrMakeWriter(non_blocking))
        .with_filter(file_env_filter());

    tracing_subscriber::registry().with(file_layer).init();

    Ok(guard)
}

/// Initialise logging for **CLI mode**.
///
/// INFO-level (and above) messages are printed to stdout so the user sees
/// progress feedback.  All messages (DEBUG and above for `lt`, WARN for
/// libraries) are also written to the rotating file log.
///
/// Returns a `WorkerGuard` that must be kept alive for the duration of the
/// program to ensure all buffered log records are flushed.
pub fn init_cli() -> Result<WorkerGuard> {
    let dir = log_dir()?;
    prune_old_logs(&dir, 7);
    let file_appender = tracing_appender::rolling::daily(&dir, "lt.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let file_layer = fmt::layer()
        .with_ansi(false)
        .with_writer(StripCrMakeWriter(non_blocking))
        .with_filter(file_env_filter());

    // Stdout layer: INFO and above (unless overridden via RUST_LOG).
    let stdout_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn,lt=info"));

    let stdout_layer = fmt::layer()
        .with_ansi(false)
        .with_writer(std::io::stdout)
        .with_filter(stdout_filter);

    tracing_subscriber::registry()
        .with(file_layer)
        .with(stdout_layer)
        .init();

    Ok(guard)
}
