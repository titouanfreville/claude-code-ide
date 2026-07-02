//! A thin, dependency-free read/act surface over a project's git state — just what
//! the [`toolbar`](super::panels::toolbar) needs: the **current branch** (for the
//! branch selector chip) and the **local branch list** + **checkout** (for its
//! dropdown). JetBrains' VCS widget, in miniature.
//!
//! The current branch is read on every chrome render, so it stays *cheap*: a single
//! read of `<root>/.git/HEAD` parsed by hand — no subprocess. The branch *list* and
//! *checkout* only run on a dropdown interaction, so those shell out to `git` (the
//! correct source of truth for packed refs, worktrees, etc.) off the UI thread.

use std::path::Path;
use std::process::Command;

/// The current branch name (e.g. `main`), or `None` when `root` is not a git repo.
/// A detached HEAD returns the short commit id prefixed with `@` so the chip still
/// shows *something* meaningful ("@a1b2c3d"). Reads `.git/HEAD` directly — no fork.
pub fn current_branch(root: &Path) -> Option<String> {
    let head = std::fs::read_to_string(root.join(".git").join("HEAD")).ok()?;
    let head = head.trim();
    // The common case: `ref: refs/heads/<branch>`.
    if let Some(rest) = head.strip_prefix("ref: ") {
        return Some(rest.rsplit('/').next().unwrap_or(rest).to_string());
    }
    // Detached HEAD: the file holds a raw 40-char sha. Show a short, marked form.
    if head.len() >= 7 && head.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(format!("@{}", &head[..7]));
    }
    None
}

/// The local branches (`git branch`), newest-checkout order as git reports it. Empty
/// when `root` isn't a repo or git isn't available. Only called when the dropdown
/// opens, so the subprocess cost is paid on interaction, not per frame.
pub fn local_branches(root: &Path) -> Vec<String> {
    let Ok(out) = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["branch", "--format=%(refname:short)"])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Check out `branch` in `root`. Returns the git error text on failure (a dirty tree,
/// an unknown branch) so the caller can surface it. Blocking — run off the UI thread.
pub fn checkout(root: &Path, branch: &str) -> Result<(), String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["checkout", branch])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_branch_parses_symbolic_head() {
        let dir = std::env::temp_dir().join(format!("mlc-git-{}", std::process::id()));
        let git = dir.join(".git");
        std::fs::create_dir_all(&git).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/feature/top-bar\n").unwrap();
        assert_eq!(current_branch(&dir).as_deref(), Some("top-bar"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn current_branch_handles_detached_head() {
        let dir = std::env::temp_dir().join(format!("mlc-git-det-{}", std::process::id()));
        let git = dir.join(".git");
        std::fs::create_dir_all(&git).unwrap();
        std::fs::write(
            git.join("HEAD"),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .unwrap();
        assert_eq!(current_branch(&dir).as_deref(), Some("@0123456"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn current_branch_none_without_repo() {
        let dir = std::env::temp_dir().join(format!("mlc-git-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(current_branch(&dir), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
