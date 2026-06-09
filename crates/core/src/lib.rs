//! Cross-cutting concerns for MoonlightCode: logging/tracing, config, secrets.
//!
//! Mirrors the user's Go `core/` conventions (injected structured logging, config
//! aggregation). HUD telemetry derives from `tracing`, not a parallel path.

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initialize structured logging. Reads `RUST_LOG` (defaults to `info`).
/// Safe to call once at startup from the composition root.
pub fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false))
        .init();
}
