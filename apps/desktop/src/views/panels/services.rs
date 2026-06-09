//! The **Services** tool window — modelled on JetBrains' Services view: a compact
//! **tree** of the app's long-lived background services, grouped into collapsible
//! categories with fixed-height rows, aligned disclosure/status columns, and a
//! monospace column for code-adjacent facts (paths, endpoints, ids).
//!
//! Categories:
//! - **Control server** (hook IPC): probed by connecting to its unix socket. Green =
//!   something answers — this instance's server *or* another instance's (the spawn
//!   refuses to hijack a live socket), which is exactly what the hook CLI sees.
//! - **MCP servers**: the embedded per-session `moonlight` hosts — runtime up/down,
//!   the live HTTP endpoints, each labelled with its session's **name** (the managed
//!   record's title) and a **live verb-call log** read from the durable audit store.
//! - **Docker**: running/stopped containers with start/stop/restart/logs ops.
//! - **HTTP**: the `http_request` call history (active env + recent results).
//!
//! Like the terminal and Run console this is a **workspace-owned bottom tool** (plain
//! `Render` entity, not a dock `Panel`): the bottom dock renders it when the stripe's
//! ⚙ fronts it. Cheap local state (socket probe, MCP endpoints + audit tail, HTTP
//! history) refreshes on a 2s poll; Docker has its own 3s background-executor poll.
//! Re-render only fires when a snapshot actually changed. Collapse is operator state.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{div, px, App, ClickEvent, Context, Div, FontWeight, Hsla, SharedString, Window};

use moonlight_domain::audit::{AuditAction, AuditEntry};
use moonlight_domain::ports::ManagedSessionStore;
use moonlight_domain::session::SessionStatus;
use moonlight_domain::trust::McpVerb;

use crate::docker;
use crate::views::chrome_requests::ChromeRequest;
use crate::views::theme;
use crate::views::workspace::ShellDeps;

/// How many recent audit rows to surface per MCP session as its verb-call log.
const LOG_LINES: usize = 6;
/// Cap a log line so a verbose summary can't blow the row width.
const LOG_CLIP: usize = 80;
/// Fixed row height for every tree row (JetBrains Services density).
const ROW_H: f32 = 24.;
/// Per-depth indent step; the disclosure/status slot is [`SLOT_W`] wide.
const INDENT_STEP: f32 = 14.;
const SLOT_W: f32 = 13.;

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

pub struct ServicesPanel {
    socket: PathBuf,
    snapshot: Snapshot,
    /// Latest Docker probe (its own background-executor poll — the CLI shell-out must
    /// never run on the foreground render/poll thread).
    docker: docker::DockerStatus,
    /// Section collapse flags (operator state; default expanded).
    mcp_open: bool,
    docker_open: bool,
    http_open: bool,
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
            cx.background_executor()
                .timer(Duration::from_secs(2))
                .await;
        })
        .detach();

        // Docker poll (3s) — the `docker` shell-out runs on a background-executor task
        // so a slow/hung daemon can't jank the UI; the foreground only diffs + notifies.
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
            });
            if alive.is_err() {
                break;
            }
            cx.background_executor()
                .timer(Duration::from_secs(3))
                .await;
        })
        .detach();

        Self {
            socket,
            snapshot: Snapshot::default(),
            docker: docker::DockerStatus::default(),
            mcp_open: true,
            docker_open: true,
            http_open: true,
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
        let _ = run_registry.start(&format!("docker logs {id}"), &docker::logs_command(id), root);
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

/// Classify one durable audit entry into a verb-call log line.
fn log_line(entry: &AuditEntry) -> LogLine {
    let (kind, text) = match &entry.action {
        AuditAction::VerbExecuted { verb, summary } => {
            let failed = summary.to_ascii_lowercase().starts_with("failed");
            (
                if failed { LogKind::Failed } else { LogKind::Ran },
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

// ── Row chrome (JetBrains Services tree: fixed-height rows, aligned slots) ─────

/// A tree row at `depth`: fixed height + leading indent so the disclosure triangles
/// and status dots of nested rows line up under their parent. Callers chain
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

/// A status dot (or empty placeholder) occupying the same leading slot as a
/// disclosure triangle, so leaf rows align under their group's label.
fn dot_slot(color: Option<Hsla>) -> impl IntoElement {
    let mut slot = div()
        .w(px(SLOT_W))
        .flex_none()
        .flex()
        .items_center()
        .justify_center();
    if let Some(c) = color {
        slot = slot.child(div().w(px(7.)).h(px(7.)).flex_none().rounded_full().bg(c));
    }
    slot
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

/// A monospace secondary fact (path / url / id), right-aligned + muted.
fn mono_meta(text: String) -> impl IntoElement {
    div()
        .flex_none()
        .font_family(theme::mono_font())
        .text_size(theme::text_xs())
        .text_color(theme::text_muted())
        .child(text)
}

/// A quiet hint row at `depth` (empty states).
fn hint_row(depth: usize, text: &str) -> impl IntoElement {
    row(depth)
        .child(div().w(px(SLOT_W)).flex_none())
        .child(
            div()
                .text_size(theme::text_xs())
                .text_color(theme::text_muted())
                .child(text.to_string()),
        )
}

impl ServicesPanel {
    /// Control server — a top-level singleton row (no children to expand).
    fn control_row(&self, ok: bool) -> impl IntoElement {
        let status = if ok {
            theme::status_color(SessionStatus::Done)
        } else {
            theme::status_color(SessionStatus::Errored)
        };
        let path = clip(&self.socket.display().to_string(), 38);
        row(0)
            .rounded(theme::radius_sm())
            .hover(|d| d.bg(theme::row_hover()))
            // Blank slot where a group's triangle would be → its name aligns with groups.
            .child(div().w(px(SLOT_W)).flex_none())
            .child(dot_slot(Some(status)))
            .child(group_label("Control server"))
            .child(chip(
                if ok { "listening" } else { "no listener" }.to_string(),
                status,
            ))
            .child(div().flex_1())
            .child(mono_meta(path))
    }

    /// The MCP servers category: header (up/down + session count) over per-endpoint
    /// rows, each expanding to its verb-call log tail.
    fn mcp_section(&self, snap: &Snapshot, now: i64, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.mcp_open;
        let count = snap.endpoints.len();
        let dot = if snap.mcp_up {
            theme::status_color(SessionStatus::Done)
        } else {
            theme::status_color(SessionStatus::Errored)
        };
        group(
            "svc-sec-mcp",
            open,
            Some(dot),
            "MCP servers",
            format!("{count} session{}", if count == 1 { "" } else { "s" }),
            theme::text_muted(),
            cx.listener(|this, _ev, _w, cx| {
                this.mcp_open = !this.mcp_open;
                cx.notify();
            }),
        )
        .when(open, |d| {
            d.when(snap.mcp_up && snap.endpoints.is_empty(), |d| {
                d.child(hint_row(1, "no session endpoints yet — launch a managed session"))
            })
            .children(snap.endpoints.iter().map(|e| endpoint_card(e, now)))
        })
    }

    /// The Docker category: header (running/total) over container rows with ops.
    fn docker_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.docker_open;
        let status = self.docker.clone();
        let running = status.containers.iter().filter(|c| c.running).count();
        let (dot, count_text) = if !status.available {
            (theme::status_color(SessionStatus::Errored), "unavailable".to_string())
        } else if running > 0 {
            (
                theme::status_color(SessionStatus::Done),
                format!("{running}/{} running", status.containers.len()),
            )
        } else {
            (
                theme::status_color(SessionStatus::Idle),
                format!("0/{} running", status.containers.len()),
            )
        };
        group(
            "svc-sec-docker",
            open,
            Some(dot),
            "Docker",
            count_text,
            theme::text_muted(),
            cx.listener(|this, _ev, _w, cx| {
                this.docker_open = !this.docker_open;
                cx.notify();
            }),
        )
        .when(open, |d| {
            d.when(!status.available, |d| {
                d.child(hint_row(1, "docker not available (CLI absent or daemon down)"))
            })
            .when(status.available && status.containers.is_empty(), |d| {
                d.child(hint_row(1, "no containers"))
            })
            .children(status.containers.iter().map(|c| self.container_row(c, cx)))
        })
    }

    /// One container row: status dot + name + status + image/ports (mono) + ops.
    fn container_row(&self, c: &docker::Container, cx: &mut Context<Self>) -> impl IntoElement {
        let dot = if c.running {
            theme::status_color(SessionStatus::Done)
        } else {
            theme::status_color(SessionStatus::Idle)
        };
        let mut meta = c.image.clone();
        if !c.ports.is_empty() {
            meta.push_str(&format!("  :{}", c.ports));
        }
        if let Some(p) = &c.compose_project {
            meta.push_str(&format!("  ⊟{p}"));
        }
        let id = c.id.clone();
        row(1)
            .rounded(theme::radius_sm())
            .hover(|d| d.bg(theme::row_hover()))
            .child(dot_slot(Some(dot)))
            .child(leaf_name(c.name.clone()))
            .child(chip(
                clip(&c.status, 22),
                if c.running {
                    theme::status_color(SessionStatus::Done)
                } else {
                    theme::text_muted()
                },
            ))
            .child(div().flex_1())
            .child(mono_meta(clip(&meta, 30)))
            // Ops: stop/restart while running, start while stopped; logs always.
            .when(c.running, |d| {
                d.child(op_button("dk-stop", &id, "stop", {
                    let id = id.clone();
                    cx.listener(move |this, _e, _w, cx| this.docker_op(docker::Op::Stop, &id, cx))
                }))
                .child(op_button("dk-restart", &id, "restart", {
                    let id = id.clone();
                    cx.listener(move |this, _e, _w, cx| {
                        this.docker_op(docker::Op::Restart, &id, cx)
                    })
                }))
            })
            .when(!c.running, |d| {
                d.child(op_button("dk-start", &id, "start", {
                    let id = id.clone();
                    cx.listener(move |this, _e, _w, cx| this.docker_op(docker::Op::Start, &id, cx))
                }))
            })
            .child(op_button("dk-logs", &id, "logs", {
                let id = id.clone();
                cx.listener(move |this, _e, _w, cx| this.docker_logs(&id, cx))
            }))
    }

    /// The HTTP category: header (active env + call count) over recent call rows.
    fn http_section(&self, snap: &Snapshot, now: i64, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.http_open;
        let env = snap.http_env.clone().unwrap_or_else(|| "local only".into());
        let count = snap.http_total;
        group(
            "svc-sec-http",
            open,
            None,
            "HTTP",
            format!("{env} · {count} call{}", if count == 1 { "" } else { "s" }),
            theme::text_muted(),
            cx.listener(|this, _ev, _w, cx| {
                this.http_open = !this.http_open;
                cx.notify();
            }),
        )
        .when(open, |d| {
            d.when(snap.http_calls.is_empty(), |d| {
                d.child(hint_row(1, "no requests yet — http_request calls land here"))
            })
            .children(snap.http_calls.iter().map(|c| http_row(c, now)))
        })
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
            .child(
                div()
                    .id("services-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .py_1()
                    .flex()
                    .flex_col()
                    .child(self.control_row(snap.socket_ok))
                    .child(self.mcp_section(&snap, now, cx))
                    .child(self.docker_section(cx))
                    .child(self.http_section(&snap, now, cx)),
            )
    }
}

/// A collapsible category: the header row (disclosure + optional health dot + label +
/// right-aligned chip), returned as a column the caller fills with child rows under
/// `.when(open, …)`. A faint top hairline separates categories.
fn group(
    id: &'static str,
    open: bool,
    dot: Option<Hsla>,
    label: &str,
    chip_text: String,
    chip_color: Hsla,
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
                .child(dot_slot(dot))
                .child(group_label(label))
                .child(div().flex_1())
                .child(chip(chip_text, chip_color)),
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
        .px(px(6.))
        .py(px(1.))
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

/// One MCP endpoint (depth 1) + its verb-call log tail (depth 2), as a column.
fn endpoint_card(e: &McpEndpoint, now: i64) -> impl IntoElement {
    let title = e.name.clone().unwrap_or_else(|| e.short_id.clone());
    div()
        .flex()
        .flex_col()
        .child(
            row(1)
                .rounded(theme::radius_sm())
                .hover(|d| d.bg(theme::row_hover()))
                .child(dot_slot(Some(theme::status_color(SessionStatus::Running))))
                .child(leaf_name(title))
                .child(
                    div()
                        .flex_none()
                        .font_family(theme::mono_font())
                        .text_size(theme::text_2xs())
                        .text_color(theme::tree_glyph())
                        .child(e.short_id.clone()),
                )
                .child(div().flex_1())
                .child(mono_meta(clip(&e.url, 36))),
        )
        .when(e.logs.is_empty(), |d| {
            d.child(hint_row(2, "no verb calls yet"))
        })
        .children(e.logs.iter().map(|l| log_row(l, now)))
}

/// One verb-call log line (depth 2): glyph + monospace summary + relative age.
fn log_row(l: &LogLine, now: i64) -> impl IntoElement {
    let c = kind_color(l.kind);
    row(2)
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

/// One HTTP call row (depth 1): method · url (mono) · status · timing · age.
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
    row(1)
        .rounded(theme::radius_sm())
        .hover(|d| d.bg(theme::row_hover()))
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
                .font_family(theme::mono_font())
                .text_size(theme::text_xs())
                .text_color(theme::text_secondary())
                .child(clip(&c.url, 48)),
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

        let phase = log_line(&entry(AuditAction::PhaseChanged { to: Phase::AutoImplement }));
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
}
