//! The git-backed [`WorkspaceProbe`] the control server uses to attribute shell
//! writes.
//!
//! The server can't reach this crate's `git` module (it lives *below* the app), so
//! the composition root injects this adapter — the same shape as the id resolver it
//! already passes down. Everything here shells out through [`crate::git`], off no
//! particular thread: it runs on the control server's own runtime while Claude Code
//! waits on the hook, so it stays to two `git` calls per mutating shell command.

use std::path::{Path, PathBuf};

use moonlight_control::WorkspaceProbe;

use crate::git::diff::{changed_files, head_blob, toplevel};

/// Answers "what is dirty here?" and "what did this file hold at HEAD?" from the
/// system `git`.
pub struct GitProbe;

impl WorkspaceProbe for GitProbe {
    fn dirty_paths(&self, cwd: &str) -> Vec<String> {
        let cwd = Path::new(cwd);
        // Paths are reported relative to the repo root, not to `cwd`, so resolve
        // the toplevel to make them absolute.
        let Some(root) = toplevel(cwd) else {
            return Vec::new();
        };
        changed_files(cwd)
            .unwrap_or_default()
            .into_iter()
            .map(|file| root.join(&file.rel_path).to_string_lossy().into_owned())
            .collect()
    }

    fn head_blob(&self, cwd: &str, path: &str) -> Option<String> {
        let cwd = Path::new(cwd);
        let root = toplevel(cwd)?;
        // `git show HEAD:<path>` wants a repo-relative path.
        let rel = PathBuf::from(path)
            .strip_prefix(&root)
            .ok()?
            .to_string_lossy()
            .into_owned();
        head_blob(&root, &rel)
    }
}
