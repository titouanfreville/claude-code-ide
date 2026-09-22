//! Where MoonlightCode keeps its own state on disk.
//!
//! The actual resolution logic lives in `moonlight-core` (see
//! [`moonlight_core::support`]) so the headless control-API daemon resolves the
//! **same** path as this desktop app — two processes computing this independently
//! would risk opening divergent SQLite files. This module just re-exports it under
//! the name every call site in this crate already uses.

pub(crate) use moonlight_core::support::{support_dir, support_path};
