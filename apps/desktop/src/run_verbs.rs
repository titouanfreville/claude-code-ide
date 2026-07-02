//! The [`VerbExecutor`] behind the MCP **run verbs** — the bridge between a CC
//! session's `run_*` tool calls and the IDE's [`RunRegistry`](crate::run::RunRegistry)
//! (the same state the Run console renders, so agent and operator share one console).
//!
//! Composition: wraps the existing executor (`run_with_coverage`, …) and handles only
//! the `Run*` family itself. Policy/audit live a layer up in `ActorService`; this is
//! pure execution.
//!
//! **Containment:** `run_start` only launches a *detected* run target
//! ([`run_config::detect`](crate::views::run_config::detect) over the session's
//! attached root) — the same closed set the operator's ▶ widget offers. It is *not*
//! an arbitrary-command primitive: a payload that isn't a detected target is refused
//! with the valid choices, so the verb's `Standard` tier can't be leveraged into a
//! general shell.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use moonlight_domain::errors::ControlError;
use moonlight_domain::ids::SessionId;
use moonlight_domain::ports::mcp::VerbExecutor;
use moonlight_domain::trust::McpVerb;

use crate::run::RunRegistry;
use crate::views::run_config;

/// Default `run_logs` tail when the agent doesn't say (0 or unparsable payload).
const DEFAULT_TAIL: usize = 100;

pub struct RunVerbExecutor {
    registry: RunRegistry,
    /// Everything that isn't a `Run*` verb falls through here (e.g. the shell-backed
    /// `run_with_coverage`).
    inner: Arc<dyn VerbExecutor>,
}

impl RunVerbExecutor {
    pub fn new(registry: RunRegistry, inner: Arc<dyn VerbExecutor>) -> Self {
        Self { registry, inner }
    }

    /// The session's project root, required by every run verb that touches targets.
    fn require_root(root: Option<&Path>) -> Result<&Path, ControlError> {
        root.ok_or_else(|| ControlError::Unsupported("session has no attached project root".into()))
    }

    fn list_targets(root: &Path) -> String {
        let configs = run_config::detect(root);
        if configs.is_empty() {
            return "no run targets detected (no Cargo.toml / package.json marker)".into();
        }
        configs
            .iter()
            .map(|c| format!("{} — `{}`", c.label, c.command))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// All run onglets, one status line each (`run_status`).
    fn status(&self) -> String {
        let tabs = self.registry.tabs();
        if tabs.is_empty() {
            return "idle — nothing has been run yet".into();
        }
        tabs.iter()
            .map(|t| format!("`{}` — {}", t.command, t.status.label()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Resolve an optional `target` (the run's exact command) to its run id;
    /// empty → the most recently started run.
    fn resolve_run(&self, target: &str) -> Result<u64, ControlError> {
        let target = target.trim();
        if target.is_empty() {
            return self
                .registry
                .last_started()
                .ok_or_else(|| ControlError::Unsupported("nothing has been run yet".into()));
        }
        self.registry.find_by_command(target).ok_or_else(|| {
            ControlError::Unsupported(format!(
                "no run for target `{target}` — current runs:\n{}",
                self.status()
            ))
        })
    }

    fn start(&self, root: &Path, payload: &str) -> Result<String, ControlError> {
        let configs = run_config::detect(root);
        if configs.is_empty() {
            return Err(ControlError::Unsupported(
                "no run targets detected for this project".into(),
            ));
        }
        let wanted = payload.trim();
        // Only a *detected* target may launch (closed set — never arbitrary shell).
        let config = if wanted.is_empty() {
            configs.first()
        } else {
            configs.iter().find(|c| c.id() == wanted)
        };
        let Some(config) = config else {
            return Err(ControlError::Unsupported(format!(
                "unknown run target `{wanted}` — valid targets:\n{}",
                Self::list_targets(root)
            )));
        };
        self.registry
            .start(&config.label, &config.command, root.to_path_buf())
            .map_err(ControlError::Transport)?;
        Ok(format!(
            "started `{}` in its Run-console tab",
            config.command
        ))
    }

    /// Stop a run (`run_stop`): payload = the target's command, empty = the most
    /// recently started run.
    fn stop(&self, payload: &str) -> Result<String, ControlError> {
        let id = self.resolve_run(payload)?;
        if self.registry.stop(id) {
            Ok("stopped".into())
        } else {
            Ok(format!(
                "not running ({})",
                self.registry
                    .snapshot(id)
                    .map(|s| s.status.label())
                    .unwrap_or_else(|| "gone".into())
            ))
        }
    }

    /// Tail captured output (`run_logs`): payload = `"<target>\n<tail>"` — both
    /// optional (empty target = latest run; empty/0 tail = the default 100).
    fn logs(&self, payload: &str) -> Result<String, ControlError> {
        let (target, tail) = match payload.split_once('\n') {
            Some((t, n)) => (t, n.trim().parse::<usize>().ok().filter(|n| *n > 0)),
            // Back-compat: a bare number is a tail; anything else is a target.
            None => match payload.trim().parse::<usize>() {
                Ok(n) => ("", Some(n).filter(|n| *n > 0)),
                Err(_) => (payload.trim(), None),
            },
        };
        let id = self.resolve_run(target)?;
        let text = self.registry.tail(id, tail.unwrap_or(DEFAULT_TAIL));
        Ok(if text.is_empty() {
            "(no captured output)".into()
        } else {
            text
        })
    }
}

#[async_trait]
impl VerbExecutor for RunVerbExecutor {
    async fn execute(
        &self,
        session: &SessionId,
        root: Option<&Path>,
        verb: McpVerb,
        payload: &str,
    ) -> Result<String, ControlError> {
        match verb {
            McpVerb::RunListTargets => Ok(Self::list_targets(Self::require_root(root)?)),
            McpVerb::RunStatus => Ok(self.status()),
            McpVerb::RunLogs => self.logs(payload),
            McpVerb::RunStart => self.start(Self::require_root(root)?, payload),
            McpVerb::RunStop => self.stop(payload),
            other => self.inner.execute(session, root, other, payload).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    /// Inner executor that records delegation (non-Run verbs must fall through).
    struct EchoInner;
    #[async_trait]
    impl VerbExecutor for EchoInner {
        async fn execute(
            &self,
            _session: &SessionId,
            _root: Option<&Path>,
            verb: McpVerb,
            _payload: &str,
        ) -> Result<String, ControlError> {
            Ok(format!("inner:{verb:?}"))
        }
    }

    fn executor() -> RunVerbExecutor {
        RunVerbExecutor::new(RunRegistry::new(), Arc::new(EchoInner))
    }

    fn cargo_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mlc-rv-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
        dir
    }

    fn run(
        ex: &RunVerbExecutor,
        root: Option<&Path>,
        verb: McpVerb,
        payload: &str,
    ) -> Result<String, ControlError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(ex.execute(&SessionId::new("s1"), root, verb, payload))
    }

    #[test]
    fn list_targets_reports_detected_configs() {
        let root = cargo_root("list");
        let out = run(&executor(), Some(&root), McpVerb::RunListTargets, "").unwrap();
        assert!(out.contains("cargo run"), "{out}");
        assert!(out.contains("cargo test"), "{out}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn start_refuses_arbitrary_commands() {
        let root = cargo_root("contain");
        let err = run(
            &executor(),
            Some(&root),
            McpVerb::RunStart,
            "rm -rf / --no-preserve-root",
        )
        .unwrap_err();
        // Refused, and the agent is told the valid (closed) set.
        assert!(err.to_string().contains("unknown run target"), "{err}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn two_targets_run_as_two_onglets_and_are_addressable() {
        let root = cargo_root("round");
        let ex = RunVerbExecutor::new(RunRegistry::new(), Arc::new(EchoInner));
        // Empty payload = the project's first/default target (cargo run).
        let started = run(&ex, Some(&root), McpVerb::RunStart, "").unwrap();
        assert!(started.contains("cargo run"), "{started}");
        // A second target gets its own onglet (both listed in status).
        run(&ex, Some(&root), McpVerb::RunStart, "cargo test").unwrap();
        let status = run(&ex, Some(&root), McpVerb::RunStatus, "").unwrap();
        assert!(status.contains("cargo run"), "{status}");
        assert!(status.contains("cargo test"), "{status}");
        assert_eq!(status.lines().count(), 2, "{status}");
        // Logs/stop address a run by its command; empty = the latest started.
        let _ = run(&ex, Some(&root), McpVerb::RunLogs, "cargo run\n5").unwrap();
        let _ = run(&ex, Some(&root), McpVerb::RunStop, "cargo run").unwrap();
        let _ = run(&ex, Some(&root), McpVerb::RunStop, "").unwrap(); // latest = cargo test
                                                                      // An unknown target is refused with the current run list.
        let err = run(&ex, Some(&root), McpVerb::RunLogs, "made-up-cmd").unwrap_err();
        assert!(err.to_string().contains("no run for target"), "{err}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn run_verbs_require_an_attached_root_but_status_reads_do_not() {
        let ex = executor();
        assert!(run(&ex, None, McpVerb::RunListTargets, "").is_err());
        assert!(run(&ex, None, McpVerb::RunStart, "").is_err());
        // Status reads the (global) console — fine without a root.
        assert_eq!(
            run(&ex, None, McpVerb::RunStatus, "").unwrap(),
            "idle — nothing has been run yet"
        );
        // Logs before anything ran: there is no run to address.
        let err = run(&ex, None, McpVerb::RunLogs, "50").unwrap_err();
        assert!(err.to_string().contains("nothing has been run"), "{err}");
    }

    #[test]
    fn non_run_verbs_delegate_to_the_inner_executor() {
        let out = run(&executor(), None, McpVerb::RunWithCoverage, "").unwrap();
        assert_eq!(out, "inner:RunWithCoverage");
    }
}
