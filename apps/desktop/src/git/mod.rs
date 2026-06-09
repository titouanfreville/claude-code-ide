//! Git integration for the IDE-classic shell.
//!
//! Read-only VCS *status* for file-tree decoration (what an agent changed — the
//! core supervision question). Shells out to the system `git` rather than linking
//! libgit2: no extra dependency, and `git status` is fast on normal repos. All
//! invocations run off the UI thread (callers use `cx.background_executor`).

pub mod commit;
pub mod diff;
pub mod log;
pub mod status;
