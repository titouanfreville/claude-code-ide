//! The GPUI view layer. Views consume engine read-models and emit `Command`s —
//! they never mutate engine state directly (architecture: event bus is the only
//! engine→UI channel). All styling reads tokens from [`theme`].
//!
//! The shell is a JetBrains-style dockable workspace ([`workspace`]) built on
//! `gpui-component`'s `DockArea`. Each dockable surface is a "plugin" — a struct
//! implementing `gpui_component::dock::Panel`: the center [`grid_home`] fleet view
//! and the edge-rail [`panels`] (file tree, terminal). [`project_space`] is the
//! shared "active project" (open folder / recent / follow-focus) the rails follow.

pub mod active_context;
pub mod active_editor;
pub mod approvals;
pub mod auto_compact;
pub mod center_requests;
pub mod chrome_requests;
pub mod edit_gate;
pub mod editor_commands;
pub mod git_console;
pub mod git_info;
pub mod grid_home;
pub mod mcp_host;
pub mod notifications;
pub mod obs_store;
pub mod open_tabs;
pub mod panels;
pub mod project_space;
pub mod restore_guard;
pub mod run_config;
pub mod session_io;
pub mod session_meta;
pub mod session_tile;
pub mod space_sessions;
pub mod theme;
pub mod workspace;
