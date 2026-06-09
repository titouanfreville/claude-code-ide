//! Git working-tree status for file-tree decoration.
//!
//! Shells out to `git status --porcelain=v1 -z` and exposes a per-path status map
//! plus a set of directories that *contain* changes (for folder dots). Pure parsing
//! is split out as [`parse_porcelain`] so it is unit-testable without a repo.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Working-tree state of a single path (the subset we decorate). Derived from the
/// porcelain XY code with a fixed precedence (conflict > rename > add > delete >
/// modify > untracked).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitFileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

impl GitFileStatus {
    /// Single-letter badge shown after the filename (JetBrains-style).
    pub fn letter(self) -> &'static str {
        match self {
            GitFileStatus::Modified => "M",
            GitFileStatus::Added => "A",
            GitFileStatus::Deleted => "D",
            GitFileStatus::Renamed => "R",
            GitFileStatus::Untracked => "?",
            GitFileStatus::Conflicted => "U",
        }
    }
}

/// Map a porcelain v1 two-char `XY` code to a [`GitFileStatus`].
fn classify(xy: &str) -> GitFileStatus {
    let mut chars = xy.chars();
    let x = chars.next().unwrap_or(' ');
    let y = chars.next().unwrap_or(' ');

    if xy == "??" {
        return GitFileStatus::Untracked;
    }
    // Unmerged / conflict states (e.g. UU, AA, DD, U?, ?U).
    if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
        return GitFileStatus::Conflicted;
    }
    if x == 'R' || x == 'C' {
        return GitFileStatus::Renamed;
    }
    if x == 'A' {
        return GitFileStatus::Added;
    }
    if x == 'D' || y == 'D' {
        return GitFileStatus::Deleted;
    }
    GitFileStatus::Modified
}

/// Parse `git status --porcelain=v1 -z` output into a map of repo-relative path →
/// status. NUL-terminated; rename/copy entries carry an extra source-path token
/// (the new path is the one we record).
pub fn parse_porcelain(output: &str) -> HashMap<PathBuf, GitFileStatus> {
    let mut map = HashMap::new();
    let mut tokens = output.split('\0').filter(|t| !t.is_empty());
    while let Some(entry) = tokens.next() {
        // Entry layout: "XY <path>" — codes in 0..2, space at 2, path from 3.
        if entry.len() < 4 {
            continue;
        }
        let xy = &entry[0..2];
        let path = &entry[3..];
        let status = classify(xy);
        // Rename/copy: the source path follows as its own token — consume it.
        if matches!(status, GitFileStatus::Renamed) {
            let _ = tokens.next();
        }
        map.insert(PathBuf::from(path), status);
    }
    map
}

/// A repository's working-tree status, keyed by absolute path, plus the set of
/// directories that contain at least one changed descendant.
pub struct RepoStatus {
    files: HashMap<PathBuf, GitFileStatus>,
    dirs_with_changes: HashSet<PathBuf>,
}

impl RepoStatus {
    /// Build from a repo toplevel and a relative-path status map (joins to absolute
    /// keys and computes the directory-aggregate set up to `toplevel`).
    fn new(toplevel: &Path, relative: HashMap<PathBuf, GitFileStatus>) -> Self {
        let mut files = HashMap::with_capacity(relative.len());
        let mut dirs_with_changes = HashSet::new();
        for (rel, status) in relative {
            let abs = toplevel.join(&rel);
            // Mark every ancestor up to (and including) the toplevel.
            let mut cur = abs.parent();
            while let Some(dir) = cur {
                dirs_with_changes.insert(dir.to_path_buf());
                if dir == toplevel {
                    break;
                }
                cur = dir.parent();
            }
            files.insert(abs, status);
        }
        Self {
            files,
            dirs_with_changes,
        }
    }

    /// Status of an exact file path, if changed.
    pub fn status_for(&self, path: &Path) -> Option<GitFileStatus> {
        self.files.get(path).copied()
    }

    /// Whether a directory contains any changed descendant.
    pub fn dir_has_changes(&self, path: &Path) -> bool {
        self.dirs_with_changes.contains(path)
    }
}

/// Load the working-tree status for the repo containing `root`. Returns `None` when
/// `root` is not inside a git repo or `git` is unavailable — callers then render an
/// undecorated tree. Blocking: run on a background executor.
pub fn load_status(root: &Path) -> Option<RepoStatus> {
    let toplevel_out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !toplevel_out.status.success() {
        return None;
    }
    let toplevel = PathBuf::from(
        String::from_utf8_lossy(&toplevel_out.stdout)
            .trim()
            .to_owned(),
    );
    if toplevel.as_os_str().is_empty() {
        return None;
    }

    let status_out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .output()
        .ok()?;
    if !status_out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&status_out.stdout);
    Some(RepoStatus::new(&toplevel, parse_porcelain(&text)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_covers_common_codes() {
        assert_eq!(classify("??"), GitFileStatus::Untracked);
        assert_eq!(classify(" M"), GitFileStatus::Modified);
        assert_eq!(classify("M "), GitFileStatus::Modified);
        assert_eq!(classify("A "), GitFileStatus::Added);
        assert_eq!(classify(" D"), GitFileStatus::Deleted);
        assert_eq!(classify("R "), GitFileStatus::Renamed);
        assert_eq!(classify("UU"), GitFileStatus::Conflicted);
        assert_eq!(classify("AA"), GitFileStatus::Conflicted);
    }

    #[test]
    fn parse_porcelain_reads_entries_and_consumes_rename_source() {
        // " M file", "?? new", "A  added", rename "R  to" + source "from".
        let out = " M src/main.rs\0?? notes.txt\0A  added.rs\0R  renamed_to.rs\0renamed_from.rs\0";
        let map = parse_porcelain(out);

        assert_eq!(
            map.get(Path::new("src/main.rs")),
            Some(&GitFileStatus::Modified)
        );
        assert_eq!(
            map.get(Path::new("notes.txt")),
            Some(&GitFileStatus::Untracked)
        );
        assert_eq!(map.get(Path::new("added.rs")), Some(&GitFileStatus::Added));
        assert_eq!(
            map.get(Path::new("renamed_to.rs")),
            Some(&GitFileStatus::Renamed)
        );
        // The source token was consumed, not recorded as its own entry.
        assert_eq!(map.get(Path::new("renamed_from.rs")), None);
        assert_eq!(map.len(), 4);
    }

    #[test]
    fn repo_status_marks_ancestor_dirs() {
        let top = Path::new("/repo");
        let mut rel = HashMap::new();
        rel.insert(PathBuf::from("a/b/c.rs"), GitFileStatus::Modified);
        let status = RepoStatus::new(top, rel);

        assert_eq!(
            status.status_for(Path::new("/repo/a/b/c.rs")),
            Some(GitFileStatus::Modified)
        );
        assert!(status.dir_has_changes(Path::new("/repo/a")));
        assert!(status.dir_has_changes(Path::new("/repo/a/b")));
        assert!(status.dir_has_changes(Path::new("/repo")));
        assert!(!status.dir_has_changes(Path::new("/repo/x")));
        assert_eq!(status.status_for(Path::new("/repo/a")), None);
    }
}
