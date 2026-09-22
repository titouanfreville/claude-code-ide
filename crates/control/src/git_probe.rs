//! A minimal, self-contained git shell-out — just enough for [`GitProbe`]
//! (`dirty_paths`/`head_blob`), not the richer status classification and diff
//! rendering `apps/desktop`'s own `git` module does for its UI (file badges,
//! per-hunk staging, commit history). Kept deliberately independent rather than
//! sharing that module: this crate must stay usable by a headless daemon with no
//! UI, and a review-attribution probe has no business depending on UI rendering
//! concerns. The porcelain record layout parsed here is the same one that
//! module's own parser reads (`XY <path>`, NUL-terminated, rename/copy carrying
//! an extra source-path token) — this just doesn't classify `XY` into a status
//! enum, since a probe only needs *which* paths changed, not *how*.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::WorkspaceProbe;

/// The repo toplevel that contains `root`, or `None` if `root` isn't in a git repo
/// (or `git` is unavailable).
fn toplevel(root: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let top = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!top.is_empty()).then(|| PathBuf::from(top))
}

/// Repo-relative paths of every working-tree change (tracked + untracked), in
/// whatever order `git` reports them. `None` only when `root` isn't a git repo or
/// `git` is unavailable — an empty `Vec` means a clean tree.
fn changed_relative_paths(root: &Path) -> Option<Vec<PathBuf>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_porcelain_paths(&String::from_utf8_lossy(&out.stdout)))
}

/// Parse `git status --porcelain=v1 -z` output into just the changed paths.
fn parse_porcelain_paths(output: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut tokens = output.split('\0').filter(|t| !t.is_empty());
    while let Some(entry) = tokens.next() {
        // Entry layout: "XY <path>" — codes in 0..2, space at 2, path from 3.
        if entry.len() < 4 {
            continue;
        }
        let xy = &entry[0..2];
        let path = &entry[3..];
        // Rename/copy: the source path follows as its own token — consume it,
        // only the current path is reported.
        if xy.starts_with('R') || xy.starts_with('C') {
            let _ = tokens.next();
        }
        paths.push(PathBuf::from(path));
    }
    paths
}

/// The content of `rel_path` as of `HEAD`, or `None` when `HEAD` has no such file
/// (new to this commit) or the blob isn't UTF-8 text.
fn head_blob(root: &Path, rel_path: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("show")
        .arg(format!("HEAD:{rel_path}"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// Answers "what is dirty here?" and "what did this file hold at HEAD?" from the
/// system `git` — the [`WorkspaceProbe`] that makes a shell command's writes
/// attributable (a `sed -i`, a redirect, a generated file: nothing that declared
/// its own path to a tool, so the ledger can't attribute it any other way).
pub struct GitProbe;

impl WorkspaceProbe for GitProbe {
    fn dirty_paths(&self, cwd: &str) -> Vec<String> {
        let cwd = Path::new(cwd);
        // Paths are reported relative to the repo root, not to `cwd`, so resolve
        // the toplevel to make them absolute.
        let Some(root) = toplevel(cwd) else {
            return Vec::new();
        };
        changed_relative_paths(&root)
            .unwrap_or_default()
            .into_iter()
            .map(|rel| root.join(&rel).to_string_lossy().into_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_add_modify_delete_and_untracked() {
        let raw = "M  src/a.rs\0A  src/b.rs\0 D src/c.rs\0?? new.txt\0";
        let paths = parse_porcelain_paths(raw);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("src/a.rs"),
                PathBuf::from("src/b.rs"),
                PathBuf::from("src/c.rs"),
                PathBuf::from("new.txt"),
            ]
        );
    }

    #[test]
    fn a_rename_reports_only_the_new_path_and_skips_the_source_token() {
        let raw = "R  new_name.rs\0old_name.rs\0M  other.rs\0";
        let paths = parse_porcelain_paths(raw);
        assert_eq!(
            paths,
            vec![PathBuf::from("new_name.rs"), PathBuf::from("other.rs")]
        );
    }

    #[test]
    fn empty_output_is_no_changes() {
        assert!(parse_porcelain_paths("").is_empty());
    }

    /// A tiny throwaway repo, so these tests exercise the real `git` shell-outs
    /// rather than just the pure parser. Holds the **canonicalized** path — macOS's
    /// `/tmp` (and `TMPDIR`) are themselves symlinks, and `git rev-parse
    /// --show-toplevel` resolves them, so comparing against the un-resolved path
    /// `std::env::temp_dir()` hands back would fail for a reason that has nothing
    /// to do with `GitProbe` itself.
    struct TempRepo(PathBuf);
    impl TempRepo {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "moonlight-control-git-probe-test-{}-{tag}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let dir = dir.canonicalize().unwrap_or(dir);
            let run = |args: &[&str]| {
                assert!(Command::new("git")
                    .arg("-C")
                    .arg(&dir)
                    .args(args)
                    .status()
                    .expect("git available")
                    .success());
            };
            run(&["init", "-q"]);
            run(&["config", "user.email", "test@example.com"]);
            run(&["config", "user.name", "test"]);
            std::fs::write(dir.join("committed.txt"), "line one\n").unwrap();
            run(&["add", "."]);
            run(&["commit", "-q", "-m", "initial"]);
            Self(dir)
        }
    }
    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn dirty_paths_reports_a_shell_edited_and_a_new_file() {
        let repo = TempRepo::new("dirty");
        std::fs::write(repo.0.join("committed.txt"), "line one\nline two\n").unwrap();
        std::fs::write(repo.0.join("untracked.txt"), "brand new\n").unwrap();

        let probe = GitProbe;
        let mut dirty = probe.dirty_paths(repo.0.to_str().unwrap());
        dirty.sort();

        let mut expected = vec![
            repo.0.join("committed.txt").to_string_lossy().into_owned(),
            repo.0.join("untracked.txt").to_string_lossy().into_owned(),
        ];
        expected.sort();
        assert_eq!(dirty, expected);
    }

    #[test]
    fn head_blob_returns_the_committed_content_not_the_working_tree_edit() {
        let repo = TempRepo::new("head-blob");
        std::fs::write(repo.0.join("committed.txt"), "changed on disk\n").unwrap();

        let probe = GitProbe;
        let abs = repo.0.join("committed.txt").to_string_lossy().into_owned();
        let blob = probe.head_blob(repo.0.to_str().unwrap(), &abs);
        assert_eq!(blob.as_deref(), Some("line one\n"));
    }

    #[test]
    fn head_blob_is_none_for_a_file_not_in_head() {
        let repo = TempRepo::new("no-head-blob");
        std::fs::write(repo.0.join("untracked.txt"), "new\n").unwrap();

        let probe = GitProbe;
        let abs = repo.0.join("untracked.txt").to_string_lossy().into_owned();
        assert_eq!(probe.head_blob(repo.0.to_str().unwrap(), &abs), None);
    }

    #[test]
    fn dirty_paths_is_empty_outside_any_git_repo() {
        let dir = std::env::temp_dir().join(format!(
            "moonlight-control-git-probe-test-not-a-repo-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let probe = GitProbe;
        assert!(probe.dirty_paths(dir.to_str().unwrap()).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
