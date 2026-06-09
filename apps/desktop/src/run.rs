//! The **run registry** — shared state behind the JetBrains-style Run tool window
//! and the MCP run verbs. Holds **one entry per run target** (the Run window's
//! onglets): launching `cargo run` then `cargo test` yields two tabs, each with its
//! own captured logs and live status; re-running a target **reuses its tab**
//! (JetBrains behavior) instead of stacking duplicates.
//!
//! Deliberately **GPUI-free and tokio-free** (std threads + a mutex), because it is
//! shared by two worlds: the [`run_console`](crate::views::panels::run_console) panel
//! polls it from the UI, and the MCP `RunVerbExecutor` reads/drives it from the MCP
//! host's tokio runtime. Whoever starts a run — the operator's ▶ or a CC session's
//! `run_start` verb — both sides see the same tabs, the same logs.
//!
//! Process model: each command runs through `sh -c` in its project root with
//! **piped** stdout/stderr (a captured console, not a PTY — JetBrains' Run window
//! model). Two reader threads per run append lines to a capped ring; a waiter thread
//! `try_wait`-polls for the exit status (no blocking `wait` while others need the
//! child for `kill`). Readers/waiters carry `(run id, epoch)` so a restarted or
//! removed run's stale threads stop touching state.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// Cap on retained log lines per run — old lines drop off the front (the agent reads
/// tails; the operator scrolls recent output).
const LOG_CAP: usize = 10_000;

/// Where a run is in its lifecycle.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RunStatus {
    Running,
    /// Process exited on its own with this code.
    Exited(i32),
    /// The operator/agent stopped it.
    Killed,
    /// The spawn itself failed (command not found, bad root).
    Failed(String),
}

impl RunStatus {
    pub fn is_running(&self) -> bool {
        matches!(self, RunStatus::Running)
    }

    /// Compact human/agent-readable status word ("running", "exit 0", …).
    pub fn label(&self) -> String {
        match self {
            RunStatus::Running => "running".into(),
            RunStatus::Exited(code) => format!("exit {code}"),
            RunStatus::Killed => "stopped".into(),
            RunStatus::Failed(err) => format!("failed: {err}"),
        }
    }
}

/// One captured output line. `stderr` drives the console's tinting.
#[derive(Clone, Debug)]
pub struct LogLine {
    pub stderr: bool,
    pub text: String,
}

/// The light, per-onglet slice (no logs) — what the Run window's tab bar renders
/// and what the MCP `run_status` verb lists.
#[derive(Clone)]
pub struct RunTab {
    pub id: u64,
    pub label: String,
    pub command: String,
    pub status: RunStatus,
}

/// The full, render/agent-ready snapshot of one run (its onglet's body).
#[derive(Clone)]
pub struct RunSnapshot {
    pub id: u64,
    pub label: String,
    pub command: String,
    /// The project root the command ran in (Rerun relaunches here).
    pub root: PathBuf,
    pub status: RunStatus,
    pub logs: Vec<LogLine>,
}

struct RunEntry {
    id: u64,
    label: String,
    command: String,
    root: PathBuf,
    status: RunStatus,
    logs: VecDeque<LogLine>,
    child: Option<Child>,
    /// Restart counter: reader/waiter threads carry `(id, epoch)` and stop touching
    /// the entry once a rerun bumped it (or the entry was removed).
    epoch: u64,
}

struct Inner {
    runs: Vec<RunEntry>,
    /// Global mutation counter (the UI's dirty check).
    seq: u64,
    next_id: u64,
    /// The most recently *started* run — what new output fronts in the UI and what
    /// target-less MCP reads default to.
    last_started: Option<u64>,
}

/// Thread-safe handle to the run set. Cheap to clone; every clone sees the same runs.
#[derive(Clone)]
pub struct RunRegistry {
    inner: Arc<Mutex<Inner>>,
}

impl Default for RunRegistry {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                runs: Vec::new(),
                seq: 0,
                next_id: 0,
                last_started: None,
            })),
        }
    }
}

impl RunRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Launch `command` (via `sh -c`) in `root`. A target that already has a tab
    /// (same command) is **restarted in place** — killed if live, logs cleared —
    /// otherwise a new tab is created. Returns the run's id, or the spawn error.
    /// The lock is held across the (fast) spawn so concurrent starts — operator ▶ vs
    /// an agent's `run_start` — fully serialize instead of leaking an orphan child.
    pub fn start(&self, label: &str, command: &str, root: PathBuf) -> Result<u64, String> {
        let mut inner = self.lock();
        inner.seq += 1;

        // Reuse the target's existing tab (JetBrains rerun) or mint a new one.
        let idx = match inner.runs.iter().position(|r| r.command == command) {
            Some(idx) => {
                let entry = &mut inner.runs[idx];
                if let Some(child) = entry.child.as_mut() {
                    if !matches!(child.try_wait(), Ok(Some(_))) {
                        let _ = child.kill();
                        let _ = child.wait(); // reap; instant after a kill
                    }
                }
                entry.child = None;
                entry.epoch += 1;
                entry.label = label.to_string();
                entry.root = root.clone();
                entry.logs.clear();
                idx
            }
            None => {
                let id = inner.next_id;
                inner.next_id += 1;
                inner.runs.push(RunEntry {
                    id,
                    label: label.to_string(),
                    command: command.to_string(),
                    root: root.clone(),
                    status: RunStatus::Running,
                    logs: VecDeque::new(),
                    child: None,
                    epoch: 0,
                });
                inner.runs.len() - 1
            }
        };

        let spawned = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let entry = &mut inner.runs[idx];
        let mut child = match spawned {
            Ok(child) => child,
            Err(err) => {
                let msg = err.to_string();
                entry.status = RunStatus::Failed(msg.clone());
                return Err(msg);
            }
        };
        // The readers own the pipes; the registry keeps the child for kill/try_wait.
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        entry.status = RunStatus::Running;
        entry.child = Some(child);
        let (id, epoch) = (entry.id, entry.epoch);
        inner.last_started = Some(id);
        drop(inner);

        if let Some(out) = stdout {
            self.spawn_reader(out, false, id, epoch);
        }
        if let Some(err) = stderr {
            self.spawn_reader(err, true, id, epoch);
        }
        self.spawn_waiter(id, epoch);
        Ok(id)
    }

    /// Kill run `id` if it's still alive. Returns whether a process was killed.
    pub fn stop(&self, id: u64) -> bool {
        let mut inner = self.lock();
        let Some(entry) = inner.runs.iter_mut().find(|r| r.id == id) else {
            return false;
        };
        let Some(child) = entry.child.as_mut() else {
            return false;
        };
        // Already exited (waiter just hasn't folded it yet) → not a kill.
        if matches!(child.try_wait(), Ok(Some(_))) {
            return false;
        }
        let killed = child.kill().is_ok();
        let _ = child.wait(); // reap; instant after a kill
        entry.child = None;
        if killed {
            entry.status = RunStatus::Killed;
            inner.seq += 1;
        }
        killed
    }

    /// Close run `id`'s onglet: kill it if live, then drop the entry entirely.
    pub fn remove(&self, id: u64) -> bool {
        self.stop(id);
        let mut inner = self.lock();
        let before = inner.runs.len();
        inner.runs.retain(|r| r.id != id);
        let removed = inner.runs.len() != before;
        if removed {
            if inner.last_started == Some(id) {
                inner.last_started = inner.runs.last().map(|r| r.id);
            }
            inner.seq += 1;
        }
        removed
    }

    /// Drop run `id`'s captured lines (the console's ⌫); keeps status/command.
    pub fn clear_logs(&self, id: u64) {
        let mut inner = self.lock();
        if let Some(entry) = inner.runs.iter_mut().find(|r| r.id == id) {
            if !entry.logs.is_empty() {
                entry.logs.clear();
                inner.seq += 1;
            }
        }
    }

    /// The global mutation counter (cheap dirty check for the UI's poll loop).
    pub fn seq(&self) -> u64 {
        self.lock().seq
    }

    /// The onglet strip: one light tab per run target, in creation order.
    pub fn tabs(&self) -> Vec<RunTab> {
        self.lock()
            .runs
            .iter()
            .map(|r| RunTab {
                id: r.id,
                label: r.label.clone(),
                command: r.command.clone(),
                status: r.status.clone(),
            })
            .collect()
    }

    /// The chrome's one-lamp summary: `Running` while anything is live, else the
    /// most recently started run's verdict, `None` when nothing ran yet.
    pub fn overall_status(&self) -> Option<RunStatus> {
        let inner = self.lock();
        if inner.runs.iter().any(|r| r.status.is_running()) {
            return Some(RunStatus::Running);
        }
        inner
            .last_started
            .and_then(|id| inner.runs.iter().find(|r| r.id == id))
            .map(|r| r.status.clone())
    }

    /// Whether the run for this exact command is live (the toolbar's play⇄stop swap
    /// is scoped to its selected target).
    pub fn command_running(&self, command: &str) -> bool {
        self.lock()
            .runs
            .iter()
            .any(|r| r.command == command && r.status.is_running())
    }

    /// The most recently started run (what new output fronts; the target-less
    /// default for MCP reads).
    pub fn last_started(&self) -> Option<u64> {
        self.lock().last_started
    }

    /// A full snapshot of run `id` (logs included), or `None` when its onglet closed.
    pub fn snapshot(&self, id: u64) -> Option<RunSnapshot> {
        let inner = self.lock();
        inner.runs.iter().find(|r| r.id == id).map(|r| RunSnapshot {
            id: r.id,
            label: r.label.clone(),
            command: r.command.clone(),
            root: r.root.clone(),
            status: r.status.clone(),
            logs: r.logs.iter().cloned().collect(),
        })
    }

    /// Resolve a run by its exact command (MCP targets address runs this way).
    pub fn find_by_command(&self, command: &str) -> Option<u64> {
        self.lock()
            .runs
            .iter()
            .find(|r| r.command == command)
            .map(|r| r.id)
    }

    /// The last `n` log lines of run `id` as plain text (the MCP `run_logs` payload).
    pub fn tail(&self, id: u64, n: usize) -> String {
        let inner = self.lock();
        let Some(entry) = inner.runs.iter().find(|r| r.id == id) else {
            return String::new();
        };
        let skip = entry.logs.len().saturating_sub(n);
        entry
            .logs
            .iter()
            .skip(skip)
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Reader thread: stream one pipe line-by-line into run `id`'s capped ring.
    /// Stops touching state when the run was rerun (epoch bump) or its tab closed.
    fn spawn_reader(
        &self,
        pipe: impl std::io::Read + Send + 'static,
        stderr: bool,
        id: u64,
        epoch: u64,
    ) {
        let registry = self.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines() {
                let Ok(text) = line else { break };
                let mut inner = registry.lock();
                let Some(entry) = inner.runs.iter_mut().find(|r| r.id == id) else {
                    break; // onglet closed
                };
                if entry.epoch != epoch {
                    break; // a rerun owns this tab now
                }
                if entry.logs.len() >= LOG_CAP {
                    entry.logs.pop_front();
                }
                entry.logs.push_back(LogLine { stderr, text });
                inner.seq += 1;
            }
        });
    }

    /// Waiter thread: poll `try_wait` until run `id`'s child exits, then fold the
    /// exit status. Polling (vs a blocking `wait`) keeps the child under the mutex
    /// so `stop`/`remove` can still kill it.
    fn spawn_waiter(&self, id: u64, epoch: u64) {
        let registry = self.clone();
        std::thread::spawn(move || loop {
            {
                let mut inner = registry.lock();
                let Some(entry) = inner.runs.iter_mut().find(|r| r.id == id) else {
                    return; // onglet closed
                };
                if entry.epoch != epoch {
                    return; // a rerun owns this tab now
                }
                let Some(child) = entry.child.as_mut() else {
                    return; // stopped (killed) meanwhile
                };
                match child.try_wait() {
                    Ok(Some(status)) => {
                        entry.child = None;
                        entry.status = RunStatus::Exited(status.code().unwrap_or(-1));
                        inner.seq += 1;
                        return;
                    }
                    Ok(None) => {} // still running
                    Err(_) => {
                        entry.child = None;
                        entry.status = RunStatus::Failed("wait failed".into());
                        inner.seq += 1;
                        return;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Poll until `pred` holds on run `id` or ~5s passes.
    fn wait_for(registry: &RunRegistry, id: u64, pred: impl Fn(&RunSnapshot) -> bool) -> RunSnapshot {
        for _ in 0..100 {
            if let Some(snap) = registry.snapshot(id) {
                if pred(&snap) {
                    return snap;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        registry.snapshot(id).expect("run still exists")
    }

    #[test]
    fn start_captures_stdout_stderr_and_exit_code() {
        let reg = RunRegistry::new();
        let id = reg
            .start(
                "demo",
                "echo out-line; echo err-line >&2; exit 3",
                std::env::temp_dir(),
            )
            .expect("spawn");
        let snap = wait_for(&reg, id, |s| s.status == RunStatus::Exited(3));
        assert_eq!(snap.status, RunStatus::Exited(3));
        assert!(snap.logs.iter().any(|l| !l.stderr && l.text == "out-line"));
        assert!(snap.logs.iter().any(|l| l.stderr && l.text == "err-line"));
        assert_eq!(snap.label, "demo");
        assert_eq!(reg.last_started(), Some(id));
    }

    #[test]
    fn two_targets_get_two_tabs_and_rerun_reuses_one() {
        let reg = RunRegistry::new();
        let a = reg.start("a", "echo first", std::env::temp_dir()).unwrap();
        let b = reg.start("b", "echo second", std::env::temp_dir()).unwrap();
        assert_ne!(a, b);
        assert_eq!(reg.tabs().len(), 2);
        wait_for(&reg, a, |s| matches!(s.status, RunStatus::Exited(_)));
        wait_for(&reg, b, |s| matches!(s.status, RunStatus::Exited(_)));

        // Re-running target `a` reuses its tab (same id, fresh logs) — no third tab.
        let a2 = reg.start("a", "echo first", std::env::temp_dir()).unwrap();
        assert_eq!(a, a2);
        assert_eq!(reg.tabs().len(), 2);
        assert_eq!(reg.last_started(), Some(a));
        let snap = wait_for(&reg, a, |s| matches!(s.status, RunStatus::Exited(_)));
        assert_eq!(
            snap.logs.iter().filter(|l| l.text == "first").count(),
            1,
            "rerun must clear the previous run's lines"
        );
    }

    #[test]
    fn stop_kills_a_running_process() {
        let reg = RunRegistry::new();
        let id = reg.start("sleeper", "sleep 30", std::env::temp_dir()).expect("spawn");
        let snap = wait_for(&reg, id, |s| s.status.is_running());
        assert!(snap.status.is_running());
        assert_eq!(reg.overall_status(), Some(RunStatus::Running));
        assert!(reg.stop(id));
        assert_eq!(reg.snapshot(id).unwrap().status, RunStatus::Killed);
        assert_eq!(reg.overall_status(), Some(RunStatus::Killed));
        // Stopping again is a no-op.
        assert!(!reg.stop(id));
    }

    #[test]
    fn remove_closes_the_onglet_and_kills_a_live_run() {
        let reg = RunRegistry::new();
        let a = reg.start("a", "sleep 30", std::env::temp_dir()).unwrap();
        let b = reg.start("b", "echo done", std::env::temp_dir()).unwrap();
        wait_for(&reg, a, |s| s.status.is_running());

        assert!(reg.remove(a));
        assert!(reg.snapshot(a).is_none());
        assert_eq!(reg.tabs().len(), 1);
        // last_started falls back to a remaining run.
        assert_eq!(reg.last_started(), Some(b));
        assert!(!reg.remove(a));
    }

    #[test]
    fn tail_returns_the_last_n_lines_of_the_addressed_run() {
        let reg = RunRegistry::new();
        let id = reg
            .start("count", "for i in 1 2 3 4 5; do echo line-$i; done", std::env::temp_dir())
            .unwrap();
        wait_for(&reg, id, |s| matches!(s.status, RunStatus::Exited(_)));
        assert_eq!(reg.tail(id, 2), "line-4\nline-5");
        assert_eq!(reg.tail(id, 100).lines().count(), 5);
        assert_eq!(reg.find_by_command("for i in 1 2 3 4 5; do echo line-$i; done"), Some(id));
    }

    #[test]
    fn failed_spawn_reports_via_status() {
        let reg = RunRegistry::new();
        // `sh` exists but the cwd doesn't — spawn fails at the OS level.
        let err = reg.start("bad", "echo hi", PathBuf::from("/nonexistent-dir-xyz"));
        assert!(err.is_err());
        let tabs = reg.tabs();
        assert!(matches!(tabs[0].status, RunStatus::Failed(_)));
    }

    #[test]
    fn clear_logs_keeps_status() {
        let reg = RunRegistry::new();
        let id = reg.start("c", "echo data", std::env::temp_dir()).unwrap();
        let snap = wait_for(&reg, id, |s| matches!(s.status, RunStatus::Exited(_)));
        assert!(!snap.logs.is_empty());
        reg.clear_logs(id);
        let snap = reg.snapshot(id).unwrap();
        assert!(snap.logs.is_empty());
        assert_eq!(snap.status, RunStatus::Exited(0));
    }
}
