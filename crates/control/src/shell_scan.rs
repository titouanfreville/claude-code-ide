//! Attribute writes made *through the shell* to the session that ran the command.
//!
//! A `Bash` call declares no target path, so there is nothing to record when it
//! starts. Instead the workspace is fingerprinted either side of the command and
//! the difference is the set of files it wrote:
//!
//! ```text
//!   PreToolUse(Bash)   → fingerprint the dirty set (path → mtime, size)
//!   …command runs…
//!   PostToolUse(Bash)  → fingerprint again; new or changed = written
//! ```
//!
//! Only the *dirty* set is fingerprinted, not the whole tree: a file the command
//! modifies becomes dirty by definition, so it shows up in the second scan even if
//! it was clean (and therefore unlisted) in the first. That keeps the cost to one
//! `git status` plus a stat per already-dirty file, and inherits git's ignore rules
//! for free — no walking `target/` or `node_modules/`.

use std::collections::HashMap;

/// A file's identity at a point in time. Content is deliberately not hashed: this
/// runs on the hook's critical path, while Claude Code waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub modified_ms: i64,
    pub len: u64,
}

/// The workspace fingerprint taken before a shell command ran.
#[derive(Debug, Clone, Default)]
pub struct Fingerprint {
    files: HashMap<String, Stamp>,
}

impl Fingerprint {
    /// Stat every path in `paths`, skipping those that can't be read (a path that
    /// vanishes between listing and stat simply isn't in the fingerprint).
    pub fn of(paths: &[String]) -> Self {
        let files = paths
            .iter()
            .filter_map(|path| stamp(path).map(|s| (path.clone(), s)))
            .collect();
        Self { files }
    }

    /// Paths in `self` that are absent from `before`, or whose stamp changed —
    /// i.e. what the command wrote. Sorted, so the ledger records deterministically.
    pub fn written_since(&self, before: &Fingerprint) -> Vec<String> {
        let mut written: Vec<String> = self
            .files
            .iter()
            .filter(|(path, now)| before.files.get(*path) != Some(now))
            .map(|(path, _)| path.clone())
            .collect();
        written.sort();
        written
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// `path`'s modification time (epoch millis) and length, or `None` if it can't be
/// stat'd or isn't a regular file.
fn stamp(path: &str) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let modified_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Some(Stamp {
        modified_ms,
        len: meta.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingerprint(entries: &[(&str, i64, u64)]) -> Fingerprint {
        Fingerprint {
            files: entries
                .iter()
                .map(|(path, modified_ms, len)| {
                    (
                        (*path).to_string(),
                        Stamp {
                            modified_ms: *modified_ms,
                            len: *len,
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn a_file_that_did_not_change_is_not_a_write() {
        let before = fingerprint(&[("/repo/a.rs", 10, 100)]);
        let after = fingerprint(&[("/repo/a.rs", 10, 100)]);
        assert!(after.written_since(&before).is_empty());
    }

    #[test]
    fn a_new_dirty_file_is_a_write() {
        // The command made a previously-clean (so unlisted) file dirty.
        let before = fingerprint(&[]);
        let after = fingerprint(&[("/repo/new.rs", 10, 100)]);
        assert_eq!(after.written_since(&before), vec!["/repo/new.rs"]);
    }

    #[test]
    fn a_rewritten_file_is_a_write_even_at_the_same_size() {
        // `sed -i s/a/b/` keeps the length; the mtime is what gives it away.
        let before = fingerprint(&[("/repo/a.rs", 10, 100)]);
        let after = fingerprint(&[("/repo/a.rs", 20, 100)]);
        assert_eq!(after.written_since(&before), vec!["/repo/a.rs"]);

        // …and a same-mtime append is caught by the length.
        let grown = fingerprint(&[("/repo/a.rs", 10, 140)]);
        assert_eq!(grown.written_since(&before), vec!["/repo/a.rs"]);
    }

    #[test]
    fn a_file_that_became_clean_again_is_not_reported() {
        // It left the dirty set (e.g. `git checkout --`), so there is nothing to
        // review — we only report what exists now.
        let before = fingerprint(&[("/repo/a.rs", 10, 100)]);
        let after = fingerprint(&[]);
        assert!(after.written_since(&before).is_empty());
    }

    #[test]
    fn writes_are_reported_in_a_stable_order() {
        let before = fingerprint(&[]);
        let after = fingerprint(&[("/repo/z.rs", 1, 1), ("/repo/a.rs", 1, 1)]);
        assert_eq!(
            after.written_since(&before),
            vec!["/repo/a.rs", "/repo/z.rs"]
        );
    }

    #[test]
    fn stamping_a_real_file_reads_its_length() {
        let path = std::env::temp_dir().join(format!("ml-stamp-{}.txt", std::process::id()));
        std::fs::write(&path, "hello").unwrap();
        let stamped = Fingerprint::of(&[path.to_string_lossy().into_owned()]);
        assert!(!stamped.is_empty());
        // A path that doesn't exist is simply absent rather than an error.
        let missing = Fingerprint::of(&["/definitely/not/here".to_string()]);
        assert!(missing.is_empty());
        std::fs::remove_file(&path).ok();
    }
}
