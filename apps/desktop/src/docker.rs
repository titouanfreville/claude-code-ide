//! Docker / Compose probe + ops for the **Services** view.
//!
//! GPUI-free and tokio-free: a blocking `docker` CLI shell-out plus pure parsing of
//! its `--format '{{json .}}'` output, so the parser and command builders are unit-
//! testable without a daemon. The Services panel runs [`probe`] on a background-
//! executor task (never the foreground poll) and renders the result; operator ops
//! ([`run_op`] start/stop/restart, [`logs_command`] for the Run console) run the same
//! way. When the `docker` binary is absent or the daemon is unreachable the probe
//! reports [`DockerStatus::available`] = `false` and the section shows a calm hint.

use std::process::Command;

use serde::Deserialize;

/// A start/stop/restart operation on one container (operator-initiated).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Start,
    Stop,
    Restart,
}

impl Op {
    fn verb(self) -> &'static str {
        match self {
            Op::Start => "start",
            Op::Stop => "stop",
            Op::Restart => "restart",
        }
    }
}

/// One container as the Services view shows it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    /// Docker's human status, e.g. `Up 2 hours` / `Exited (0) 3 minutes ago`.
    pub status: String,
    pub running: bool,
    /// Compacted published host ports, e.g. `5432, 6379` (empty when none).
    pub ports: String,
    /// The Compose project this container belongs to, if any (from labels).
    pub compose_project: Option<String>,
}

/// The whole probe result: whether docker answered, and the containers it listed.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DockerStatus {
    /// The `docker` CLI ran and the daemon answered (exit 0). `false` = not installed
    /// or daemon down — the section then shows a hint instead of rows.
    pub available: bool,
    pub containers: Vec<Container>,
}

/// One line of `docker ps --format '{{json .}}'`. Fields default so an older docker
/// that omits one (e.g. `State`) still parses.
#[derive(Deserialize, Default)]
struct PsJson {
    #[serde(rename = "ID", default)]
    id: String,
    #[serde(rename = "Names", default)]
    names: String,
    #[serde(rename = "Image", default)]
    image: String,
    #[serde(rename = "Status", default)]
    status: String,
    #[serde(rename = "State", default)]
    state: String,
    #[serde(rename = "Ports", default)]
    ports: String,
    #[serde(rename = "Labels", default)]
    labels: String,
}

/// Shell `docker ps -a --format '{{json .}}'` and parse it. Any spawn error (docker
/// absent) or non-zero exit (daemon down) yields an unavailable, empty status.
pub fn probe() -> DockerStatus {
    let output = Command::new("docker")
        .args(["ps", "-a", "--no-trunc", "--format", "{{json .}}"])
        .output();
    match output {
        Ok(out) if out.status.success() => DockerStatus {
            available: true,
            containers: parse_ps(&String::from_utf8_lossy(&out.stdout)),
        },
        // Installed but daemon down, or not installed at all — same calm hint either way.
        _ => DockerStatus::default(),
    }
}

/// Parse `docker ps` JSON-lines output into containers (one JSON object per line;
/// blank / unparseable lines are skipped).
fn parse_ps(stdout: &str) -> Vec<Container> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<PsJson>(line).ok())
        .map(|p| {
            let running = p.state.eq_ignore_ascii_case("running")
                || (p.state.is_empty() && p.status.starts_with("Up"));
            Container {
                // `Names` can be a comma list (rare); the first is the canonical name.
                name: p.names.split(',').next().unwrap_or(&p.names).trim().to_string(),
                id: p.id.chars().take(12).collect(),
                image: p.image,
                status: p.status,
                running,
                ports: compact_ports(&p.ports),
                compose_project: compose_project(&p.labels),
            }
        })
        .collect()
}

/// Reduce docker's verbose `Ports` field to a sorted, de-duplicated list of published
/// host ports. `0.0.0.0:5432->5432/tcp, :::5432->5432/tcp` → `5432`.
fn compact_ports(raw: &str) -> String {
    let mut ports: Vec<u32> = raw
        .split(',')
        .filter_map(|seg| {
            // Take the host side of `<host>:<port>-><container>` mappings only.
            let mapped = seg.split("->").next()?;
            let port = mapped.rsplit(':').next()?.trim();
            port.parse::<u32>().ok()
        })
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Extract the Compose project name from a container's label string
/// (`com.docker.compose.project=<name>,…`).
fn compose_project(labels: &str) -> Option<String> {
    labels
        .split(',')
        .find_map(|kv| kv.trim().strip_prefix("com.docker.compose.project="))
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

/// Run a start/stop/restart op on a container by id/name. Blocking; returns the
/// docker stderr on failure.
pub fn run_op(op: Op, id: &str) -> Result<(), String> {
    let output = Command::new("docker")
        .args([op.verb(), id])
        .output()
        .map_err(|e| format!("docker {}: {e}", op.verb()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// The command to stream a container's logs in the IDE Run console (`-f` follows).
pub fn logs_command(id: &str) -> String {
    format!("docker logs -f --tail 200 {id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"ID":"abc123def456","Names":"moonlight-db","Image":"postgres:16","Status":"Up 2 hours","State":"running","Ports":"0.0.0.0:5432->5432/tcp, :::5432->5432/tcp","Labels":"com.docker.compose.project=moonlight,com.docker.compose.service=db"}
{"ID":"99","Names":"broken-unterminated
{"ID":"def789","Names":"mailhog","Image":"mailhog/mailhog","Status":"Exited (0) 3 minutes ago","State":"exited","Ports":"","Labels":""}"#;

    #[test]
    fn parse_ps_reads_running_and_exited_and_skips_garbage() {
        let cs = parse_ps(SAMPLE);
        assert_eq!(cs.len(), 2, "the malformed middle line is skipped: {cs:?}");

        let db = &cs[0];
        assert_eq!(db.name, "moonlight-db");
        assert_eq!(db.image, "postgres:16");
        assert!(db.running);
        assert_eq!(db.ports, "5432");
        assert_eq!(db.compose_project.as_deref(), Some("moonlight"));
        assert_eq!(db.id, "abc123def456");

        let mh = &cs[1];
        assert_eq!(mh.name, "mailhog");
        assert!(!mh.running);
        assert_eq!(mh.ports, "");
        assert_eq!(mh.compose_project, None);
    }

    #[test]
    fn compact_ports_dedups_and_sorts_host_ports() {
        assert_eq!(
            compact_ports("0.0.0.0:6379->6379/tcp, :::6379->6379/tcp"),
            "6379"
        );
        assert_eq!(
            compact_ports("0.0.0.0:8080->80/tcp, 0.0.0.0:443->443/tcp"),
            "443, 8080"
        );
        assert_eq!(compact_ports(""), "");
        // An unpublished port (no host mapping) contributes nothing.
        assert_eq!(compact_ports("5432/tcp"), "");
    }

    #[test]
    fn falls_back_to_status_when_state_is_absent() {
        let line = r#"{"ID":"x","Names":"old","Image":"i","Status":"Up 5 minutes","Ports":"","Labels":""}"#;
        let cs = parse_ps(line);
        assert!(cs[0].running, "no State field → infer from 'Up' status");
    }

    #[test]
    fn op_verbs_and_logs_command_are_well_formed() {
        assert_eq!(Op::Start.verb(), "start");
        assert_eq!(Op::Stop.verb(), "stop");
        assert_eq!(Op::Restart.verb(), "restart");
        assert_eq!(logs_command("db"), "docker logs -f --tail 200 db");
    }
}
