//! What the control server needs to know about a workspace to attribute a write it
//! didn't see coming.
//!
//! `Edit`/`Write` name their target, so the server reads the pre-image itself. A
//! `Bash` command names nothing: the only way to learn what it wrote is to compare
//! the workspace before and after, and the only available "before" for a file
//! nobody read is VCS. Both of those are git operations, and git plumbing lives in
//! the desktop app — so this is a port the composition root fills, in the same
//! spirit as [`crate::server::IdResolver`].

use std::sync::Arc;

/// Read-only view of a workspace's version control, used to find and explain
/// inferred writes. Every method returns "nothing" rather than an error: capture is
/// an observation, and a repo-less workspace simply yields no attribution.
pub trait WorkspaceProbe: Send + Sync {
    /// Absolute paths under the repo containing `cwd` that differ from `HEAD`,
    /// including untracked files. Empty when `cwd` is not a repo (or git is absent).
    fn dirty_paths(&self, cwd: &str) -> Vec<String>;

    /// The content of `path` as of `HEAD`, or `None` when `HEAD` has no such file
    /// (it is new) or it isn't text.
    fn head_blob(&self, cwd: &str, path: &str) -> Option<String>;
}

/// A probe that reports nothing — the default when no workspace plumbing is wired
/// (tests, the degraded standalone server). Shell writes then go unattributed,
/// exactly as they did before the probe existed.
pub struct NoProbe;

impl WorkspaceProbe for NoProbe {
    fn dirty_paths(&self, _cwd: &str) -> Vec<String> {
        Vec::new()
    }
    fn head_blob(&self, _cwd: &str, _path: &str) -> Option<String> {
        None
    }
}

/// Shared handle to the workspace probe.
pub type SharedProbe = Arc<dyn WorkspaceProbe>;
