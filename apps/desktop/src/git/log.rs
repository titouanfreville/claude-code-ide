//! Recent-history read for the **Git tool window**'s Log view.
//!
//! One `git log` with NUL-separated fields (and a `\x1e` record separator so
//! multi-line-safe even if a subject somehow carries a NUL-free newline via
//! `%s` it can't — but refs/decorations stay unambiguous). Pure parsing is
//! split out for tests.

use std::path::Path;
use std::process::Command;

/// One commit row in the Log view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    /// Abbreviated hash (display form).
    pub hash: String,
    pub subject: String,
    pub author: String,
    /// Relative committer date ("3 hours ago").
    pub when: String,
    /// Decorations ("HEAD -> main, origin/main"), empty when none.
    pub refs: String,
}

/// Parse the `%h%x00%s%x00%an%x00%cr%x00%D%x1e` stream into entries.
pub fn parse_log(output: &str) -> Vec<LogEntry> {
    output
        .split('\u{1e}')
        .filter(|rec| !rec.trim().is_empty())
        .filter_map(|rec| {
            let mut f = rec.trim_start_matches('\n').split('\0');
            Some(LogEntry {
                hash: f.next()?.to_string(),
                subject: f.next()?.to_string(),
                author: f.next()?.to_string(),
                when: f.next()?.to_string(),
                refs: f.next().unwrap_or("").to_string(),
            })
        })
        .collect()
}

/// Load the last `n` commits for the repo containing `root`. `None` when not a
/// repo / no commits yet / git unavailable.
pub fn load_log(root: &Path, n: usize) -> Option<Vec<LogEntry>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "log",
            &format!("-n{n}"),
            "--pretty=format:%h%x00%s%x00%an%x00%cr%x00%D%x1e",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_log(&String::from_utf8_lossy(&out.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_log_splits_records_and_fields() {
        let raw = "abc1234\0fix: thing\0Ada\03 hours ago\0HEAD -> main, origin/main\u{1e}\ndef5678\0feat: other\0Bob\02 days ago\0\u{1e}";
        let log = parse_log(raw);
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].hash, "abc1234");
        assert_eq!(log[0].subject, "fix: thing");
        assert_eq!(log[0].refs, "HEAD -> main, origin/main");
        assert_eq!(log[1].author, "Bob");
        assert_eq!(log[1].refs, "");
    }

    #[test]
    fn parse_log_tolerates_empty_output() {
        assert!(parse_log("").is_empty());
    }
}
