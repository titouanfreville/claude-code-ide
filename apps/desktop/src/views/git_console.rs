//! The shared **git command console**: every git operation the IDE itself runs
//! (toolbar checkout, Commit-tool stage/unstage/commit) records its command and
//! outcome here, and the Git tool window's Console view replays it — errors
//! stop being toast-only.
//!
//! Same shape as the run registry's UI contract: a cheaply-cloneable sync sink
//! (`Arc<Mutex<…>>` + an atomic dirty counter) that panels poll. UI-local —
//! never the engine bus.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Keep the last N operations (a console, not an audit log — that's moonlight.db).
const CAP: usize = 200;

/// One recorded git operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitOp {
    /// Display form of what ran ("checkout feature-x", "commit", "stage a.rs").
    pub label: String,
    pub ok: bool,
    /// Stdout summary on success, stderr on failure (may be empty).
    pub detail: String,
}

/// Cloneable handle to the shared console buffer.
#[derive(Clone, Default)]
pub struct GitConsole {
    ops: Arc<Mutex<Vec<GitOp>>>,
    seq: Arc<AtomicU64>,
}

impl GitConsole {
    /// Record one operation's outcome (ring-capped).
    pub fn record(&self, label: impl Into<String>, result: &Result<String, String>) {
        let op = match result {
            Ok(out) => GitOp {
                label: label.into(),
                ok: true,
                detail: out.clone(),
            },
            Err(err) => GitOp {
                label: label.into(),
                ok: false,
                detail: err.clone(),
            },
        };
        let mut ops = self.ops.lock().unwrap_or_else(|p| p.into_inner());
        ops.push(op);
        let len = ops.len();
        if len > CAP {
            ops.drain(..len - CAP);
        }
        self.seq.fetch_add(1, Ordering::Relaxed);
    }

    /// Dirty counter — compare across polls to skip unchanged re-renders.
    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }

    /// Snapshot of the recorded ops, oldest first.
    pub fn ops(&self) -> Vec<GitOp> {
        self.ops.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_bumps_seq_and_caps_the_buffer() {
        let console = GitConsole::default();
        assert_eq!(console.seq(), 0);

        console.record("checkout main", &Ok(String::new()));
        console.record("commit", &Err("nothing staged".into()));
        assert_eq!(console.seq(), 2);

        let ops = console.ops();
        assert_eq!(ops.len(), 2);
        assert!(ops[0].ok);
        assert!(!ops[1].ok);
        assert_eq!(ops[1].detail, "nothing staged");

        for i in 0..(CAP + 10) {
            console.record(format!("op {i}"), &Ok(String::new()));
        }
        assert_eq!(console.ops().len(), CAP);
        // Oldest entries were dropped — the first surviving label is post-cap.
        assert!(console.ops()[0].label.starts_with("op "));
    }
}
