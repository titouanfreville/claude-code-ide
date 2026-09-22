//! Cross-cutting concerns for MoonlightCode: logging/tracing, config, secrets.
//!
//! Mirrors the user's Go `core/` conventions (injected structured logging, config
//! aggregation). HUD telemetry derives from `tracing`, not a parallel path.

pub mod obs;
pub mod support;

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initialize structured logging. Reads `RUST_LOG` (defaults to `info`).
/// Safe to call once at startup from the composition root.
///
/// Logs go to stdout **and** to a file named after this binary in the support
/// directory. The file matters because MoonlightCode's processes are not always
/// started by a human at a terminal: an IDE autostarts `moonlightd`, and stdout is
/// then whatever the editor was given — usually nothing. A daemon whose only record
/// of what it decided went to a closed pipe cannot be diagnosed at all.
pub fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false));

    match open_log_file() {
        Some(file) => registry
            .with(
                fmt::layer()
                    .with_writer(file)
                    .with_ansi(false)
                    .with_target(true),
            )
            .init(),
        None => registry.init(),
    }
}

/// `<support dir>/<this binary>.log`, appended.
///
/// Named after the executable so two MoonlightCode processes sharing a state scope
/// do not interleave their logs, and `MOONLIGHT_HOME` moves the file with the rest
/// of the state — a sandbox run does not write into the real log.
///
/// Returns `None` rather than failing: a process that cannot open its log should
/// still run.
fn open_log_file() -> Option<std::fs::File> {
    let name = std::env::current_exe()
        .ok()?
        .file_stem()?
        .to_string_lossy()
        .into_owned();
    let path = support::support_path(&format!("{name}.log"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()
}
