//! Index-level git operations for the **Commit tool window**: the staged ⇄
//! unstaged split (porcelain XY, finer than [`status`](super::status)'s collapsed
//! per-file state), stage/unstage, and the commit itself.
//!
//! The split is index-faithful on purpose: the review tab's per-hunk **Accept
//! stages hunks** (`git apply --cached`, see [`diff::restage_file`]), so whatever
//! the operator accepted shows up here under *Staged* and `commit` commits exactly
//! that. Pure parsing is split out for tests; the shell ops are thin `git -C`
//! wrappers (blocking — run off the UI thread or accept the ~ms pause like the
//! toolbar's checkout does).

use std::path::{Path, PathBuf};
use std::process::Command;

use super::status::GitFileStatus;

/// One changed path in the commit tool, split by where the change sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitEntry {
    /// Repo-relative path (the porcelain path).
    pub rel: PathBuf,
    /// Index-side state (staged), when X marks a staged change.
    pub staged: Option<GitFileStatus>,
    /// Worktree-side state (unstaged / untracked), when Y marks one.
    pub unstaged: Option<GitFileStatus>,
}

/// Map one porcelain X or Y code char to a status (for that side only).
/// `' '` = no change on that side. `?` is handled by the caller (untracked).
fn side_status(c: char) -> Option<GitFileStatus> {
    match c {
        ' ' | '?' | '!' => None,
        'M' | 'T' => Some(GitFileStatus::Modified),
        'A' | 'C' => Some(GitFileStatus::Added),
        'D' => Some(GitFileStatus::Deleted),
        'R' => Some(GitFileStatus::Renamed),
        'U' => Some(GitFileStatus::Conflicted),
        _ => Some(GitFileStatus::Modified),
    }
}

/// Parse `git status --porcelain=v1 -z` keeping the X (index) / Y (worktree)
/// split. Untracked (`??`) lands as `unstaged: Untracked`. Rename entries consume
/// their source token like [`status::parse_porcelain`].
pub fn parse_entries(output: &str) -> Vec<CommitEntry> {
    let mut entries = Vec::new();
    let mut tokens = output.split('\0').filter(|t| !t.is_empty());
    while let Some(entry) = tokens.next() {
        if entry.len() < 4 {
            continue;
        }
        let mut xy = entry[0..2].chars();
        let (x, y) = (xy.next().unwrap_or(' '), xy.next().unwrap_or(' '));
        let rel = PathBuf::from(&entry[3..]);
        if x == 'R' || x == 'C' {
            let _ = tokens.next(); // rename/copy source path
        }
        let (staged, unstaged) = if x == '?' && y == '?' {
            (None, Some(GitFileStatus::Untracked))
        } else {
            (side_status(x), side_status(y))
        };
        if staged.is_some() || unstaged.is_some() {
            entries.push(CommitEntry {
                rel,
                staged,
                unstaged,
            });
        }
    }
    entries.sort_by(|a, b| a.rel.cmp(&b.rel));
    entries
}

/// Load the commit-tool entries for the repo containing `root`. `None` when not
/// a repo / git unavailable.
pub fn load_entries(root: &Path) -> Option<Vec<CommitEntry>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_entries(&String::from_utf8_lossy(&out.stdout)))
}

/// Run a git subcommand in `root`, mapping failure to its stderr text.
fn run_git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Stage one path (whole file — per-hunk staging lives in the review tab).
pub fn stage(root: &Path, rel: &Path) -> Result<(), String> {
    run_git(root, &["add", "--", &rel.to_string_lossy()]).map(|_| ())
}

/// Unstage one path (keep its worktree content). On an **unborn branch** (no
/// commit yet) there is no HEAD to restore the index from — the initial-add
/// case unstages via `rm --cached` instead (worktree file kept).
pub fn unstage(root: &Path, rel: &Path) -> Result<(), String> {
    match run_git(root, &["restore", "--staged", "--", &rel.to_string_lossy()]) {
        Ok(_) => Ok(()),
        Err(e) if e.contains("HEAD") => {
            run_git(root, &["rm", "--cached", "-q", "--", &rel.to_string_lossy()]).map(|_| ())
        }
        Err(e) => Err(e),
    }
}

/// Commit the index with `message`. Returns git's summary line ("1 file
/// changed, …") on success.
pub fn commit(root: &Path, message: &str) -> Result<String, String> {
    run_git(root, &["commit", "-m", message])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_entries_splits_index_and_worktree_sides() {
        // "MM" = staged-modified AND worktree-modified; "M " staged only;
        // " M" worktree only; "??" untracked; "A " staged add;
        // rename "R  to" consumes its "from" token.
        let out = "MM both.rs\0M  staged.rs\0 M wt.rs\0?? new.txt\0A  added.rs\0R  to.rs\0from.rs\0";
        let e = parse_entries(out);
        let find = |p: &str| e.iter().find(|c| c.rel == Path::new(p)).unwrap();

        let both = find("both.rs");
        assert_eq!(both.staged, Some(GitFileStatus::Modified));
        assert_eq!(both.unstaged, Some(GitFileStatus::Modified));

        assert_eq!(find("staged.rs").staged, Some(GitFileStatus::Modified));
        assert_eq!(find("staged.rs").unstaged, None);
        assert_eq!(find("wt.rs").staged, None);
        assert_eq!(find("wt.rs").unstaged, Some(GitFileStatus::Modified));
        assert_eq!(find("new.txt").unstaged, Some(GitFileStatus::Untracked));
        assert_eq!(find("added.rs").staged, Some(GitFileStatus::Added));
        assert_eq!(find("to.rs").staged, Some(GitFileStatus::Renamed));
        // Source token consumed.
        assert!(e.iter().all(|c| c.rel != Path::new("from.rs")));
        // Sorted by path.
        let paths: Vec<_> = e.iter().map(|c| c.rel.to_string_lossy().into_owned()).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted);
    }

    #[test]
    fn stage_commit_unstage_round_trip_in_a_temp_repo() {
        let dir = std::env::temp_dir().join(format!("ml-commit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| run_git(&dir, args).expect("git op");
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t.t"]);
        git(&["config", "user.name", "t"]);

        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        let entries = load_entries(&dir).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].unstaged, Some(GitFileStatus::Untracked));

        stage(&dir, Path::new("a.txt")).unwrap();
        let entries = load_entries(&dir).unwrap();
        assert_eq!(entries[0].staged, Some(GitFileStatus::Added));
        assert_eq!(entries[0].unstaged, None);

        unstage(&dir, Path::new("a.txt")).unwrap();
        assert_eq!(
            load_entries(&dir).unwrap()[0].unstaged,
            Some(GitFileStatus::Untracked)
        );

        stage(&dir, Path::new("a.txt")).unwrap();
        let summary = commit(&dir, "add a").unwrap();
        assert!(summary.contains("add a") || summary.contains("1 file"), "{summary}");
        assert!(load_entries(&dir).unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
