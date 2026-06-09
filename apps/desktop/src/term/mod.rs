//! Terminal emulation for the IDE-classic manual terminal panel.
//!
//! This is the operator's own shell (run `git`, `cargo`, …) — **not** a Claude
//! Code session. It is built directly on the public `alacritty_terminal` API
//! (PTY + VTE parser + grid state); none of Zed's GPL terminal code is used.
//!
//! [`emulator`] owns the PTY and parser off the UI thread; the terminal *view*
//! ([`crate::views::panels::terminal`]) renders the grid and forwards key input.

pub mod emulator;
