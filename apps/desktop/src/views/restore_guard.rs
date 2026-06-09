//! Restore safe-mode breadcrumb — the boot-loop circuit-breaker.
//!
//! `Workspace::new` runs the eager open-tab restore (build panels, spawn the sessions'
//! PTYs) **synchronously inside** `cx.open_window(...).expect(...)` in `main`. A panic
//! there is fatal *at launch*, and because the same `open_tabs.json` reloads every
//! launch, a restore that panics becomes a boot loop the operator can only escape by
//! deleting state — the "I lost my sessions again" trap.
//!
//! This breadcrumb breaks the loop: [`begin`] writes a marker right before restore,
//! [`finish`] removes it right after. If a launch finds the marker still present
//! ([`crashed_last_time`]), the previous run died mid-restore, so the workspace skips
//! the eager restore *once* and lets the operator back in. Nothing is lost — the
//! managed-session records rehydrate the fleet grid regardless; only the auto-reopen of
//! tabs is skipped, and the next clean launch restores normally.
//!
//! Best-effort and fail-open: a missing/unreadable marker reads as "clean".

use std::path::PathBuf;

/// macOS-first marker location under Application Support (next to `open_tabs.json`).
fn path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/MoonlightCode/restore.lock"))
}

/// Whether the previous launch died *during* restore — a [`begin`] marker that was
/// never cleared by [`finish`]. Fail-open: any error reads as "clean" (not crashed).
pub fn crashed_last_time() -> bool {
    path().is_some_and(|p| crashed_at(&p))
}

/// Mark "restore in progress". Call immediately before the eager restore runs.
pub fn begin() {
    if let Some(path) = path() {
        begin_at(&path);
    }
}

/// Clear the marker — restore finished (cleanly, or we deliberately skipped it).
pub fn finish() {
    if let Some(path) = path() {
        finish_at(&path);
    }
}

// Path-parameterized cores (so the breadcrumb logic is testable without touching the
// real, process-global `$HOME`-derived location).

fn crashed_at(path: &std::path::Path) -> bool {
    path.exists()
}

fn begin_at(path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, b"restoring");
}

fn finish_at(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_without_finish_reads_as_crashed() {
        let dir = std::env::temp_dir().join(format!("mlc-restore-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("restore.lock");

        // Clean to start (no marker).
        assert!(!crashed_at(&path));
        // begin() without finish() → the next launch sees a crash.
        begin_at(&path);
        assert!(crashed_at(&path));
        // finish() clears it → clean again.
        finish_at(&path);
        assert!(!crashed_at(&path));
        // finish() on an already-clean path is a no-op (no panic).
        finish_at(&path);
        assert!(!crashed_at(&path));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
