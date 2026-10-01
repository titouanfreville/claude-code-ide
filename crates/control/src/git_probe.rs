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

/// The candidates git is asked about for one repo-relative path: every ancestor
/// directory (trailing `/`, which `check-ignore` reads as a directory even once it no
/// longer exists), shallowest first, then the path itself.
fn ignore_candidates(rel: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = rel[at..].find('/') {
        at += i + 1;
        out.push(rel[..at].to_string());
    }
    out.push(rel.to_string());
    out
}

/// Where `rel` is ignored from: its shallowest ignored ancestor, else itself. `None`
/// unless git ignores the path itself — a tracked file under an ignored directory is
/// not subject to the ignore rules, and `check-ignore` does not report it.
fn ignore_mask(rel: &str, ignored: &std::collections::HashSet<String>) -> Option<String> {
    if !ignored.contains(rel) {
        return None;
    }
    ignore_candidates(rel)
        .into_iter()
        .find(|c| ignored.contains(c))
}

/// For each of `paths` (absolute) that its git repository ignores, the mask to show
/// it under: the absolute path of its shallowest ignored ancestor (with a trailing
/// `/`), or the file itself when only the file is ignored (`.env`).
///
/// Each path is checked against **its own** repository, not `root`'s: a session can
/// write outside the directory it was started in. `root` is only the first guess, and
/// a path under a repository already found costs no further lookup. One
/// `git check-ignore` per repository; a path in no repository is never masked, and
/// neither is anything when `git` is unavailable.
pub fn ignored_masks(root: &Path, paths: &[String]) -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;

    let mut tops: Vec<PathBuf> = toplevel(root).into_iter().collect();
    let mut outside: Vec<PathBuf> = Vec::new();
    let mut by_top: HashMap<PathBuf, Vec<(&String, String)>> = HashMap::new();
    for abs in paths {
        let path = Path::new(abs);
        let top = match tops.iter().find(|t| path.starts_with(t)) {
            Some(top) => Some(top.clone()),
            None => {
                // The nearest directory that still exists: a deleted file's own
                // directory may be gone too.
                let dir = path.ancestors().skip(1).find(|d| d.is_dir());
                match dir {
                    Some(dir) if !outside.iter().any(|o| o == dir) => match toplevel(dir) {
                        Some(top) => {
                            tops.push(top.clone());
                            Some(top)
                        }
                        None => {
                            outside.push(dir.to_path_buf());
                            None
                        }
                    },
                    _ => None,
                }
            }
        };
        let Some(top) = top else { continue };
        let Ok(rel) = path.strip_prefix(&top) else {
            continue;
        };
        let rel = rel.to_string_lossy().into_owned();
        if !rel.is_empty() {
            by_top.entry(top).or_default().push((abs, rel));
        }
    }
    let mut masks = HashMap::new();
    for (top, rels) in by_top {
        let ignored = check_ignore(
            &top,
            rels.iter().flat_map(|(_, rel)| ignore_candidates(rel)),
        );
        for (abs, rel) in rels {
            if let Some(mask) = ignore_mask(&rel, &ignored) {
                // `join` keeps the candidate's trailing `/`, so a directory mask stays one.
                masks.insert(abs.clone(), top.join(&mask).to_string_lossy().into_owned());
            }
        }
    }
    masks
}

/// The `candidates` git ignores in the repository at `top`, from one `check-ignore`.
/// Empty on any error: an error must mask nothing.
fn check_ignore(
    top: &Path,
    candidates: impl Iterator<Item = String>,
) -> std::collections::HashSet<String> {
    use std::collections::HashSet;
    use std::io::Write;
    use std::process::Stdio;

    let unique: HashSet<String> = candidates.collect();
    let Ok(mut child) = Command::new("git")
        .arg("-C")
        .arg(top)
        .args(["check-ignore", "--stdin", "-z"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return HashSet::new();
    };
    let input = unique.iter().fold(String::new(), |mut acc, c| {
        acc.push_str(c);
        acc.push('\0');
        acc
    });
    // Written from a thread: git answers while it reads, and a large batch would fill
    // the stdout pipe before stdin was drained.
    let stdin = child.stdin.take();
    let writer = std::thread::spawn(move || {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(input.as_bytes());
        }
    });
    let Ok(out) = child.wait_with_output() else {
        return HashSet::new();
    };
    let _ = writer.join();
    // 0: some ignored; 1: none; anything else is an error.
    if !matches!(out.status.code(), Some(0) | Some(1)) {
        return HashSet::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
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
    fn candidates_walk_from_the_shallowest_directory_to_the_file() {
        assert_eq!(ignore_candidates("a/b/c.txt"), ["a/", "a/b/", "a/b/c.txt"]);
        assert_eq!(ignore_candidates(".env"), [".env"]);
    }

    #[test]
    fn a_path_is_masked_at_its_first_ignored_level_and_only_if_itself_ignored() {
        let ignored: std::collections::HashSet<String> = [
            "pkg/node_modules/",
            "pkg/node_modules/x/",
            "pkg/node_modules/x/y.js",
            ".env",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(
            ignore_mask("pkg/node_modules/x/y.js", &ignored).as_deref(),
            Some("pkg/node_modules/")
        );
        assert_eq!(ignore_mask(".env", &ignored).as_deref(), Some(".env"));
        // Tracked under an ignored directory: git does not report the file itself.
        assert_eq!(ignore_mask("pkg/node_modules/tracked.js", &ignored), None);
        assert_eq!(ignore_mask("src/a.rs", &ignored), None);
    }

    #[test]
    fn ignored_masks_reads_the_repository_ignore_rules() {
        let repo = TempRepo::new("ignored-masks");
        std::fs::write(repo.0.join(".gitignore"), "build/\n.env\n").unwrap();
        let abs = |rel: &str| repo.0.join(rel).to_string_lossy().into_owned();
        let paths = vec![
            abs("mod/build/out/a.class"),
            abs(".env"),
            abs("src/a.rs"),
            "/elsewhere/x".to_string(),
        ];
        let masks = ignored_masks(&repo.0, &paths);
        assert_eq!(
            masks.get(&abs("mod/build/out/a.class")),
            Some(&(abs("mod/build") + "/"))
        );
        assert_eq!(masks.get(&abs(".env")), Some(&abs(".env")));
        assert!(!masks.contains_key(&abs("src/a.rs")));
        assert!(!masks.contains_key("/elsewhere/x"));

        // A session started elsewhere that wrote into this repository: its own rules apply.
        let other = TempRepo::new("ignored-masks-other-root");
        let masks = ignored_masks(&other.0, &[abs("mod/build/out/a.class")]);
        assert_eq!(
            masks.get(&abs("mod/build/out/a.class")),
            Some(&(abs("mod/build") + "/"))
        );
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
