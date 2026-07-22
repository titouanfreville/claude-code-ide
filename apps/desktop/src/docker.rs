//! Docker / Compose probe + ops for the **Services** view.
//!
//! GPUI-free and tokio-free: a blocking `docker` CLI shell-out plus pure parsing of
//! its `--format '{{json .}}'` output, so the parser and command builders are unit-
//! testable without a daemon. The Services panel runs [`probe`] on a background-
//! executor task (never the foreground poll) and renders the result; operator ops
//! ([`run_op`] start/stop/restart, [`logs_command`] for the Run console) run the same
//! way. When the `docker` binary is absent or the daemon is unreachable the probe
//! reports [`DockerStatus::available`] = `false` and the section shows a calm hint.

use std::collections::BTreeMap;
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

/// One local image as the Images section shows it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Image {
    pub id: String,
    /// `repository:tag` (or `<none>:<none>` for dangling images).
    pub repo: String,
    pub tag: String,
    pub size: String,
    /// Docker's human "created" ago string, e.g. `3 weeks ago`.
    pub created: String,
}

impl Image {
    /// The display name `repository:tag`.
    pub fn name(&self) -> String {
        format!("{}:{}", self.repo, self.tag)
    }
}

/// One docker network.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Network {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub scope: String,
}

/// One docker volume (name + driver; the mountpoint is fetched lazily by inspect).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Volume {
    pub name: String,
    pub driver: String,
}

/// The whole probe result: whether docker answered, and the objects it listed.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DockerStatus {
    /// The `docker` CLI ran and the daemon answered (exit 0). `false` = not installed
    /// or daemon down — the section then shows a hint instead of rows.
    pub available: bool,
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
    pub networks: Vec<Network>,
    pub volumes: Vec<Volume>,
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

/// Shell `docker ps -a` and parse it — plus images / networks / volumes for the
/// Services tree. The container probe drives `available`: any spawn error (docker
/// absent) or non-zero exit (daemon down) yields an unavailable, empty status; the
/// other three are best-effort (a failure just leaves that list empty).
pub fn probe() -> DockerStatus {
    let output = Command::new("docker")
        .args(["ps", "-a", "--no-trunc", "--format", "{{json .}}"])
        .output();
    match output {
        Ok(out) if out.status.success() => DockerStatus {
            available: true,
            containers: parse_ps(&String::from_utf8_lossy(&out.stdout)),
            images: probe_lines(&["images", "--format", "{{json .}}"], parse_image),
            networks: probe_lines(&["network", "ls", "--format", "{{json .}}"], parse_network),
            volumes: probe_lines(&["volume", "ls", "--format", "{{json .}}"], parse_volume),
        },
        // Installed but daemon down, or not installed at all — same calm hint either way.
        _ => DockerStatus::default(),
    }
}

/// Run a `docker` list command emitting `{{json .}}` lines and parse each with `f`
/// (best-effort: any error → empty; unparseable lines skipped).
fn probe_lines<T>(args: &[&str], f: fn(&str) -> Option<T>) -> Vec<T> {
    let Ok(out) = Command::new("docker").args(args).output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(f)
        .collect()
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
                name: p
                    .names
                    .split(',')
                    .next()
                    .unwrap_or(&p.names)
                    .trim()
                    .to_string(),
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

#[derive(Deserialize, Default)]
struct ImageJson {
    #[serde(rename = "ID", default)]
    id: String,
    #[serde(rename = "Repository", default)]
    repository: String,
    #[serde(rename = "Tag", default)]
    tag: String,
    #[serde(rename = "Size", default)]
    size: String,
    #[serde(rename = "CreatedSince", default)]
    created_since: String,
}

/// Parse one `docker images --format '{{json .}}'` line.
fn parse_image(line: &str) -> Option<Image> {
    let j: ImageJson = serde_json::from_str(line).ok()?;
    Some(Image {
        id: j
            .id
            .strip_prefix("sha256:")
            .unwrap_or(&j.id)
            .chars()
            .take(12)
            .collect(),
        repo: if j.repository.is_empty() {
            "<none>".to_string()
        } else {
            j.repository
        },
        tag: if j.tag.is_empty() {
            "<none>".to_string()
        } else {
            j.tag
        },
        size: j.size,
        created: j.created_since,
    })
}

#[derive(Deserialize, Default)]
struct NetworkJson {
    #[serde(rename = "ID", default)]
    id: String,
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "Driver", default)]
    driver: String,
    #[serde(rename = "Scope", default)]
    scope: String,
}

/// Parse one `docker network ls --format '{{json .}}'` line.
fn parse_network(line: &str) -> Option<Network> {
    let j: NetworkJson = serde_json::from_str(line).ok()?;
    Some(Network {
        id: j.id.chars().take(12).collect(),
        name: j.name,
        driver: j.driver,
        scope: j.scope,
    })
}

#[derive(Deserialize, Default)]
struct VolumeJson {
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "Driver", default)]
    driver: String,
}

/// Parse one `docker volume ls --format '{{json .}}'` line.
fn parse_volume(line: &str) -> Option<Volume> {
    let j: VolumeJson = serde_json::from_str(line).ok()?;
    (!j.name.is_empty()).then_some(Volume {
        name: j.name,
        driver: j.driver,
    })
}

/// The host mountpoint of a volume (`docker volume inspect`), for the detail pane.
/// `None` on any error.
pub fn volume_mountpoint(name: &str) -> Option<String> {
    let out = Command::new("docker")
        .args(["volume", "inspect", "--format", "{{.Mountpoint}}", name])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
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

/// Snapshot the tail of a container's logs for the in-panel **Logs** tab (blocking).
/// stdout and stderr are merged (stderr appended) so a viewer sees everything; a spawn
/// failure returns the error as the body so the tab is never mysteriously blank.
pub fn logs_snapshot(id: &str, tail: usize) -> String {
    let output = Command::new("docker")
        .args(["logs", "--tail", &tail.to_string(), id])
        .output();
    match output {
        Ok(out) => {
            let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
            let err = String::from_utf8_lossy(&out.stderr);
            if !err.trim().is_empty() {
                if !s.is_empty() && !s.ends_with('\n') {
                    s.push('\n');
                }
                s.push_str(&err);
            }
            s
        }
        Err(e) => format!("failed to read logs: {e}"),
    }
}

/// Live resource usage for a container (one-shot `docker stats --no-stream`).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Stats {
    pub cpu_perc: String,
    pub mem_usage: String,
    pub mem_perc: String,
    pub net_io: String,
    pub block_io: String,
    pub pids: String,
}

#[derive(Deserialize, Default)]
struct StatsJson {
    #[serde(rename = "CPUPerc", default)]
    cpu_perc: String,
    #[serde(rename = "MemUsage", default)]
    mem_usage: String,
    #[serde(rename = "MemPerc", default)]
    mem_perc: String,
    #[serde(rename = "NetIO", default)]
    net_io: String,
    #[serde(rename = "BlockIO", default)]
    block_io: String,
    #[serde(rename = "PIDs", default)]
    pids: String,
}

/// One-shot resource stats for a running container. `None` when docker errors or the
/// container isn't running (stats only reports on running containers).
pub fn stats_snapshot(id: &str) -> Option<Stats> {
    let output = Command::new("docker")
        .args(["stats", "--no-stream", "--format", "{{json .}}", id])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_stats(&String::from_utf8_lossy(&output.stdout))
}

/// Parse the first JSON line of `docker stats --format '{{json .}}'`.
fn parse_stats(stdout: &str) -> Option<Stats> {
    let line = stdout.lines().find(|l| !l.trim().is_empty())?;
    let j: StatsJson = serde_json::from_str(line).ok()?;
    Some(Stats {
        cpu_perc: j.cpu_perc,
        mem_usage: j.mem_usage,
        mem_perc: j.mem_perc,
        net_io: j.net_io,
        block_io: j.block_io,
        pids: j.pids,
    })
}

/// The exact current configuration of a container, for the **Config** tab — the
/// live-inspected facts an operator wants to verify (ports, env, mounts, networks).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Inspect {
    pub created: String,
    pub restart_policy: String,
    pub command: String,
    /// Each exposed port and its host binding, e.g. `5432/tcp → 0.0.0.0:5432` or
    /// `6379/tcp (not published)`.
    pub ports: Vec<String>,
    pub env: Vec<String>,
    /// `source → destination (mode)` for each mount.
    pub mounts: Vec<String>,
    pub networks: Vec<String>,
}

#[derive(Deserialize, Default)]
struct InspectJson {
    #[serde(rename = "Created", default)]
    created: String,
    #[serde(rename = "Config", default)]
    config: InspectConfig,
    #[serde(rename = "HostConfig", default)]
    host_config: InspectHostConfig,
    #[serde(rename = "NetworkSettings", default)]
    network_settings: InspectNetwork,
    #[serde(rename = "Mounts", default)]
    mounts: Vec<InspectMount>,
}

#[derive(Deserialize, Default)]
struct InspectConfig {
    #[serde(rename = "Env", default)]
    env: Vec<String>,
    #[serde(rename = "Cmd", default)]
    cmd: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
struct InspectHostConfig {
    #[serde(rename = "RestartPolicy", default)]
    restart_policy: InspectRestartPolicy,
}

#[derive(Deserialize, Default)]
struct InspectRestartPolicy {
    #[serde(rename = "Name", default)]
    name: String,
}

#[derive(Deserialize, Default)]
struct InspectNetwork {
    #[serde(rename = "Ports", default)]
    ports: BTreeMap<String, Option<Vec<PortBinding>>>,
    #[serde(rename = "Networks", default)]
    networks: BTreeMap<String, serde::de::IgnoredAny>,
}

#[derive(Deserialize, Default)]
struct PortBinding {
    #[serde(rename = "HostIp", default)]
    host_ip: String,
    #[serde(rename = "HostPort", default)]
    host_port: String,
}

#[derive(Deserialize, Default)]
struct InspectMount {
    #[serde(rename = "Source", default)]
    source: String,
    #[serde(rename = "Destination", default)]
    destination: String,
    #[serde(rename = "Mode", default)]
    mode: String,
}

/// Inspect a container's live configuration. `None` when docker errors or the output
/// can't be parsed. The container's `image` lives on the [`Container`] already; this
/// adds the deeper facts (ports/env/mounts/networks/policy/command).
pub fn inspect_snapshot(id: &str) -> Option<Inspect> {
    let output = Command::new("docker").args(["inspect", id]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_inspect(&String::from_utf8_lossy(&output.stdout))
}

/// Parse `docker inspect <id>` (a one-element JSON array) into the [`Inspect`] facts.
fn parse_inspect(stdout: &str) -> Option<Inspect> {
    let arr: Vec<InspectJson> = serde_json::from_str(stdout).ok()?;
    let j = arr.into_iter().next()?;

    let ports = j
        .network_settings
        .ports
        .into_iter()
        .map(|(port, binds)| match binds {
            Some(bs) if !bs.is_empty() => {
                let hosts = bs
                    .iter()
                    .map(|b| {
                        let host = if b.host_ip.is_empty() {
                            "0.0.0.0"
                        } else {
                            &b.host_ip
                        };
                        format!("{host}:{}", b.host_port)
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{port} → {hosts}")
            }
            _ => format!("{port} (not published)"),
        })
        .collect();

    let mounts = j
        .mounts
        .into_iter()
        .map(|m| {
            let mode = if m.mode.is_empty() {
                String::new()
            } else {
                format!(" ({})", m.mode)
            };
            format!("{} → {}{mode}", m.source, m.destination)
        })
        .collect();

    Some(Inspect {
        created: j.created,
        restart_policy: {
            let n = j.host_config.restart_policy.name;
            if n.is_empty() {
                "no".to_string()
            } else {
                n
            }
        },
        command: j.config.cmd.unwrap_or_default().join(" "),
        ports,
        env: j.config.env,
        mounts,
        networks: j.network_settings.networks.into_keys().collect(),
    })
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

    #[test]
    fn parse_stats_reads_the_first_json_line() {
        let raw = r#"{"BlockIO":"0B / 0B","CPUPerc":"0.15%","Container":"abc","ID":"abc","MemPerc":"1.20%","MemUsage":"25MiB / 2GiB","Name":"db","NetIO":"1.2kB / 800B","PIDs":"7"}"#;
        let s = parse_stats(raw).expect("parses");
        assert_eq!(s.cpu_perc, "0.15%");
        assert_eq!(s.mem_usage, "25MiB / 2GiB");
        assert_eq!(s.mem_perc, "1.20%");
        assert_eq!(s.net_io, "1.2kB / 800B");
        assert_eq!(s.block_io, "0B / 0B");
        assert_eq!(s.pids, "7");
        assert!(parse_stats("").is_none(), "empty output → None");
    }

    #[test]
    fn parse_inspect_extracts_ports_env_mounts_networks() {
        let raw = r#"[{
            "Created":"2026-06-01T10:00:00Z",
            "Config":{"Image":"postgres:16","Env":["POSTGRES_PASSWORD=secret","TZ=UTC"],"Cmd":["postgres","-c","max_connections=200"]},
            "HostConfig":{"RestartPolicy":{"Name":"unless-stopped"}},
            "NetworkSettings":{
                "Ports":{"5432/tcp":[{"HostIp":"0.0.0.0","HostPort":"5432"}],"9999/tcp":null},
                "Networks":{"moonlight_default":{"IPAddress":"172.18.0.2"}}
            },
            "Mounts":[{"Source":"/var/lib/pg","Destination":"/var/lib/postgresql/data","Mode":"rw","Type":"volume"}]
        }]"#;
        let i = parse_inspect(raw).expect("parses");
        assert_eq!(i.created, "2026-06-01T10:00:00Z");
        assert_eq!(i.restart_policy, "unless-stopped");
        assert_eq!(i.command, "postgres -c max_connections=200");
        assert_eq!(i.env, vec!["POSTGRES_PASSWORD=secret", "TZ=UTC"]);
        // BTreeMap ordering makes ports deterministic: 5432 before 9999.
        assert_eq!(
            i.ports,
            vec!["5432/tcp → 0.0.0.0:5432", "9999/tcp (not published)"]
        );
        assert_eq!(
            i.mounts,
            vec!["/var/lib/pg → /var/lib/postgresql/data (rw)"]
        );
        assert_eq!(i.networks, vec!["moonlight_default"]);
    }

    #[test]
    fn parse_image_reads_name_size_and_shortens_id() {
        let line = r#"{"ID":"sha256:abc123def4567890","Repository":"postgres","Tag":"16","Size":"438MB","CreatedSince":"3 weeks ago"}"#;
        let i = parse_image(line).unwrap();
        assert_eq!(i.name(), "postgres:16");
        assert_eq!(i.id, "abc123def456");
        assert_eq!(i.size, "438MB");
        assert_eq!(i.created, "3 weeks ago");
        // Dangling image → <none>:<none>.
        let dangling = r#"{"ID":"x","Repository":"","Tag":"","Size":"1MB","CreatedSince":"now"}"#;
        assert_eq!(parse_image(dangling).unwrap().name(), "<none>:<none>");
    }

    #[test]
    fn parse_network_and_volume() {
        let n = parse_network(
            r#"{"ID":"9f0e1d2c3b4a5678","Name":"bridge","Driver":"bridge","Scope":"local"}"#,
        )
        .unwrap();
        assert_eq!(n.id, "9f0e1d2c3b4a");
        assert_eq!(n.name, "bridge");
        assert_eq!(n.driver, "bridge");

        let v = parse_volume(r#"{"Name":"pgdata","Driver":"local"}"#).unwrap();
        assert_eq!(v.name, "pgdata");
        assert_eq!(v.driver, "local");
        // Nameless volume line is skipped.
        assert!(parse_volume(r#"{"Name":"","Driver":"local"}"#).is_none());
    }

    #[test]
    fn parse_inspect_defaults_absent_restart_policy_to_no() {
        let raw = r#"[{"Created":"x","Config":{"Image":"i"},"HostConfig":{},"NetworkSettings":{},"Mounts":[]}]"#;
        let i = parse_inspect(raw).expect("parses");
        assert_eq!(i.restart_policy, "no");
        assert!(i.ports.is_empty());
        assert!(i.command.is_empty());
    }
}
