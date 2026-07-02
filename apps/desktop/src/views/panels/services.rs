//! The **Services** tool window — modelled on JetBrains' Services view: a
//! **master–detail** split. The left pane is a compact, selectable **tree** of the
//! app's long-lived background services, grouped into collapsible categories (Docker
//! nests one level deeper, by Compose **stack**); the right pane is a **detail
//! inspector** that shows the full facts (endpoints, paths, ids) and live data for
//! whichever resource is selected. This keeps the tree scannable while the noisy
//! per-resource detail lives in one focused place instead of spilling inline.
//!
//! Every tree entry carries a **type icon** (coloured by health) so the categories
//! read at a glance; the Docker container inspector is a small **tabbed** surface —
//! **Logs** (open by default), **Stats**, and **Config** (the exact live configuration
//! — ports, env, mounts, networks) — with start/stop/restart actions pinned alongside.
//!
//! Categories:
//! - **Control server** (hook IPC): probed by connecting to its unix socket. Green =
//!   something answers — this instance's server *or* another instance's (the spawn
//!   refuses to hijack a live socket), which is exactly what the hook CLI sees.
//! - **MCP servers**: the embedded per-session `moonlight` hosts — runtime up/down,
//!   the live HTTP endpoints, each labelled with its session's **name** and a **live
//!   verb-call log** read from the durable audit store.
//! - **Docker**: containers grouped by Compose stack, with start/stop/restart, live
//!   logs, resource stats, and the inspected configuration.
//! - **HTTP**: the `http_request` call history (active env + recent results).
//!
//! Like the terminal and Run console this is a **workspace-owned bottom tool** (plain
//! `Render` entity, not a dock `Panel`): the bottom dock renders it when the stripe's
//! ⚙ fronts it. Cheap local state refreshes on a 2s poll; Docker has its own 3s
//! background-executor poll that also refreshes the open container's Logs/Stats tab.
//! Re-render only fires when a snapshot actually changed. Collapse + selection + the
//! active Docker tab are operator state that survive the poll.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    div, px, svg, AnyElement, App, ClickEvent, Context, Div, FontWeight, Hsla, Pixels, SharedString,
    Stateful, Window,
};

use moonlight_domain::audit::{AuditAction, AuditEntry};
use moonlight_domain::ports::ManagedSessionStore;
use moonlight_domain::session::SessionStatus;
use moonlight_domain::trust::McpVerb;

use crate::docker;
use crate::mcp_activity;
use crate::views::center_requests::OpenRequest;
use crate::views::chrome_requests::ChromeRequest;
use crate::views::theme;
use crate::views::workspace::ShellDeps;

/// How many recent audit rows to surface per MCP session as its verb-call log. The
/// detail pane shows the full tail, so this is generous (the tree shows none).
const LOG_LINES: usize = 24;
/// Cap a log line so a verbose summary can't blow the row width.
const LOG_CLIP: usize = 96;
/// Fixed row height for every tree row (JetBrains Services density).
const ROW_H: f32 = 24.;
/// Per-depth indent step; the disclosure/status slot is [`SLOT_W`] wide.
const INDENT_STEP: f32 = 14.;
const SLOT_W: f32 = 13.;
/// Width of the master (tree) pane; the detail pane takes the rest.
const TREE_W: f32 = 300.;
/// Tail length of the in-panel container log snapshot.
const DOCKER_LOG_TAIL: usize = 400;

/// A tree/detail entry icon: either a monochrome unicode glyph or an embedded SVG mark
/// (asset path). Both render tinted by the entry's health colour.
#[derive(Clone, Copy)]
enum Ico {
    Glyph(&'static str),
    Svg(&'static str),
}

// Type icons — one mark per entry kind, tinted by health. Docker and MCP use their
// real vector logos (served by `crate::assets`); the rest are calm geometric glyphs.
const IC_CONTROL: Ico = Ico::Glyph("⇄");
const IC_MCP: Ico = Ico::Svg("icons/mcp.svg");
const IC_DOCKER: Ico = Ico::Svg("icons/docker.svg");
const IC_COMPOSE: Ico = Ico::Svg("icons/layers.svg");
const IC_CONTAINERS: Ico = Ico::Svg("icons/container.svg");
/// Individual containers + images wear the Docker whale (the logo at object level).
const IC_CONTAINER: Ico = Ico::Svg("icons/docker.svg");
const IC_IMAGES: Ico = Ico::Svg("icons/boxes.svg");
const IC_IMAGE: Ico = Ico::Svg("icons/docker.svg");
const IC_NETWORK: Ico = Ico::Svg("icons/network.svg");
const IC_VOLUME: Ico = Ico::Svg("icons/hard-drive.svg");
const IC_HTTP: Ico = Ico::Glyph("⇅");

/// One audit row, pre-classified for the verb-call log (kept timestamp-raw so the
/// snapshot only changes when real rows do — relative "ago" is computed at render).
#[derive(Clone, PartialEq, Eq)]
struct LogLine {
    at_millis: i64,
    kind: LogKind,
    text: String,
}

/// The visual class of a log line (drives its glyph + colour).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LogKind {
    Ran,
    Failed,
    Denied,
    Approved,
    Phase,
    Feedback,
    Revert,
}

/// One live embedded MCP server: its session, friendly name, endpoint, and the tail
/// of its verb-call audit log.
#[derive(Clone, PartialEq, Eq)]
struct McpEndpoint {
    short_id: String,
    name: Option<String>,
    url: String,
    logs: Vec<LogLine>,
}

/// What the panel knows about the services this frame (rebuilt by the poll).
#[derive(Clone, Default, PartialEq, Eq)]
struct Snapshot {
    /// The control socket answers a connect.
    socket_ok: bool,
    /// The MCP host runtime is up (`ShellDeps.mcp_host` is `Some`).
    mcp_up: bool,
    /// Live per-session MCP endpoints with names + verb logs.
    endpoints: Vec<McpEndpoint>,
    /// Active HTTP environment name (active space's `.moonlight/http` manifest).
    http_env: Option<String>,
    /// Total recorded HTTP calls.
    http_total: usize,
    /// The most recent HTTP calls (newest first) for the compact summary.
    http_calls: Vec<crate::http::HttpCall>,
}

/// Which resource the detail pane is inspecting. Identifiers are stable across polls
/// (MCP short id / Docker container id), so selection survives a snapshot refresh; if
/// the resource vanishes the detail pane shows a graceful "gone" state.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
enum Selection {
    #[default]
    Control,
    Mcp(String),
    Docker(String),
    DockerImage(String),
    DockerNetwork(String),
    DockerVolume(String),
    Http,
}

/// The active tab of the Docker container inspector.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum DockerTab {
    #[default]
    Logs,
    Stats,
    Config,
}

pub struct ServicesPanel {
    socket: PathBuf,
    snapshot: Snapshot,
    /// Latest Docker probe (its own background-executor poll — the CLI shell-out must
    /// never run on the foreground render/poll thread).
    docker: docker::DockerStatus,
    /// The resource whose detail is shown in the right pane.
    selected: Selection,
    /// Section collapse flags (operator state; default expanded).
    mcp_open: bool,
    docker_open: bool,
    /// Compose stacks the operator has collapsed (default: all expanded).
    stacks_closed: HashSet<String>,
    /// Fixed Docker subgroup collapse (Containers / Images / Networks / Volumes),
    /// keyed by section id. Default open = Containers; the rest start collapsed.
    docker_sections_closed: HashSet<String>,
    /// Lazily-fetched host mountpoint for the selected volume, keyed by name.
    detail_volume_mount: Option<(String, Option<String>)>,
    /// Active tab of the Docker container inspector.
    docker_tab: DockerTab,
    /// Fetched Logs/Stats/Config for the selected container, keyed by its id so a
    /// stale fetch for a previous selection is ignored (shows "loading…").
    detail_logs: Option<(String, String)>,
    detail_stats: Option<(String, Option<docker::Stats>)>,
    detail_inspect: Option<(String, Option<docker::Inspect>)>,
    /// Per-MCP-server activity mined from the session transcripts (its own slow poll).
    mcp_servers: Vec<mcp_activity::McpServerView>,
}

impl ServicesPanel {
    pub fn new(socket: PathBuf, cx: &mut Context<Self>) -> Self {
        // 2s poll: probe the socket + snapshot the MCP endpoints (name + audit tail);
        // notify only on change. All reads are local (unix connect + SQLite, sub-ms).
        cx.spawn(async move |this, cx| loop {
            let alive = this.update(cx, |panel: &mut Self, cx| {
                let next = build_snapshot(&panel.socket, cx);
                if next != panel.snapshot {
                    panel.snapshot = next;
                    cx.notify();
                }
            });
            if alive.is_err() {
                break; // panel dropped
            }
            cx.background_executor().timer(Duration::from_secs(2)).await;
        })
        .detach();

        // Docker poll (3s) — the `docker` shell-out runs on a background-executor task
        // so a slow/hung daemon can't jank the UI; the foreground only diffs + notifies.
        // While a container is selected with the Logs/Stats tab open, this also refreshes
        // that live data so the inspector stays current.
        cx.spawn(async move |this, cx| loop {
            let next = cx
                .background_executor()
                .spawn(async { docker::probe() })
                .await;
            let alive = this.update(cx, |panel: &mut Self, cx| {
                if next != panel.docker {
                    panel.docker = next;
                    cx.notify();
                }
                panel.autorefresh_docker(cx);
            });
            if alive.is_err() {
                break;
            }
            cx.background_executor().timer(Duration::from_secs(3)).await;
        })
        .detach();

        // MCP activity poll (4s) — mine each managed session's transcript for
        // `mcp__<server>__<tool>` calls and group by server. Transcript reads can be
        // large, so this runs on the background executor with a re-parse cache keyed by
        // file (mtime, len): only changed transcripts are re-read. The cache lives in
        // the task, moved in and back out across each iteration.
        cx.spawn(async move |this, cx| {
            let mut cache: mcp_activity::ScanCache = std::collections::HashMap::new();
            loop {
                let gathered =
                    this.update(cx, |_p: &mut Self, cx| gather_mcp_inputs(cx));
                let Ok(gathered) = gathered else {
                    break; // panel dropped
                };
                if let Some((sessions, roots)) = gathered {
                    let mut owned = std::mem::take(&mut cache);
                    let (servers, returned) = cx
                        .background_executor()
                        .spawn(async move {
                            let s = mcp_activity::build(&sessions, &roots, &mut owned);
                            (s, owned)
                        })
                        .await;
                    cache = returned;
                    let alive = this.update(cx, |panel: &mut Self, cx| {
                        if panel.mcp_servers != servers {
                            panel.mcp_servers = servers;
                            cx.notify();
                        }
                    });
                    if alive.is_err() {
                        break;
                    }
                }
                cx.background_executor().timer(Duration::from_secs(4)).await;
            }
        })
        .detach();

        Self {
            socket,
            snapshot: Snapshot::default(),
            docker: docker::DockerStatus::default(),
            selected: Selection::Control,
            mcp_open: true,
            docker_open: true,
            stacks_closed: HashSet::new(),
            // Images / Networks / Volumes start collapsed to keep the tree tidy.
            docker_sections_closed: ["images", "networks", "volumes"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            detail_volume_mount: None,
            docker_tab: DockerTab::Logs,
            detail_logs: None,
            detail_stats: None,
            detail_inspect: None,
            mcp_servers: Vec::new(),
        }
    }

    /// Select a resource for the detail pane. Selecting a container resets the
    /// inspector to the **Logs** tab and kicks off its fetch, so logs are visible the
    /// moment you open a container.
    fn select(&mut self, sel: Selection, cx: &mut Context<Self>) {
        if self.selected == sel {
            return;
        }
        self.selected = sel;
        cx.notify();
        if matches!(self.selected, Selection::Docker(_)) {
            self.docker_tab = DockerTab::Logs;
            self.refresh_docker_detail(cx);
        }
        if let Selection::DockerVolume(name) = &self.selected {
            self.fetch_volume_mount(name.clone(), cx);
        }
    }

    /// Switch the Docker inspector tab and fetch that tab's data if we don't have it.
    fn set_docker_tab(&mut self, tab: DockerTab, cx: &mut Context<Self>) {
        if self.docker_tab != tab {
            self.docker_tab = tab;
            cx.notify();
            self.refresh_docker_detail(cx);
        }
    }

    /// Collapse / expand a Compose stack subgroup.
    fn toggle_stack(&mut self, name: String, cx: &mut Context<Self>) {
        if !self.stacks_closed.remove(&name) {
            self.stacks_closed.insert(name);
        }
        cx.notify();
    }

    /// Collapse / expand a fixed Docker subgroup (Containers/Images/Networks/Volumes).
    fn toggle_docker_section(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.docker_sections_closed.remove(key) {
            self.docker_sections_closed.insert(key.to_string());
        }
        cx.notify();
    }

    /// Fetch a volume's host mountpoint off-thread for the Volume detail pane.
    fn fetch_volume_mount(&self, name: String, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mount = cx
                .background_executor()
                .spawn({
                    let name = name.clone();
                    async move { docker::volume_mountpoint(&name) }
                })
                .await;
            let _ = this.update(cx, |p, cx| {
                p.detail_volume_mount = Some((name, mount));
                cx.notify();
            });
        })
        .detach();
    }

    /// Fetch the active Docker tab's data for the selected container off-thread, then
    /// store it (keyed by id so a late reply for an old selection is dropped at render).
    fn refresh_docker_detail(&self, cx: &mut Context<Self>) {
        let Selection::Docker(id) = self.selected.clone() else {
            return;
        };
        match self.docker_tab {
            DockerTab::Logs => {
                cx.spawn(async move |this, cx| {
                    let text = cx
                        .background_executor()
                        .spawn({
                            let id = id.clone();
                            async move { docker::logs_snapshot(&id, DOCKER_LOG_TAIL) }
                        })
                        .await;
                    let _ = this.update(cx, |p, cx| {
                        p.detail_logs = Some((id, text));
                        cx.notify();
                    });
                })
                .detach();
            }
            DockerTab::Stats => {
                cx.spawn(async move |this, cx| {
                    let stats = cx
                        .background_executor()
                        .spawn({
                            let id = id.clone();
                            async move { docker::stats_snapshot(&id) }
                        })
                        .await;
                    let _ = this.update(cx, |p, cx| {
                        p.detail_stats = Some((id, stats));
                        cx.notify();
                    });
                })
                .detach();
            }
            DockerTab::Config => {
                cx.spawn(async move |this, cx| {
                    let inspect = cx
                        .background_executor()
                        .spawn({
                            let id = id.clone();
                            async move { docker::inspect_snapshot(&id) }
                        })
                        .await;
                    let _ = this.update(cx, |p, cx| {
                        p.detail_inspect = Some((id, inspect));
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }

    /// Piggyback on the 3s Docker poll to keep the open container's **live** tabs
    /// (Logs, Stats) current. Config is static, so it's fetched on demand only.
    fn autorefresh_docker(&self, cx: &mut Context<Self>) {
        if matches!(self.selected, Selection::Docker(_))
            && matches!(self.docker_tab, DockerTab::Logs | DockerTab::Stats)
        {
            self.refresh_docker_detail(cx);
        }
    }

    /// Fire an operator Docker op (start/stop/restart) off-thread, then immediately
    /// re-probe so the row's lamp/status reflects the change without waiting for the
    /// 3s poll.
    fn docker_op(&self, op: docker::Op, id: &str, cx: &mut Context<Self>) {
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let _ = cx
                .background_executor()
                .spawn(async move { docker::run_op(op, &id) })
                .await;
            let next = cx
                .background_executor()
                .spawn(async { docker::probe() })
                .await;
            let _ = this.update(cx, |panel: &mut Self, cx| {
                panel.docker = next;
                cx.notify();
                // The op changed running state — refresh the open tab too.
                panel.refresh_docker_detail(cx);
            });
        })
        .detach();
    }

    /// Stream a container's logs into the shared Run console (the operator can switch
    /// to the ⊳ Run tool window to read/stop them).
    fn docker_logs(&self, id: &str, cx: &mut Context<Self>) {
        let Some(deps) = cx.try_global::<ShellDeps>() else {
            return;
        };
        let run_registry = deps.run_registry.clone();
        let root = deps.focus.read(cx).root();
        let _ = run_registry.start(
            &format!("docker logs {id}"),
            &docker::logs_command(id),
            root,
        );
    }
}

/// Build the current services snapshot from the shell globals (MCP host + store).
fn build_snapshot(socket: &Path, cx: &gpui::App) -> Snapshot {
    let store = shell_store(cx);
    let endpoints = mcp_handle(cx)
        .map(|h| {
            h.endpoints()
                .into_iter()
                .map(|(id, url)| McpEndpoint {
                    short_id: short_id(id.as_str()),
                    name: store
                        .as_ref()
                        .and_then(|s| s.managed(&id).ok().flatten())
                        .and_then(|m| m.title),
                    url,
                    logs: store
                        .as_ref()
                        .and_then(|s| s.recent_audit(&id, LOG_LINES).ok())
                        .unwrap_or_default()
                        .iter()
                        .map(log_line)
                        .collect(),
                })
                .collect()
        })
        .unwrap_or_default();
    // HTTP: the active space's manifest env name + the shared call history tail.
    let deps = cx.try_global::<ShellDeps>();
    let http_env = deps
        .map(|d| d.focus.read(cx).root())
        .map(|root| crate::http::load_manifest(&root))
        .and_then(|m| m.active_name());
    let (http_total, http_calls) = deps
        .map(|d| (d.http_history.len(), d.http_history.recent(8)))
        .unwrap_or((0, Vec::new()));

    Snapshot {
        socket_ok: probe_socket(socket),
        mcp_up: mcp_handle(cx).is_some(),
        endpoints,
        http_env,
        http_total,
        http_calls,
    }
}

/// Gather the inputs for the MCP-activity scan from the shell globals: the managed
/// sessions `(full_id, title)` whose transcripts to mine, and the project roots to
/// check for configured (accessible) MCP servers. `None` when the shell isn't
/// installed or the store can't be read. Cheap (in-memory store read); the heavy
/// transcript/config I/O happens off-thread in [`mcp_activity::build`].
fn gather_mcp_inputs(cx: &gpui::App) -> Option<(Vec<(String, Option<String>)>, Vec<PathBuf>)> {
    let deps = cx.try_global::<ShellDeps>()?;
    let managed = deps.store.all_managed().ok()?;
    let mut sessions = Vec::with_capacity(managed.len());
    let mut roots: Vec<PathBuf> = Vec::new();
    for m in &managed {
        sessions.push((m.id.as_str().to_string(), m.title.clone()));
        if let Some(r) = &m.root {
            let p = PathBuf::from(r);
            if !roots.contains(&p) {
                roots.push(p);
            }
        }
    }
    let froot = deps.focus.read(cx).root();
    if !roots.contains(&froot) {
        roots.push(froot);
    }
    Some((sessions, roots))
}

/// The MCP host handle from the shell global (`None` in static/test views or when
/// the host runtime failed to start).
fn mcp_handle(cx: &gpui::App) -> Option<crate::views::mcp_host::McpHostHandle> {
    cx.try_global::<ShellDeps>()?.mcp_host.clone()
}

/// The durable managed-session store from the shell global (for names + audit logs).
fn shell_store(cx: &gpui::App) -> Option<Arc<dyn ManagedSessionStore>> {
    Some(cx.try_global::<ShellDeps>()?.store.clone())
}

/// `true` when a connect on the unix socket at `path` succeeds — exactly what the
/// hook CLI experiences. (macOS may still accept briefly on a freshly-dead listener's
/// file, so this is "reachable now", not a liveness proof.)
fn probe_socket(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

/// First 8 chars of a session id — enough to match the tile/tab.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Wall-clock millis now (no chrono dep; mirrors `workspace::now_ms`).
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Compact relative age ("now", "12s", "3m", "2h", "5d") from two epoch-millis.
fn rel_ago(now: i64, at: i64) -> String {
    let secs = ((now - at) / 1000).max(0);
    if secs < 2 {
        "now".into()
    } else if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// The MCP tool name for a verb (nicer than the Debug enum name in the log).
fn verb_label(v: McpVerb) -> &'static str {
    match v {
        McpVerb::RunWithCoverage => "run_with_coverage",
        McpVerb::QueryDb => "query_db",
        McpVerb::HttpRequest => "http_request",
        McpVerb::OpenReview => "open_review",
        McpVerb::StartDebug => "start_debug",
        McpVerb::RunStart => "run_start",
        McpVerb::RunStop => "run_stop",
        McpVerb::RunStatus => "run_status",
        McpVerb::RunLogs => "run_logs",
        McpVerb::RunListTargets => "run_list_targets",
        McpVerb::RequestPhase => "request_phase",
        McpVerb::ReportBlocked => "report_blocked",
        McpVerb::PhaseStatus => "phase_status",
        McpVerb::PresentPlan => "present_plan",
    }
}

/// Truncate `s` to at most `max` chars, appending an ellipsis when clipped.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    } else {
        s.to_string()
    }
}

/// Group containers by Compose stack for the tree: `(stack, members)` pairs sorted by
/// stack name, then the standalone (no-project) containers. Ordering is deterministic
/// so the tree never jitters between polls.
fn stack_groups(containers: &[docker::Container]) -> (Vec<(String, Vec<&docker::Container>)>, Vec<&docker::Container>) {
    use std::collections::BTreeMap;
    let mut stacks: BTreeMap<String, Vec<&docker::Container>> = BTreeMap::new();
    let mut standalone: Vec<&docker::Container> = Vec::new();
    for c in containers {
        match &c.compose_project {
            Some(p) => stacks.entry(p.clone()).or_default().push(c),
            None => standalone.push(c),
        }
    }
    (stacks.into_iter().collect(), standalone)
}

/// Classify one durable audit entry into a verb-call log line.
fn log_line(entry: &AuditEntry) -> LogLine {
    let (kind, text) = match &entry.action {
        AuditAction::VerbExecuted { verb, summary } => {
            let failed = summary.to_ascii_lowercase().starts_with("failed");
            (
                if failed {
                    LogKind::Failed
                } else {
                    LogKind::Ran
                },
                format!("{} · {summary}", verb_label(*verb)),
            )
        }
        AuditAction::Denied { what, reason } => (LogKind::Denied, format!("{what} — {reason}")),
        AuditAction::Approved { what } => (LogKind::Approved, format!("approved · {what}")),
        AuditAction::Reverted { reverts } => (LogKind::Revert, format!("reverted {reverts}")),
        AuditAction::FeedbackInjected { message } => {
            (LogKind::Feedback, format!("feedback · {message}"))
        }
        AuditAction::PhaseChanged { to } => (LogKind::Phase, format!("phase → {}", to.label())),
    };
    LogLine {
        at_millis: entry.at.as_millis(),
        kind,
        text: clip(&text, LOG_CLIP),
    }
}

/// Glyph for a log-line kind.
fn kind_glyph(k: LogKind) -> &'static str {
    match k {
        LogKind::Ran | LogKind::Approved => "✓",
        LogKind::Failed | LogKind::Denied => "✘",
        LogKind::Phase => "◆",
        LogKind::Feedback => "➤",
        LogKind::Revert => "⟲",
    }
}

/// Colour for a log-line kind.
fn kind_color(k: LogKind) -> Hsla {
    match k {
        LogKind::Ran | LogKind::Approved => theme::status_color(SessionStatus::Done),
        LogKind::Failed | LogKind::Denied => theme::status_color(SessionStatus::Errored),
        LogKind::Phase => theme::status_color(SessionStatus::Running),
        LogKind::Feedback => theme::text_secondary(),
        LogKind::Revert => theme::text_muted(),
    }
}

// ── Tree-row chrome (JetBrains Services tree: fixed-height rows, aligned slots) ─

/// A tree row at `depth`: fixed height + leading indent so the disclosure triangles
/// and type icons of nested rows line up under their parent. Callers chain
/// `.id()/.hover()/.on_click()/.child()`.
fn row(depth: usize) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .h(px(ROW_H))
        .pl(px(6. + depth as f32 * INDENT_STEP))
        .pr_2()
        .gap_1p5()
}

/// The disclosure triangle in the leading slot (so labels align open or closed).
fn disclosure(open: bool) -> impl IntoElement {
    div()
        .w(px(SLOT_W))
        .flex_none()
        .text_size(theme::text_xs())
        .text_color(theme::tree_glyph())
        .child(if open { "▾" } else { "▸" })
}

/// Render an icon (glyph or SVG mark) at `size`, tinted `color`. GPUI paints an SVG as
/// a single-colour mask, so the logos adopt the status colour like the glyphs do.
fn ico_el(ico: Ico, color: Hsla, size: Pixels) -> AnyElement {
    match ico {
        Ico::Glyph(g) => div()
            .flex_none()
            .text_size(size)
            .text_color(color)
            .child(g)
            .into_any_element(),
        Ico::Svg(path) => svg()
            .flex_none()
            .size(size)
            .path(path)
            .text_color(color)
            .into_any_element(),
    }
}

/// A type icon occupying the leading slot, tinted by health/status. Replaces the plain
/// status dot: the icon tells you *what* the entry is, the colour tells you its state.
fn icon_slot(ico: Ico, color: Hsla) -> impl IntoElement {
    div()
        .w(px(SLOT_W))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(ico_el(ico, color, px(12.)))
}

/// A category (group) header label — medium weight, the JetBrains node style.
fn group_label(text: &str) -> impl IntoElement {
    div()
        .text_size(theme::text_base())
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_primary())
        .child(text.to_string())
}

/// A right-aligned count / status chip on a row.
fn chip(text: String, color: Hsla) -> impl IntoElement {
    div()
        .flex_none()
        .text_size(theme::text_xs())
        .text_color(color)
        .child(text)
}

/// A leaf row's primary name.
fn leaf_name(text: String) -> impl IntoElement {
    div()
        .text_size(theme::text_sm())
        .text_color(theme::text_primary())
        .child(text)
}

/// A quiet hint row at `depth` (empty states).
fn hint_row(depth: usize, text: &str) -> impl IntoElement {
    row(depth).child(div().w(px(SLOT_W)).flex_none()).child(
        div()
            .text_size(theme::text_xs())
            .text_color(theme::text_muted())
            .child(text.to_string()),
    )
}

/// A selectable leaf in the master tree: a type icon then whatever the caller adds
/// (name + right-aligned chip). `leading_blank` inserts a blank disclosure slot (for
/// top-level leaves, so their icon lines up with a group's icon). Selection fills the
/// row with the accent wash; otherwise it hover-lights.
fn nav_leaf(
    id: SharedString,
    depth: usize,
    leading_blank: bool,
    selected: bool,
    icon: Ico,
    color: Hsla,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let mut r = row(depth)
        .id(id)
        .rounded(theme::radius_sm())
        .cursor_pointer()
        .on_click(on_click);
    r = if selected {
        r.bg(theme::row_selected())
    } else {
        r.hover(|d| d.bg(theme::row_hover()))
    };
    if leading_blank {
        r = r.child(div().w(px(SLOT_W)).flex_none());
    }
    r.child(icon_slot(icon, color))
}

impl ServicesPanel {
    // ── Master pane (the selectable resource tree) ────────────────────────────

    /// Control server — a top-level singleton leaf (no children to expand).
    fn control_leaf(&self, ok: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let status = if ok {
            theme::status_color(SessionStatus::Done)
        } else {
            theme::status_color(SessionStatus::Errored)
        };
        nav_leaf(
            "svc-nav-control".into(),
            0,
            true,
            self.selected == Selection::Control,
            IC_CONTROL,
            status,
            cx.listener(|this, _e, _w, cx| this.select(Selection::Control, cx)),
        )
        .child(group_label("Control server"))
        .child(div().flex_1())
        .child(chip(
            if ok { "listening" } else { "down" }.into(),
            status,
        ))
    }

    /// The MCP servers category: a collapsible header (server count + total calls)
    /// over one selectable leaf **per MCP server** — grouped by server, not by session.
    /// Each server is coloured green when it has activity, muted when merely accessible.
    fn mcp_group(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.mcp_open;
        let servers = &self.mcp_servers;
        let total_calls: usize = servers.iter().map(|s| s.total_calls).sum();
        let dot = if total_calls > 0 {
            theme::status_color(SessionStatus::Done)
        } else {
            theme::status_color(SessionStatus::Idle)
        };
        group(
            "svc-sec-mcp",
            open,
            IC_MCP,
            dot,
            "MCP servers",
            format!(
                "{} server{} · {total_calls} call{}",
                servers.len(),
                if servers.len() == 1 { "" } else { "s" },
                if total_calls == 1 { "" } else { "s" },
            ),
            cx.listener(|this, _ev, _w, cx| {
                this.mcp_open = !this.mcp_open;
                cx.notify();
            }),
        )
        .when(open, |d| {
            d.when(servers.is_empty(), |d| {
                d.child(hint_row(1, "no MCP servers yet"))
            })
            .children(servers.iter().map(|s| {
                let name = s.server.clone();
                let active = s.total_calls > 0;
                let dot = if active {
                    theme::status_color(SessionStatus::Done)
                } else {
                    theme::status_color(SessionStatus::Idle)
                };
                let chip_text = if active {
                    format!("{} call{}", s.total_calls, if s.total_calls == 1 { "" } else { "s" })
                } else {
                    "idle".to_string()
                };
                nav_leaf(
                    SharedString::from(format!("svc-nav-mcp-{name}")),
                    1,
                    false,
                    self.selected == Selection::Mcp(name.clone()),
                    IC_MCP,
                    dot,
                    cx.listener({
                        let name = name.clone();
                        move |this, _e, _w, cx| this.select(Selection::Mcp(name.clone()), cx)
                    }),
                )
                .child(leaf_name(s.server.clone()))
                .child(div().flex_1())
                .child(chip(chip_text, theme::text_muted()))
            }))
        })
    }

    /// The Docker category: a collapsible header (running/total) whose children are
    /// the Compose **stack** subgroups, a **Containers** group for standalone
    /// containers, then **Images**, **Networks** and **Volumes** sections.
    fn docker_group(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.docker_open;
        let status = self.docker.clone();
        let running = status.containers.iter().filter(|c| c.running).count();
        let (dot, count_text) = if !status.available {
            (
                theme::status_color(SessionStatus::Errored),
                "unavailable".to_string(),
            )
        } else if running > 0 {
            (
                theme::status_color(SessionStatus::Done),
                format!("{running}/{} up", status.containers.len()),
            )
        } else {
            (
                theme::status_color(SessionStatus::Idle),
                format!("0/{} up", status.containers.len()),
            )
        };
        let (stacks, standalone) = stack_groups(&status.containers);
        group(
            "svc-sec-docker",
            open,
            IC_DOCKER,
            dot,
            "Docker",
            count_text,
            cx.listener(|this, _ev, _w, cx| {
                this.docker_open = !this.docker_open;
                cx.notify();
            }),
        )
        .when(open, |d| {
            d.when(!status.available, |d| {
                d.child(hint_row(1, "docker not available"))
            })
            .children(
                stacks
                    .into_iter()
                    .map(|(name, members)| self.stack_subgroup(name, members, cx)),
            )
            .when(status.available, |d| {
                d.child(self.containers_section(standalone, cx))
                    .child(self.images_section(cx))
                    .child(self.networks_section(cx))
                    .child(self.volumes_section(cx))
            })
        })
    }

    /// A Compose stack subgroup: a collapsible depth-1 header (layers logo) over its
    /// containers at depth 2. The stack's health dot reflects "any member running".
    fn stack_subgroup(
        &self,
        name: String,
        members: Vec<&docker::Container>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let open = !self.stacks_closed.contains(&name);
        let up = members.iter().filter(|c| c.running).count();
        let dot = if up > 0 {
            theme::status_color(SessionStatus::Done)
        } else {
            theme::status_color(SessionStatus::Idle)
        };
        let leaves: Vec<_> = members
            .iter()
            .map(|c| self.container_leaf(c, 2, cx))
            .collect();
        let toggle = name.clone();
        subgroup(
            SharedString::from(format!("svc-stack-{name}")),
            open,
            IC_COMPOSE,
            dot,
            name,
            format!("{up}/{}", members.len()),
            cx.listener(move |this, _e, _w, cx| this.toggle_stack(toggle.clone(), cx)),
        )
        .when(open, |d| d.children(leaves))
    }

    /// The standalone (non-Compose) **Containers** subgroup.
    fn containers_section(
        &self,
        standalone: Vec<&docker::Container>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let open = !self.docker_sections_closed.contains("containers");
        let up = standalone.iter().filter(|c| c.running).count();
        let dot = if up > 0 {
            theme::status_color(SessionStatus::Done)
        } else {
            theme::status_color(SessionStatus::Idle)
        };
        let leaves: Vec<_> = standalone
            .iter()
            .map(|c| self.container_leaf(c, 2, cx))
            .collect();
        let empty = leaves.is_empty();
        subgroup(
            "svc-dk-containers".into(),
            open,
            IC_CONTAINERS,
            dot,
            "Containers".to_string(),
            format!("{}", standalone.len()),
            cx.listener(|this, _e, _w, cx| this.toggle_docker_section("containers", cx)),
        )
        .when(open, |d| {
            d.when(empty, |d| d.child(hint_row(2, "no standalone containers")))
                .children(leaves)
        })
    }

    /// The **Images** subgroup — the locally downloaded images.
    fn images_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let open = !self.docker_sections_closed.contains("images");
        let images = self.docker.images.clone();
        subgroup(
            "svc-dk-images".into(),
            open,
            IC_IMAGES,
            theme::text_muted(),
            "Images".to_string(),
            format!("{}", images.len()),
            cx.listener(|this, _e, _w, cx| this.toggle_docker_section("images", cx)),
        )
        .when(open, |d| {
            d.when(images.is_empty(), |d| d.child(hint_row(2, "no images")))
                .children(images.iter().map(|img| {
                    let id = img.id.clone();
                    nav_leaf(
                        SharedString::from(format!("svc-nav-img-{id}")),
                        2,
                        false,
                        self.selected == Selection::DockerImage(id.clone()),
                        IC_IMAGE,
                        theme::text_secondary(),
                        cx.listener(move |this, _e, _w, cx| {
                            this.select(Selection::DockerImage(id.clone()), cx)
                        }),
                    )
                    .child(leaf_name(img.name()))
                    .child(div().flex_1())
                    .child(chip(img.size.clone(), theme::text_muted()))
                }))
        })
    }

    /// The **Networks** subgroup.
    fn networks_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let open = !self.docker_sections_closed.contains("networks");
        let networks = self.docker.networks.clone();
        subgroup(
            "svc-dk-networks".into(),
            open,
            IC_NETWORK,
            theme::text_muted(),
            "Networks".to_string(),
            format!("{}", networks.len()),
            cx.listener(|this, _e, _w, cx| this.toggle_docker_section("networks", cx)),
        )
        .when(open, |d| {
            d.when(networks.is_empty(), |d| d.child(hint_row(2, "no networks")))
                .children(networks.iter().map(|n| {
                    let id = n.id.clone();
                    nav_leaf(
                        SharedString::from(format!("svc-nav-net-{id}")),
                        2,
                        false,
                        self.selected == Selection::DockerNetwork(id.clone()),
                        IC_NETWORK,
                        theme::text_secondary(),
                        cx.listener(move |this, _e, _w, cx| {
                            this.select(Selection::DockerNetwork(id.clone()), cx)
                        }),
                    )
                    .child(leaf_name(n.name.clone()))
                    .child(div().flex_1())
                    .child(chip(n.driver.clone(), theme::text_muted()))
                }))
        })
    }

    /// The **Volumes** subgroup.
    fn volumes_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let open = !self.docker_sections_closed.contains("volumes");
        let volumes = self.docker.volumes.clone();
        subgroup(
            "svc-dk-volumes".into(),
            open,
            IC_VOLUME,
            theme::text_muted(),
            "Volumes".to_string(),
            format!("{}", volumes.len()),
            cx.listener(|this, _e, _w, cx| this.toggle_docker_section("volumes", cx)),
        )
        .when(open, |d| {
            d.when(volumes.is_empty(), |d| d.child(hint_row(2, "no volumes")))
                .children(volumes.iter().map(|v| {
                    let name = v.name.clone();
                    nav_leaf(
                        SharedString::from(format!("svc-nav-vol-{name}")),
                        2,
                        false,
                        self.selected == Selection::DockerVolume(name.clone()),
                        IC_VOLUME,
                        theme::text_secondary(),
                        cx.listener({
                            let name = name.clone();
                            move |this, _e, _w, cx| {
                                this.select(Selection::DockerVolume(name.clone()), cx)
                            }
                        }),
                    )
                    .child(leaf_name(clip(&v.name, 26)))
                    .child(div().flex_1())
                    .child(chip(v.driver.clone(), theme::text_muted()))
                }))
        })
    }

    /// One container as a selectable leaf at `depth` (always 2 — under a stack or the
    /// Containers group).
    fn container_leaf(
        &self,
        c: &docker::Container,
        depth: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let cid = c.id.clone();
        let dot = if c.running {
            theme::status_color(SessionStatus::Done)
        } else {
            theme::status_color(SessionStatus::Idle)
        };
        nav_leaf(
            SharedString::from(format!("svc-nav-dk-{cid}")),
            depth,
            false,
            self.selected == Selection::Docker(cid.clone()),
            IC_CONTAINER,
            dot,
            cx.listener(move |this, _e, _w, cx| this.select(Selection::Docker(cid.clone()), cx)),
        )
        .child(leaf_name(c.name.clone()))
        .child(div().flex_1())
        .when(!c.running, |d| {
            d.child(chip("stopped".into(), theme::text_muted()))
        })
    }

    /// HTTP — a top-level selectable leaf (its detail holds env + call history).
    fn http_leaf(&self, snap: &Snapshot, cx: &mut Context<Self>) -> impl IntoElement {
        let count = snap.http_total;
        nav_leaf(
            "svc-nav-http".into(),
            0,
            true,
            self.selected == Selection::Http,
            IC_HTTP,
            theme::accent(),
            cx.listener(|this, _e, _w, cx| this.select(Selection::Http, cx)),
        )
        .child(group_label("HTTP"))
        .child(div().flex_1())
        .child(chip(
            format!("{count} call{}", if count == 1 { "" } else { "s" }),
            theme::text_muted(),
        ))
    }

    // ── Detail pane (the inspector for the selected resource) ──────────────────

    fn detail_pane(&self, snap: &Snapshot, now: i64, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.selected.clone() {
            Selection::Control => scroll_detail("svc-d-control", self.detail_control()),
            Selection::Mcp(name) => match self.mcp_servers.iter().find(|s| s.server == name) {
                Some(s) => scroll_detail("svc-d-mcp", self.detail_mcp(s, snap, now)),
                None => scroll_detail(
                    "svc-d-mcp",
                    detail_gone("This MCP server is no longer listed."),
                ),
            },
            Selection::Docker(cid) => match self.docker.containers.iter().find(|c| c.id == cid) {
                // The container inspector manages its own scrolling (the Logs tab).
                Some(c) => self.detail_docker(c, cx).into_any_element(),
                None => scroll_detail(
                    "svc-d-dk",
                    detail_gone("This container is no longer listed."),
                ),
            },
            Selection::DockerImage(id) => match self.docker.images.iter().find(|i| i.id == id) {
                Some(img) => scroll_detail("svc-d-img", self.detail_image(img)),
                None => scroll_detail("svc-d-img", detail_gone("This image is no longer listed.")),
            },
            Selection::DockerNetwork(id) => {
                match self.docker.networks.iter().find(|n| n.id == id) {
                    Some(n) => scroll_detail("svc-d-net", self.detail_network(n)),
                    None => scroll_detail(
                        "svc-d-net",
                        detail_gone("This network is no longer listed."),
                    ),
                }
            }
            Selection::DockerVolume(name) => {
                match self.docker.volumes.iter().find(|v| v.name == name) {
                    Some(v) => scroll_detail("svc-d-vol", self.detail_volume(v)),
                    None => scroll_detail(
                        "svc-d-vol",
                        detail_gone("This volume is no longer listed."),
                    ),
                }
            }
            Selection::Http => scroll_detail("svc-d-http", self.detail_http(snap, now, cx)),
        };
        div()
            .id("svc-detail")
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .bg(theme::surface_base())
            .flex()
            .flex_col()
            .child(body)
    }

    fn detail_control(&self) -> impl IntoElement {
        let ok = self.snapshot.socket_ok;
        let status = if ok {
            theme::status_color(SessionStatus::Done)
        } else {
            theme::status_color(SessionStatus::Errored)
        };
        detail_body(
            detail_head(
                IC_CONTROL,
                status,
                ok,
                "Control server".into(),
                Some("Hook IPC — the unix socket the Claude Code hooks call.".into()),
                if ok { "listening" } else { "no listener" }.into(),
            ),
            div()
                .flex()
                .flex_col()
                .child(field_mono("socket", self.socket.display().to_string()))
                .child(field_text(
                    "reachable",
                    if ok { "yes — a connect succeeds" } else { "no" }.into(),
                    status,
                ))
                .child(detail_note(
                    "Green means something answers this socket — this instance's server \
                     or another instance's (the spawn refuses to hijack a live socket), \
                     which is exactly what the hook CLI sees.",
                )),
        )
    }

    /// The per-server inspector: config facts (accessible / transport), the sessions
    /// that use it, the tool breakdown, and the recent-call tail — all mined from the
    /// transcripts. The `moonlight` server additionally lists its live embedded
    /// endpoints (from the fast snapshot).
    fn detail_mcp(
        &self,
        s: &mcp_activity::McpServerView,
        snap: &Snapshot,
        now: i64,
    ) -> impl IntoElement {
        let active = s.total_calls > 0;
        let dot = if active {
            theme::status_color(SessionStatus::Done)
        } else {
            theme::status_color(SessionStatus::Idle)
        };
        let subtitle = match (&s.transport, s.accessible) {
            (Some(t), _) => format!("{t} · MCP server"),
            (None, _) => "MCP server (seen in transcripts)".to_string(),
        };
        let pill = if active {
            format!("{} call{}", s.total_calls, if s.total_calls == 1 { "" } else { "s" })
        } else if s.accessible {
            "accessible".to_string()
        } else {
            "idle".to_string()
        };

        // Facts.
        let mut facts = div()
            .flex()
            .flex_col()
            .child(field_text(
                "accessible",
                if s.accessible {
                    "yes — configured for this project"
                } else {
                    "not in config (used ad-hoc)"
                }
                .into(),
                theme::text_secondary(),
            ))
            .child(field_text(
                "transport",
                s.transport.clone().unwrap_or_else(|| "—".into()),
                theme::text_secondary(),
            ))
            .child(field_text(
                "total calls",
                format!("{}", s.total_calls),
                theme::text_secondary(),
            ));
        if s.last_used > 0 {
            facts = facts.child(field_text(
                "last used",
                format!("{} ago", rel_ago(now, s.last_used)),
                theme::text_secondary(),
            ));
        }

        // The moonlight server's live embedded endpoints (bonus, from the snapshot).
        let endpoints = (s.server == "moonlight" && !snap.endpoints.is_empty()).then(|| {
            let mut block = div().flex().flex_col().child(detail_sub("Live endpoints"));
            block = block.children(snap.endpoints.iter().map(|e| {
                let label = e.name.clone().unwrap_or_else(|| e.short_id.clone());
                field_mono(&clip(&label, 14), e.url.clone())
            }));
            block
        });

        // The moonlight audit verb log — authoritative for moonlight (it records the
        // approve/deny/phase/feedback outcomes the transcript's tool_use can't show),
        // merged across sessions, newest first.
        let verb_log = (s.server == "moonlight").then(|| {
            let mut lines: Vec<LogLine> =
                snap.endpoints.iter().flat_map(|e| e.logs.clone()).collect();
            lines.sort_by(|a, b| b.at_millis.cmp(&a.at_millis));
            lines.truncate(12);
            let mut block = div().flex().flex_col().child(detail_sub("Moonlight verb log"));
            if lines.is_empty() {
                block = block.child(hint_row(0, "no verb calls yet"));
            } else {
                block = block.children(lines.into_iter().map(|l| log_row(&l, now)));
            }
            block
        });

        // Sessions using this server.
        let mut sessions = div().flex().flex_col().child(detail_sub("Sessions"));
        if s.sessions.is_empty() {
            sessions = sessions.child(hint_row(0, "no recorded activity yet"));
        } else {
            sessions = sessions.children(s.sessions.iter().map(|u| {
                let label = u.name.clone().unwrap_or_else(|| u.short_id.clone());
                mcp_meta_row(
                    label,
                    format!("{} call{}", u.calls, if u.calls == 1 { "" } else { "s" }),
                    if u.last_used > 0 {
                        rel_ago(now, u.last_used)
                    } else {
                        String::new()
                    },
                )
            }));
        }

        // Tool breakdown.
        let tools = (!s.tools.is_empty()).then(|| {
            let mut block = div().flex().flex_col().child(detail_sub("Tools"));
            block = block.children(s.tools.iter().map(|(tool, n)| {
                mcp_meta_row(tool.clone(), format!("{n}×"), String::new())
            }));
            block
        });

        // Recent activity tail.
        let mut recent = div().flex().flex_col().child(detail_sub("Recent activity"));
        if s.recent.is_empty() {
            recent = recent.child(hint_row(0, "no calls recorded"));
        } else {
            recent = recent.children(s.recent.iter().map(|c| mcp_call_row(c, now)));
        }

        detail_body(
            detail_head(IC_MCP, dot, active, s.server.clone(), Some(subtitle), pill),
            div()
                .flex()
                .flex_col()
                .child(facts)
                .children(endpoints)
                .children(verb_log)
                .child(sessions)
                .children(tools)
                .child(recent),
        )
    }

    /// The Docker container inspector: header + tab bar (with pinned actions) + the
    /// active tab's body. Full-height so the Logs tab can scroll independently.
    fn detail_docker(&self, c: &docker::Container, cx: &mut Context<Self>) -> impl IntoElement {
        let dot = if c.running {
            theme::status_color(SessionStatus::Done)
        } else {
            theme::status_color(SessionStatus::Idle)
        };
        let id = c.id.clone();
        let subtitle = match &c.compose_project {
            Some(p) => format!("{} · stack {p}", c.image),
            None => c.image.clone(),
        };

        // Pinned actions: start (stopped) / stop + restart (running), logs → console,
        // and a refresh for the active tab.
        let mut actions = div().flex().flex_row().items_center().gap_1();
        if c.running {
            actions = actions
                .child(op_button("dk-stop", &id, "stop", {
                    let id = id.clone();
                    cx.listener(move |this, _e, _w, cx| this.docker_op(docker::Op::Stop, &id, cx))
                }))
                .child(op_button("dk-restart", &id, "restart", {
                    let id = id.clone();
                    cx.listener(move |this, _e, _w, cx| {
                        this.docker_op(docker::Op::Restart, &id, cx)
                    })
                }));
        } else {
            actions = actions.child(op_button("dk-start", &id, "start", {
                let id = id.clone();
                cx.listener(move |this, _e, _w, cx| this.docker_op(docker::Op::Start, &id, cx))
            }));
        }
        actions = actions
            .child(op_button("dk-console", &id, "logs ↗", {
                let id = id.clone();
                cx.listener(move |this, _e, _w, cx| this.docker_logs(&id, cx))
            }))
            .child(op_button("dk-refresh", &id, "↻", {
                cx.listener(move |this, _e, _w, cx| this.refresh_docker_detail(cx))
            }));

        let tab_bar = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .pb_1()
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(self.tab_button("Logs", DockerTab::Logs, cx))
            .child(self.tab_button("Stats", DockerTab::Stats, cx))
            .child(self.tab_button("Config", DockerTab::Config, cx))
            .child(div().flex_1())
            .child(actions);

        let body = match self.docker_tab {
            DockerTab::Logs => self.docker_logs_body(&id).into_any_element(),
            DockerTab::Stats => self.docker_stats_body(&id, c.running).into_any_element(),
            DockerTab::Config => self.docker_config_body(c).into_any_element(),
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h(px(0.))
            .p_3()
            .gap_2()
            .child(detail_head(
                IC_CONTAINER,
                dot,
                c.running,
                c.name.clone(),
                Some(subtitle),
                if c.running { "running" } else { "stopped" }.into(),
            ))
            .child(tab_bar)
            .child(body)
    }

    /// A Docker inspector tab button — accent-washed when active, else muted/hover.
    fn tab_button(
        &self,
        label: &'static str,
        tab: DockerTab,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.docker_tab == tab;
        div()
            .id(SharedString::from(format!("dk-tab-{label}")))
            .px(px(9.))
            .py(px(2.))
            .rounded(theme::radius_sm())
            .cursor_pointer()
            .text_size(theme::text_xs())
            .font_weight(FontWeight::MEDIUM)
            .text_color(if active {
                theme::text_primary()
            } else {
                theme::text_muted()
            })
            .when(active, |d| d.bg(theme::row_selected()))
            .when(!active, |d| d.hover(|d| d.bg(theme::row_hover())))
            .on_click(cx.listener(move |this, _e, _w, cx| this.set_docker_tab(tab, cx)))
            .child(label)
    }

    /// The **Logs** tab body: the fetched log tail in a mono, scrollable box. Shows
    /// "loading…" until the fetch for *this* container lands.
    fn docker_logs_body(&self, id: &str) -> impl IntoElement {
        let text = match &self.detail_logs {
            Some((lid, t)) if lid.as_str() == id => Some(t.clone()),
            _ => None,
        };
        let mut box_ = div()
            .id("dk-logs-box")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .p_2()
            .rounded(theme::radius_sm())
            .bg(theme::surface_sunken())
            .border_1()
            .border_color(theme::border_subtle())
            .font_family(theme::mono_font())
            .text_size(theme::text_xs())
            .text_color(theme::text_secondary())
            .flex()
            .flex_col();
        match text {
            None => box_ = box_.child(loading_line()),
            Some(t) if t.trim().is_empty() => {
                box_ = box_.child(
                    div()
                        .text_color(theme::text_muted())
                        .child("· no output ·"),
                )
            }
            Some(t) => {
                box_ = box_.children(
                    t.lines()
                        .map(|line| div().child(line.to_string()).into_any_element()),
                )
            }
        }
        box_
    }

    /// The **Stats** tab body: live resource usage, or a calm hint when the container
    /// isn't running (docker only reports stats for running containers).
    fn docker_stats_body(&self, id: &str, running: bool) -> impl IntoElement {
        let mut body = div().flex().flex_col().pt_1();
        if !running {
            return body.child(hint_row(0, "container is not running — no live stats"));
        }
        match &self.detail_stats {
            Some((sid, Some(s))) if sid.as_str() == id => {
                body = body
                    .child(field_text("cpu", s.cpu_perc.clone(), theme::text_secondary()))
                    .child(field_text(
                        "memory",
                        format!("{}  ({})", s.mem_usage, s.mem_perc),
                        theme::text_secondary(),
                    ))
                    .child(field_text("net i/o", s.net_io.clone(), theme::text_secondary()))
                    .child(field_text(
                        "block i/o",
                        s.block_io.clone(),
                        theme::text_secondary(),
                    ))
                    .child(field_text("pids", s.pids.clone(), theme::text_secondary()));
            }
            Some((sid, None)) if sid.as_str() == id => {
                body = body.child(hint_row(0, "stats unavailable"));
            }
            _ => body = body.child(loading_line()),
        }
        body
    }

    /// The **Config** tab body: the container's exact live configuration from
    /// `docker inspect` — ports exposed, environment, mounts, networks, and policy.
    fn docker_config_body(&self, c: &docker::Container) -> impl IntoElement {
        let mut body = div()
            .id("dk-config")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .pt_1()
            .child(field_mono("image", c.image.clone()))
            .child(field_mono("id", c.id.clone()));
        match &self.detail_inspect {
            Some((iid, Some(i))) if iid == &c.id => {
                body = body
                    .child(field_text(
                        "created",
                        i.created.clone(),
                        theme::text_secondary(),
                    ))
                    .child(field_text(
                        "restart",
                        i.restart_policy.clone(),
                        theme::text_secondary(),
                    ))
                    .when(!i.command.is_empty(), |d| {
                        d.child(field_mono("command", i.command.clone()))
                    })
                    .child(list_block("Ports exposed", &i.ports, "none published"))
                    .child(list_block("Environment", &i.env, "none"))
                    .child(list_block("Mounts", &i.mounts, "none"))
                    .child(list_block("Networks", &i.networks, "none"));
            }
            Some((iid, None)) if iid == &c.id => {
                body = body.child(detail_note("inspect unavailable for this container"));
            }
            _ => body = body.child(div().pt_2().child(loading_line())),
        }
        body
    }

    /// The image inspector: repo:tag, id, size, created, plus the containers using it.
    fn detail_image(&self, img: &docker::Image) -> impl IntoElement {
        let users: Vec<String> = self
            .docker
            .containers
            .iter()
            .filter(|c| c.image == img.name() || c.image == img.id)
            .map(|c| c.name.clone())
            .collect();
        detail_body(
            detail_head(
                IC_IMAGE,
                theme::text_secondary(),
                false,
                img.name(),
                Some("Docker image".into()),
                img.size.clone(),
            ),
            div()
                .flex()
                .flex_col()
                .child(field_mono("id", img.id.clone()))
                .child(field_text("tag", img.tag.clone(), theme::text_secondary()))
                .child(field_text("size", img.size.clone(), theme::text_secondary()))
                .child(field_text(
                    "created",
                    img.created.clone(),
                    theme::text_secondary(),
                ))
                .child(list_block("Used by containers", &users, "not used by any container")),
        )
    }

    /// The network inspector: id, driver, scope, and the containers attached to it.
    fn detail_network(&self, n: &docker::Network) -> impl IntoElement {
        let attached: Vec<String> = self
            .docker
            .containers
            .iter()
            .filter(|c| c.compose_project.is_some())
            .filter(|_| n.driver == "bridge" && n.name.ends_with("_default"))
            .map(|c| c.name.clone())
            .collect();
        detail_body(
            detail_head(
                IC_NETWORK,
                theme::status_color(SessionStatus::Running),
                true,
                n.name.clone(),
                Some("Docker network".into()),
                n.driver.clone(),
            ),
            div()
                .flex()
                .flex_col()
                .child(field_mono("id", n.id.clone()))
                .child(field_text("driver", n.driver.clone(), theme::text_secondary()))
                .child(field_text("scope", n.scope.clone(), theme::text_secondary()))
                .when(!attached.is_empty(), |d| {
                    d.child(list_block("Likely attached", &attached, "—"))
                }),
        )
    }

    /// The volume inspector: driver + the host mountpoint (lazily inspected).
    fn detail_volume(&self, v: &docker::Volume) -> impl IntoElement {
        let mount = match &self.detail_volume_mount {
            Some((name, m)) if name == &v.name => m.clone(),
            _ => None,
        };
        let mount_el = match mount {
            Some(m) => field_mono("mountpoint", m).into_any_element(),
            None => field_text("mountpoint", "loading…".into(), theme::text_muted())
                .into_any_element(),
        };
        detail_body(
            detail_head(
                IC_VOLUME,
                theme::status_color(SessionStatus::Done),
                false,
                v.name.clone(),
                Some("Docker volume".into()),
                v.driver.clone(),
            ),
            div()
                .flex()
                .flex_col()
                .child(field_text("driver", v.driver.clone(), theme::text_secondary()))
                .child(mount_el),
        )
    }

    fn detail_http(&self, snap: &Snapshot, now: i64, cx: &mut Context<Self>) -> impl IntoElement {
        let env = snap.http_env.clone().unwrap_or_else(|| "local only".into());
        let count = snap.http_total;

        let open_btn = div()
            .id("svc-http-open")
            .flex_none()
            .self_start()
            .px(px(8.))
            .py(px(3.))
            .rounded(theme::radius_sm())
            .border_1()
            .border_color(theme::tint(theme::accent(), 0.5))
            .text_size(theme::text_xs())
            .text_color(theme::accent())
            .cursor_pointer()
            .hover(|d| {
                d.bg(theme::tint(theme::accent(), 0.16))
                    .text_color(theme::text_primary())
            })
            .on_click(cx.listener(|_this, _ev, _w, cx| {
                if let Some(deps) = cx.try_global::<ShellDeps>() {
                    let center = deps.center.clone();
                    center.update(cx, |_, cx| cx.emit(OpenRequest::Http));
                }
            }))
            .child("↗ open the request builder");

        let mut calls = div().flex().flex_col().child(detail_sub("Recent calls"));
        if snap.http_calls.is_empty() {
            calls = calls.child(hint_row(0, "no requests yet — http_request calls land here"));
        } else {
            calls = calls.children(snap.http_calls.iter().map(|c| http_row(c, now)));
        }

        detail_body(
            detail_head(
                IC_HTTP,
                theme::accent(),
                count > 0,
                "HTTP".into(),
                Some("http_request call history for the active space.".into()),
                format!("{count} call{}", if count == 1 { "" } else { "s" }),
            ),
            div()
                .flex()
                .flex_col()
                .child(field_text("environment", env, theme::text_secondary()))
                .child(field_text(
                    "total",
                    format!("{count}"),
                    theme::text_secondary(),
                ))
                .child(div().pt_1().child(open_btn))
                .child(calls),
        )
    }
}

impl Render for ServicesPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snap = self.snapshot.clone();
        let now = now_millis();

        div()
            .id("services-panel")
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::surface_base())
            .text_color(theme::text_primary())
            // The tool window's bar: title + the uniform hide ✕ (bottom dock).
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(28.))
                    .px_2()
                    .gap_1()
                    .bg(theme::surface_sunken())
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .child(
                        div()
                            .text_size(theme::text_xs())
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text_secondary())
                            .child("Services"),
                    )
                    .child(div().flex_1())
                    .child(super::tool_hide_button(
                        "services-hide",
                        ChromeRequest::HideBottomDock,
                    )),
            )
            // The master–detail split: selectable tree (left) + inspector (right).
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_row()
                    .child(
                        div()
                            .id("services-tree")
                            .flex_none()
                            .w(px(TREE_W))
                            .min_h(px(0.))
                            .overflow_y_scroll()
                            .py_1()
                            .bg(theme::surface_sunken())
                            .border_r_1()
                            .border_color(theme::border_subtle())
                            .flex()
                            .flex_col()
                            .child(self.control_leaf(snap.socket_ok, cx))
                            .child(self.mcp_group(cx))
                            .child(self.docker_group(cx))
                            .child(self.http_leaf(&snap, cx)),
                    )
                    .child(self.detail_pane(&snap, now, cx)),
            )
    }
}

/// A collapsible category header for the master tree: disclosure + a health-tinted
/// type icon + label + a right-aligned muted count chip, returned as a column the
/// caller fills with child leaves under `.when(open, …)`. A faint top hairline
/// separates categories.
fn group(
    id: &'static str,
    open: bool,
    icon: Ico,
    dot: Hsla,
    label: &str,
    chip_text: String,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    div()
        .flex()
        .flex_col()
        .border_t_1()
        .border_color(theme::border_subtle())
        .child(
            row(0)
                .id(id)
                .rounded(theme::radius_sm())
                .hover(|d| d.bg(theme::row_hover()))
                .on_click(on_toggle)
                .child(disclosure(open))
                .child(icon_slot(icon, dot))
                .child(group_label(label))
                .child(div().flex_1())
                .child(chip(chip_text, theme::text_muted())),
        )
}

/// A collapsible **subgroup** (depth-1) inside a category: disclosure + tinted icon +
/// medium label + a muted count chip. Returned as a column the caller fills with child
/// leaves under `.when(open, …)`. Unlike [`group`] it carries no top hairline.
fn subgroup(
    id: SharedString,
    open: bool,
    icon: Ico,
    dot: Hsla,
    label: String,
    chip_text: String,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    div().flex().flex_col().child(
        row(1)
            .id(id)
            .rounded(theme::radius_sm())
            .hover(|d| d.bg(theme::row_hover()))
            .on_click(on_toggle)
            .child(disclosure(open))
            .child(icon_slot(icon, dot))
            .child(
                div()
                    .text_size(theme::text_sm())
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text_secondary())
                    .child(label),
            )
            .child(div().flex_1())
            .child(chip(chip_text, theme::text_muted())),
    )
}

// ── Detail-pane chrome ────────────────────────────────────────────────────────

/// Wrap a short detail body in a scrolling, padded column (for the non-Docker
/// inspectors, whose content grows but manages no inner scroll of its own).
fn scroll_detail(id: &'static str, inner: impl IntoElement) -> gpui::AnyElement {
    div()
        .id(id)
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scroll()
        .p_3()
        .child(inner)
        .into_any_element()
}

/// The detail header: a lit type icon, the resource title, a status pill, and an
/// optional one-line subtitle — over a hairline that separates it from the body.
fn detail_head(
    icon: Ico,
    dot: Hsla,
    live: bool,
    title: String,
    subtitle: Option<String>,
    pill: String,
) -> impl IntoElement {
    // The lamp: the type icon at title size, tinted by health, with a soft glow when
    // the resource is live. Wrapped so the glow applies to glyphs and SVG marks alike.
    let mut lamp = div()
        .flex_none()
        .flex()
        .items_center()
        .child(ico_el(icon, dot, theme::text_md()));
    if live {
        lamp = lamp.shadow(theme::glow(dot));
    }
    div()
        .flex()
        .flex_col()
        .gap_1()
        .pb_2()
        .border_b_1()
        .border_color(theme::border_subtle())
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(lamp)
                .child(
                    div()
                        .text_size(theme::text_lg())
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::text_primary())
                        .child(title),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .flex_none()
                        .px(px(7.))
                        .py(px(1.))
                        .rounded(theme::radius_sm())
                        .bg(theme::tint(dot, 0.16))
                        .text_size(theme::text_2xs())
                        .text_color(dot)
                        .child(pill),
                ),
        )
        .children(subtitle.map(|s| {
            div()
                .text_size(theme::text_sm())
                .text_color(theme::text_secondary())
                .child(s)
        }))
}

/// Wrap a detail header + body in the standard inspector column.
fn detail_body(head: impl IntoElement, body: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(head)
        .child(div().pt_1().flex().flex_col().child(body))
}

/// A property row: a fixed-width uppercase muted label + a value element.
fn field(label: &str, value: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_baseline()
        .gap_3()
        .py(px(2.))
        .child(
            div()
                .flex_none()
                .w(px(92.))
                .text_size(theme::text_2xs())
                .text_color(theme::text_muted())
                .child(label.to_uppercase()),
        )
        .child(value)
}

/// A property row whose value is a monospace fact (path / url / id).
fn field_mono(label: &str, value: String) -> impl IntoElement {
    field(
        label,
        div()
            .flex_1()
            .min_w(px(0.))
            .font_family(theme::mono_font())
            .text_size(theme::text_xs())
            .text_color(theme::text_secondary())
            .child(value),
    )
}

/// A property row whose value is plain text in `color`.
fn field_text(label: &str, value: String, color: Hsla) -> impl IntoElement {
    field(
        label,
        div()
            .flex_1()
            .min_w(px(0.))
            .text_size(theme::text_sm())
            .text_color(color)
            .child(value),
    )
}

/// A titled block of monospace lines (ports / env / mounts / networks) for the Config
/// tab, or the `empty` hint when the list is empty.
fn list_block(title: &str, items: &[String], empty: &str) -> impl IntoElement {
    let mut block = div().flex().flex_col().child(detail_sub(title));
    if items.is_empty() {
        block = block.child(
            div()
                .pl(px(2.))
                .text_size(theme::text_xs())
                .text_color(theme::text_muted())
                .child(empty.to_string()),
        );
    } else {
        block = block.children(items.iter().map(|it| {
            div()
                .pl(px(2.))
                .py(px(1.))
                .font_family(theme::mono_font())
                .text_size(theme::text_xs())
                .text_color(theme::text_secondary())
                .child(it.clone())
        }));
    }
    block
}

/// A muted section subheader inside the detail body.
fn detail_sub(text: &str) -> impl IntoElement {
    div()
        .pt_2()
        .pb_1()
        .text_size(theme::text_2xs())
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_muted())
        .child(text.to_uppercase())
}

/// A soft explanatory paragraph in the detail body.
fn detail_note(text: &str) -> impl IntoElement {
    div()
        .pt_2()
        .text_size(theme::text_xs())
        .text_color(theme::text_muted())
        .child(text.to_string())
}

/// A muted "loading…" placeholder while an async fetch is in flight.
fn loading_line() -> impl IntoElement {
    div()
        .text_size(theme::text_xs())
        .text_color(theme::text_muted())
        .child("loading…")
}

/// The detail pane when the selected resource has disappeared.
fn detail_gone(text: &str) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .size_full()
        .gap_1()
        .child(
            div()
                .text_size(theme::text_sm())
                .text_color(theme::text_muted())
                .child(text.to_string()),
        )
}

/// A quiet pill action button (start / stop / restart / logs) — bordered, hover-lit.
fn op_button(
    prefix: &str,
    id: &str,
    label: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("{prefix}-{id}")))
        .flex_none()
        .px(px(8.))
        .py(px(2.))
        .rounded(theme::radius_sm())
        .border_1()
        .border_color(theme::border_subtle())
        .text_size(theme::text_xs())
        .text_color(theme::text_secondary())
        .cursor_pointer()
        .hover(|d| {
            d.bg(theme::tint(theme::accent(), 0.16))
                .border_color(theme::accent())
                .text_color(theme::text_primary())
        })
        .on_click(on_click)
        .child(label)
}

/// One verb-call log line: glyph + monospace summary + relative age.
fn log_row(l: &LogLine, now: i64) -> impl IntoElement {
    let c = kind_color(l.kind);
    row(0)
        .child(
            div()
                .w(px(SLOT_W))
                .flex_none()
                .text_size(theme::text_xs())
                .text_color(c)
                .child(kind_glyph(l.kind)),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .font_family(theme::mono_font())
                .text_size(theme::text_xs())
                .text_color(theme::text_secondary())
                .child(l.text.clone()),
        )
        .child(
            div()
                .flex_none()
                .text_size(theme::text_2xs())
                .text_color(theme::text_muted())
                .child(rel_ago(now, l.at_millis)),
        )
}

/// A compact detail row: a name, a right-aligned count/chip, and an optional age —
/// used for the per-server "Sessions" and "Tools" lists.
fn mcp_meta_row(label: String, right: String, ago: String) -> impl IntoElement {
    row(0)
        .child(div().w(px(SLOT_W)).flex_none())
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(theme::text_sm())
                .text_color(theme::text_secondary())
                .child(label),
        )
        .child(
            div()
                .flex_none()
                .text_size(theme::text_xs())
                .text_color(theme::text_muted())
                .child(right),
        )
        .when(!ago.is_empty(), |d| {
            d.child(
                div()
                    .flex_none()
                    .w(px(36.))
                    .text_size(theme::text_2xs())
                    .text_color(theme::text_muted())
                    .child(ago),
            )
        })
}

/// One MCP call in the recent-activity tail: tool (mono) · session · age.
fn mcp_call_row(c: &mcp_activity::McpCall, now: i64) -> impl IntoElement {
    let ago = if c.at_millis > 0 {
        rel_ago(now, c.at_millis)
    } else {
        String::new()
    };
    row(0)
        .child(
            div()
                .w(px(SLOT_W))
                .flex_none()
                .text_size(theme::text_2xs())
                .text_color(theme::tree_glyph())
                .child("›"),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .font_family(theme::mono_font())
                .text_size(theme::text_xs())
                .text_color(theme::text_secondary())
                .child(c.tool.clone()),
        )
        .child(
            div()
                .flex_none()
                .font_family(theme::mono_font())
                .text_size(theme::text_2xs())
                .text_color(theme::text_muted())
                .child(c.session_short.clone()),
        )
        .child(
            div()
                .flex_none()
                .w(px(36.))
                .text_size(theme::text_2xs())
                .text_color(theme::text_muted())
                .child(ago),
        )
}

/// One HTTP call row: method · url (mono) · status · timing · age.
fn http_row(c: &crate::http::HttpCall, now: i64) -> impl IntoElement {
    let ok_color = if c.ok {
        theme::status_color(SessionStatus::Done)
    } else {
        theme::status_color(SessionStatus::Errored)
    };
    let status_text = match (c.status, &c.error) {
        (Some(code), _) => format!("{code}"),
        (None, Some(_)) => "ERR".to_string(),
        (None, None) => "—".to_string(),
    };
    row(0)
        .child(
            div()
                .flex_none()
                .w(px(40.))
                .font_family(theme::mono_font())
                .text_size(theme::text_xs())
                .text_color(theme::text_secondary())
                .child(c.method.clone()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .font_family(theme::mono_font())
                .text_size(theme::text_xs())
                .text_color(theme::text_secondary())
                .child(clip(&c.url, 60)),
        )
        .child(
            div()
                .flex_none()
                .w(px(34.))
                .text_size(theme::text_xs())
                .text_color(ok_color)
                .child(status_text),
        )
        .child(
            div()
                .flex_none()
                .text_size(theme::text_2xs())
                .text_color(theme::text_muted())
                .child(format!("{}ms", c.ms)),
        )
        .child(
            div()
                .flex_none()
                .text_size(theme::text_2xs())
                .text_color(theme::text_muted())
                .child(rel_ago(now, c.at_millis)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    use moonlight_domain::ids::{SessionId, Timestamp};
    use moonlight_domain::phase::Phase;

    #[test]
    fn probe_socket_sees_absent_as_dead_and_live_as_ok() {
        let dir = std::env::temp_dir().join(format!("ml-svc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("probe.sock");

        // Absent → dead.
        assert!(!probe_socket(&path));
        // Live listener → ok. (The freshly-dropped-listener case is deliberately
        // untested: macOS may keep accepting on the dead file briefly — OS behavior,
        // not ours.)
        let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(probe_socket(&path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn short_id_truncates_to_eight() {
        assert_eq!(short_id("a2dafbb6-2f90-4f14"), "a2dafbb6");
        assert_eq!(short_id("abc"), "abc");
    }

    #[test]
    fn rel_ago_buckets_by_magnitude() {
        assert_eq!(rel_ago(10_000, 10_000), "now");
        assert_eq!(rel_ago(10_000, 9_000), "now"); // <2s rounds to now
        assert_eq!(rel_ago(15_000, 10_000), "5s");
        assert_eq!(rel_ago(130_000, 10_000), "2m");
        assert_eq!(rel_ago(7_210_000, 10_000), "2h");
        // A clock skew (at in the future) clamps to "now", never negative.
        assert_eq!(rel_ago(1_000, 5_000), "now");
    }

    fn entry(action: AuditAction) -> AuditEntry {
        AuditEntry {
            id: "01".into(),
            session_id: SessionId::new("s1"),
            at: Timestamp(42),
            action,
            revertible: false,
        }
    }

    #[test]
    fn log_line_classifies_each_audit_action() {
        let ran = log_line(&entry(AuditAction::VerbExecuted {
            verb: McpVerb::RunWithCoverage,
            summary: "41 passed".into(),
        }));
        assert_eq!(ran.kind, LogKind::Ran);
        assert!(ran.text.starts_with("run_with_coverage · "), "{}", ran.text);
        assert_eq!(ran.at_millis, 42);

        // A "failed: …" summary flips the kind to Failed.
        let failed = log_line(&entry(AuditAction::VerbExecuted {
            verb: McpVerb::RunStart,
            summary: "failed: boom".into(),
        }));
        assert_eq!(failed.kind, LogKind::Failed);

        let denied = log_line(&entry(AuditAction::Denied {
            what: "RequestPhase plan".into(),
            reason: "needs approval".into(),
        }));
        assert_eq!(denied.kind, LogKind::Denied);
        assert!(denied.text.contains("needs approval"));

        let phase = log_line(&entry(AuditAction::PhaseChanged {
            to: Phase::AutoImplement,
        }));
        assert_eq!(phase.kind, LogKind::Phase);
        assert!(phase.text.contains("Auto"));
    }

    #[test]
    fn clip_caps_long_text() {
        assert_eq!(clip("short", 80), "short");
        let long = "x".repeat(100);
        let clipped = clip(&long, 80);
        assert_eq!(clipped.chars().count(), 81); // 80 + ellipsis
        assert!(clipped.ends_with('…'));
    }

    #[test]
    fn verb_label_is_the_tool_name() {
        assert_eq!(verb_label(McpVerb::RequestPhase), "request_phase");
        assert_eq!(verb_label(McpVerb::RunWithCoverage), "run_with_coverage");
    }

    #[test]
    fn selection_default_is_control() {
        assert_eq!(Selection::default(), Selection::Control);
    }

    #[test]
    fn docker_tab_defaults_to_logs() {
        assert_eq!(DockerTab::default(), DockerTab::Logs);
    }

    fn container(name: &str, project: Option<&str>, running: bool) -> docker::Container {
        docker::Container {
            id: format!("id-{name}"),
            name: name.to_string(),
            image: "img".into(),
            status: if running { "Up" } else { "Exited" }.into(),
            running,
            ports: String::new(),
            compose_project: project.map(|s| s.to_string()),
        }
    }

    #[test]
    fn stack_groups_partitions_and_sorts() {
        let cs = vec![
            container("web", Some("shop"), true),
            container("loose", None, false),
            container("db", Some("shop"), true),
            container("cache", Some("infra"), true),
        ];
        let (stacks, standalone) = stack_groups(&cs);

        // Stacks are sorted by name: infra before shop.
        let names: Vec<&str> = stacks.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["infra", "shop"]);
        // The shop stack keeps both its members.
        let shop = &stacks.iter().find(|(n, _)| n == "shop").unwrap().1;
        assert_eq!(shop.len(), 2);
        // Standalone holds only the project-less container.
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].name, "loose");
    }
}
