//! Per-file working-tree diffs for the code-review gate (G4).
//!
//! Lists the files an agent changed in a repo and produces the unified diff of each
//! against `HEAD` (the "what did this session do" view). Like [`super::status`], it
//! shells out to the system `git` and is blocking — callers run it off the UI
//! thread. Diff *text* is returned raw; the review panel colorizes it per line.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use moonlight_domain::ids::{HunkId, SessionId};
use moonlight_domain::review::ReviewHunk;

use super::status::{parse_porcelain, GitFileStatus};

/// One changed file in the working tree: its repo-relative path and status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub rel_path: PathBuf,
    pub status: GitFileStatus,
}

impl ChangedFile {
    /// Display path (repo-relative, forward slashes as git emits them).
    pub fn display(&self) -> String {
        self.rel_path.to_string_lossy().into_owned()
    }
}

/// The repo toplevel that contains `root`, or `None` if `root` is not in a git repo
/// (or `git` is unavailable).
pub fn toplevel(root: &Path) -> Option<PathBuf> {
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

/// List the working-tree changes in the repo containing `root`, sorted by path for
/// a stable file list. Returns an empty vec when there are no changes and `None`
/// only when `root` is not a git repo / `git` is unavailable.
pub fn changed_files(root: &Path) -> Option<Vec<ChangedFile>> {
    toplevel(root)?;
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut files: Vec<ChangedFile> = parse_porcelain(&text)
        .into_iter()
        .map(|(rel_path, status)| ChangedFile { rel_path, status })
        .collect();
    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Some(files)
}

/// The unified diff of one changed file against `HEAD` (or, for an untracked file,
/// against an empty tree so its whole content shows as additions). Returns an empty
/// string when there is nothing to show; `None` when `git` cannot be run.
pub fn file_diff(root: &Path, file: &ChangedFile) -> Option<String> {
    let rel = file.rel_path.as_os_str();
    let out = if file.status == GitFileStatus::Untracked {
        // Untracked files aren't in HEAD; compare against /dev/null to render the
        // full file as additions. `--no-index` exits 1 when files differ — expected,
        // so we read stdout regardless of the exit code.
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["diff", "--no-color", "--no-index", "--", "/dev/null"])
            .arg(rel)
            .output()
            .ok()?
    } else {
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["diff", "--no-color", "HEAD", "--"])
            .arg(rel)
            .output()
            .ok()?
    };
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Split a file's unified diff into its `(header, hunk_bodies)`: the preamble
/// (`diff --git`, `index`, `---`, `+++`) up to the first `@@`, then one string per
/// `@@ … @@` section (each `@@` header + its body, trailing newline trimmed). The
/// header is needed to *reassemble* a valid patch for staging a subset of hunks
/// ([`restage_file`]); the bodies are what the review UI renders per hunk.
pub fn split_file_diff(diff: &str) -> (String, Vec<String>) {
    let mut header = String::new();
    let mut bodies: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    for line in diff.lines() {
        if line.starts_with("@@") {
            if let Some(done) = current.take() {
                bodies.push(done);
            }
            current = Some(String::new());
        }
        match current.as_mut() {
            Some(buf) => {
                buf.push_str(line);
                buf.push('\n');
            }
            None => {
                header.push_str(line);
                header.push('\n');
            }
        }
    }
    if let Some(done) = current.take() {
        bodies.push(done);
    }
    let bodies = bodies
        .into_iter()
        .map(|b| b.trim_end_matches('\n').to_string())
        .collect();
    (header, bodies)
}

/// Split a single file's unified diff into individual reviewable [`ReviewHunk`]s
/// (one per `@@ … @@` section) — the unit the operator accepts or rejects. A diff
/// with no textual hunks (binary, pure rename) yields an empty vec, and the caller
/// falls back to showing the raw diff.
pub fn parse_hunks(diff: &str, session: &SessionId, file_path: &str) -> Vec<ReviewHunk> {
    let (_, bodies) = split_file_diff(diff);
    bodies
        .into_iter()
        .enumerate()
        .map(|(i, body)| ReviewHunk {
            id: HunkId::new(format!("{file_path}#{i}")),
            session_id: session.clone(),
            file_path: file_path.to_string(),
            diff: body,
        })
        .collect()
}

/// Stage exactly the operator-**accepted** hunks of one file into the git index,
/// idempotently. The model: reset the file's index entry to `HEAD`, then apply the
/// reassembled patch (`header` + accepted hunk bodies) with `git apply --cached
/// --recount` — `--recount` re-derives the `@@` line counts so applying a *subset*
/// of hunks is robust (no manual line-number surgery, no sequential drift).
///
/// `accepted_bodies` are the `@@`-prefixed hunk strings from [`split_file_diff`].
/// An **untracked** file has no per-hunk granularity (it's all additions): any
/// accepted hunk stages the whole file, none unstages it. Returns the git error
/// text on failure so the review panel can surface it.
pub fn restage_file(
    root: &Path,
    rel_path: &str,
    header: &str,
    accepted_bodies: &[String],
    untracked: bool,
) -> Result<(), String> {
    if untracked {
        let args: &[&str] = if accepted_bodies.is_empty() {
            &["reset", "-q", "--", rel_path] // unstage the whole new file
        } else {
            &["add", "--", rel_path] // stage the whole new file
        };
        return run_git(root, args);
    }

    // Tracked: reset the index entry to HEAD first, so re-staging is idempotent and
    // free of drift from previously-applied hunks.
    run_git(root, &["reset", "-q", "--", rel_path])?;
    if accepted_bodies.is_empty() {
        return Ok(());
    }

    let mut patch = String::new();
    patch.push_str(header);
    if !header.ends_with('\n') {
        patch.push('\n');
    }
    for body in accepted_bodies {
        patch.push_str(body);
        if !body.ends_with('\n') {
            patch.push('\n');
        }
    }
    apply_cached(root, &patch)
}

/// Run `git -C root <args>`, mapping a non-zero exit to its stderr text.
fn run_git(root: &Path, args: &[&str]) -> Result<(), String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("git {args:?} failed to launch: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Pipe `patch` into `git apply --cached --recount` (stage it without touching the
/// working tree). `--recount` lets git infer hunk line counts, so a reassembled
/// subset of hunks applies cleanly.
fn apply_cached(root: &Path, patch: &str) -> Result<(), String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["apply", "--cached", "--recount", "--whitespace=nowarn", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git apply failed to launch: {e}"))?;
    {
        let mut stdin = child.stdin.take().ok_or("git apply: no stdin")?;
        stdin
            .write_all(patch.as_bytes())
            .map_err(|e| format!("git apply: writing patch failed: {e}"))?;
    } // stdin dropped → EOF, so git apply proceeds
    let out = child
        .wait_with_output()
        .map_err(|e| format!("git apply: wait failed: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git apply --cached failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hunks_splits_on_at_markers_and_drops_preamble() {
        let diff = "diff --git a/x.rs b/x.rs\nindex 1..2 100644\n--- a/x.rs\n+++ b/x.rs\n\
                    @@ -1,2 +1,3 @@\n ctx\n+added\n@@ -10,1 +11,2 @@\n-old\n+new\n";
        let hunks = parse_hunks(diff, &SessionId::new("s"), "x.rs");
        assert_eq!(hunks.len(), 2);
        assert!(hunks[0].diff.starts_with("@@ -1,2 +1,3 @@"));
        assert!(hunks[0].diff.contains("+added"));
        assert!(
            !hunks[0].diff.contains("diff --git"),
            "the file preamble must be dropped, got {:?}",
            hunks[0].diff
        );
        assert!(hunks[1].diff.starts_with("@@ -10,1 +11,2 @@"));
        assert_eq!(hunks[0].file_path, "x.rs");
        assert_eq!(hunks[1].id, HunkId::new("x.rs#1"));
        assert_eq!(hunks[0].session_id, SessionId::new("s"));
    }

    #[test]
    fn parse_hunks_empty_for_no_textual_changes() {
        // Binary diffs have no @@ sections → nothing to review per-hunk.
        let diff = "diff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\n";
        assert!(parse_hunks(diff, &SessionId::new("s"), "img.png").is_empty());
    }

    #[test]
    fn parse_hunks_untracked_whole_file_is_one_hunk() {
        let diff = "diff --git a/new.rs b/new.rs\n--- /dev/null\n+++ b/new.rs\n\
                    @@ -0,0 +1,2 @@\n+line1\n+line2\n";
        let hunks = parse_hunks(diff, &SessionId::new("s"), "new.rs");
        assert_eq!(hunks.len(), 1);
        assert!(hunks[0].diff.contains("+line1"));
        assert!(hunks[0].diff.contains("+line2"));
    }

    #[test]
    fn split_file_diff_separates_header_and_hunks() {
        let diff = "diff --git a/x.rs b/x.rs\nindex 1..2 100644\n--- a/x.rs\n+++ b/x.rs\n\
                    @@ -1 +1 @@\n-a\n+b\n@@ -5 +5 @@\n-c\n+d\n";
        let (header, bodies) = split_file_diff(diff);
        assert!(header.contains("--- a/x.rs") && header.contains("+++ b/x.rs"));
        assert!(!header.contains("@@"), "header must stop before the first hunk");
        assert_eq!(bodies.len(), 2);
        assert!(bodies[0].starts_with("@@ -1 +1 @@"));
        assert!(bodies[1].contains("+d"));
    }

    #[test]
    fn restage_file_stages_only_accepted_hunks() {
        // A throwaway repo so we exercise real `git apply --cached`.
        let dir = std::env::temp_dir().join(format!("ml-restage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args([
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "user.name=t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out
        };
        git(&["init", "-q"]);
        let file = dir.join("f.txt");
        std::fs::write(&file, "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "init"]);
        // Two well-separated edits ⇒ two distinct hunks.
        std::fs::write(&file, "L1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nL9\nl10\n").unwrap();

        let cf = ChangedFile {
            rel_path: PathBuf::from("f.txt"),
            status: GitFileStatus::Modified,
        };
        let diff = file_diff(&dir, &cf).unwrap();
        let (header, bodies) = split_file_diff(&diff);
        assert_eq!(bodies.len(), 2, "expected 2 hunks, diff was:\n{diff}");

        // Accept only the first hunk (the L1 change).
        restage_file(&dir, "f.txt", &header, &bodies[0..1], false).unwrap();

        let staged = git(&["diff", "--cached"]);
        let staged = String::from_utf8_lossy(&staged.stdout);
        assert!(staged.contains("+L1"), "first hunk should be staged:\n{staged}");
        assert!(
            !staged.contains("+L9"),
            "second (unaccepted) hunk must NOT be staged:\n{staged}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn changed_file_display_is_repo_relative() {
        let f = ChangedFile {
            rel_path: PathBuf::from("src/main.rs"),
            status: GitFileStatus::Modified,
        };
        assert_eq!(f.display(), "src/main.rs");
    }

    #[test]
    fn non_repo_path_yields_none() {
        // A path guaranteed not to be inside a git repo.
        let tmp = std::env::temp_dir().join("moonlight-not-a-repo-xyz");
        let _ = std::fs::create_dir_all(&tmp);
        assert!(changed_files(&tmp).is_none());
    }
}
