//! MoonlightCode desktop â composition root.
//!
//! Opens the JetBrains-style dockable workspace shell: a `DockArea` with the
//! fleet grid at center, the project file tree on the left rail, and the manual
//! terminal on the bottom rail (see [`views::workspace`]). The composition root
//! still runs a domain self-check (a PDP verdict) at startup so the engine/domain
//! graph is provably live, then builds the shared UI state and hands the window to
//! the UI layer. Concrete adapters are built and injected into ports here â the
//! single wiring place.

mod agent_backend;
mod agy_hook;
// AGY token / context / cost via the `agy` language-server RPC (see the module docs).
mod agy_ls;
mod agy_setup;
mod assets;
mod docker;
mod git;
mod git_probe;
mod grpc;
mod hook_install;
mod http;
mod http_verbs;
mod icon;
mod lsp;
mod mcp_activity;
mod obs;
mod pair_diff;
mod path_tree;
mod review_findings;
mod review_skills;
mod run;
mod run_verbs;
#[cfg(test)]
mod seed;
mod support;
mod term;
mod transcript;
mod views;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use gpui::{AppContext, TitlebarOptions, WindowOptions};
use gpui_component::Root;

use moonlight_control::{
    append_safe_tool, load_config, query_hook, user_config_path, AiWorkspaceResolver,
    ApprovalNotifier, ControlServer, Decision, GateView, HookResponse, KeystoneApprovalGate,
    ObserveOnlyControl, PendingApprovals, RuntimeSafeTools, SteerControl,
};
use moonlight_detection::{
    AntigravityDetectionSource, CompositeDetectionSource, JsonlDetectionSource,
};
use moonlight_domain::ids::SessionId;
use moonlight_domain::phase::Phase;
use moonlight_domain::ports::mcp::McpActor;
use moonlight_domain::ports::{
    ControlPort, DetectionSource, ManagedSessionStore, PermissionRequest, PolicyDecisionPoint,
    SessionChangeStore,
};
use moonlight_domain::review::Feedback;
use moonlight_domain::session::SessionStatus;
use moonlight_domain::trust::{DangerClass, McpVerb, TrustTier};
use moonlight_engine::{Command, EngineEvent, EventBus, SessionSupervisor};
use moonlight_mcp_server::{
    ActorService, BusPolicyView, ControlApiState, McpHost, ShellVerbExecutor, StoreAuditSink,
};
use moonlight_persistence::Store;
use moonlight_trust::DefaultPdp;
use tokio::sync::{broadcast, mpsc};

use views::active_context::ActiveContext;
use views::active_editor::ActiveEditor;
use views::center_requests::CenterRequests;
use views::edit_gate::EditGate;
use views::editor_commands::EditorCommands;
use views::mcp_host::McpHostHandle;
use views::notifications::Notifications;
use views::obs_store::ObsStore;
use views::project_space::ProjectSpace;
use views::workspace::{init_shell, ShellDeps, Workspace};

/// How often the detection adapter is polled for new transcript activity.
const DETECTION_INTERVAL: Duration = Duration::from_millis(750);

fn main() {
    // CLI subcommands (handled before any GUI/tracing init):
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        // Agent CLIs invoke `moonlight hook <event>` per tool call. Fail-open: any
        // error path just allows the action. Claude sends `pre-tool-use`; the AGY
        // plugin sends `agy-pre-tool-use` (different payload shape + verdict format).
        Some("hook") => {
            return match args.get(2).map(String::as_str) {
                Some("agy-pre-tool-use") => run_hook_agy(),
                _ => run_hook(),
            };
        }
        // `moonlight hooks {install,uninstall,status} [claude|agy|all]` manages hook
        // registration for both backends.
        Some("hooks") => return hook_install::run(&args),
        // CC's `statusLine` command for app-launched sessions: capture obs + echo a line.
        Some("statusline") => return run_statusline(),
        _ => {}
    }

    moonlight_core::init_tracing();
    install_panic_hook();
    tracing::info!("MoonlightCode starting â dockable IDE shell");

    startup_self_check();

    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(|cx| {
            // Brand the macOS Dock with our app icon (runtime call; works for the bare
            // `cargo run` binary that has no `.app` bundle). Main thread, no-op elsewhere.
            icon::set_app_icon();

            // Must precede any gpui-component usage.
            gpui_component::init(cx);
            // Brand the gpui-component chrome (dock tabs, title bar, scrollbars, editor)
            // to our dark palette — it otherwise initializes in light mode.
            views::theme::install(cx);

            // Engine wiring: the supervisor owns the fleet and publishes facts on the
            // bus; the UI mirrors it via a bus subscription (single source of truth,
            // single engineâUI channel). `ObserveOnlyControl` is the L0 control port.
            let bus = EventBus::new(256);

            // Durable store of managed-session identity + the audit log. Shared between
            // the supervisor (refreshes managed state + appends audit) and the UI shell
            // (consults it on layout-restore to re-resume managed sessions embedded).
            let store = open_store();
            // The same connection, seen through each of its two ports.
            let managed: Arc<dyn ManagedSessionStore> = store.clone();
            let changes: Arc<dyn SessionChangeStore> = store.clone();
            // Steering channel (Option C): the `SteerControl` adapter queues injected
            // feedback (rejection-as-feedback, FR18-20) here; `run_steer_drain` writes it
            // into the target session's embedded terminal — the engine can't reach the
            // PTY, so delivery stays behind the port but is actuated by the UI.
            let (steer_tx, steer_rx) = mpsc::unbounded_channel::<Feedback>();
            // Same sender, kept so the control API can report whether a reject's
            // feedback can land instead of claiming success for a dropped message.
            let steer_probe = steer_tx.clone();
            // Opt-in auto-adoption (allowlist, empty by default) — the same
            // `~/.moonlight/config.json` key the daemon reads, so the two can't
            // disagree about which trees are governed automatically.
            let auto_adopt = std::env::var_os("HOME")
                .map(moonlight_control::auto_adopt_roots)
                .unwrap_or_default();
            if !auto_adopt.is_empty() {
                tracing::info!(roots = ?auto_adopt, "auto-adopt is configured for these roots");
            }
            let supervisor = SessionSupervisor::with_store(
                Arc::new(SteerControl::new(steer_tx)),
                bus.clone(),
                Some(managed.clone()),
            )
            .with_auto_adopt_roots(auto_adopt);

            // Control gate: a shared per-session read-model the hook `ControlServer`
            // reads. It is kept in sync from the bus below; the server itself runs on
            // its own tokio runtime thread (Unix-socket I/O needs tokio's reactor,
            // which GPUI's executor does not provide).
            let gate_view: GateView = Arc::new(RwLock::new(HashMap::new()));
            // Held-approval keystone: the server registers pending approvals here and
            // notifies the app over the bus; the engine loop resolves them when the
            // operator approves/denies in the cockpit (same registry, both runtimes).
            let pending = Arc::new(PendingApprovals::new());
            // One bus-backed notifier shared by the hook server and the MCP approval
            // gate — both surface their holds through the same cockpit affordance.
            let notifier: Arc<dyn ApprovalNotifier> = Arc::new(BusNotifier { bus: bus.clone() });
            // Runtime "always allow" overlay: the control server reads it to vouch a held
            // external-MCP tool; the command router (below) appends to it when the operator
            // clicks "always allow". Shared `Arc` so the decision takes effect immediately.
            let runtime_safe: RuntimeSafeTools = Arc::default();
            spawn_control_server(
                gate_view.clone(),
                pending.clone(),
                notifier.clone(),
                runtime_safe.clone(),
                managed.clone(),
                changes.clone(),
            );

            // Fold engine facts into the gate read-model (on GPUI's executor).
            {
                let mut rx = bus.subscribe();
                let gate_view = gate_view.clone();
                cx.spawn(async move |_cx| loop {
                    match rx.recv().await {
                        Ok(event) => moonlight_mcp_server::apply_gate_event(&gate_view, &event),
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                })
                .detach();
            }

            // Operator intents (e.g. adoption) flow UI â supervisor over this channel.
            // Deliver injected feedback (Option C): drain the steer channel into the
            // target sessions' embedded terminals. `ShellDeps` (the session→terminal
            // registry) is installed when the window opens; until then `recv` just waits.
            cx.spawn(async move |cx| run_steer_drain(steer_rx, cx.clone()).await)
                .detach();

            // MCP actor host (Slice 3): one embedded HTTP MCP server per managed
            // session, sharing the single PDP, the durable audit store, and the
            // held-approval keystone. The policy view is folded from the bus (below)
            // so the actor always gates against the operator's *current* phase/trust.
            let mcp_policy = Arc::new(BusPolicyView::new());
            {
                let mut rx = bus.subscribe();
                let policy = mcp_policy.clone();
                cx.spawn(async move |_cx| loop {
                    match rx.recv().await {
                        Ok(event) => policy.apply(&event),
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                })
                .detach();
            }
            // Seed the policy view from the durable store so the actor can resolve a
            // managed session immediately — independent of whether the fold above catches
            // that session's live `SessionUpserted` (it may have been published before the
            // subscription, or dropped on a `Lagged` burst at boot hydrate). Runs *after*
            // the subscription so a live upsert during seeding still wins (merge-only).
            if let Ok(records) = store.all_managed() {
                mcp_policy.seed_from_managed(&records);
            }

            // Fleet read-model for the control API's `/control/discoverable-sessions` —
            // the only place an *unadopted*, merely-detected session is visible (it has
            // no store row until adopted). No separate store seed needed: the engine
            // loop's `hydrate_from_store()` republishes every persisted session as a
            // `SessionUpserted` on this same bus at boot, which this fold catches as
            // long as the subscription (started here) precedes it — it does.
            let control_fleet = Arc::new(moonlight_mcp_server::BusFleetView::new());
            {
                let mut rx = bus.subscribe();
                let fleet = control_fleet.clone();
                cx.spawn(async move |_cx| loop {
                    match rx.recv().await {
                        Ok(event) => fleet.apply(&event),
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                })
                .detach();
            }
            // The run registry backs both the Run console (UI) and the MCP run verbs —
            // one shared state, so operator- and agent-started runs land in one console.
            // The executor stack: run verbs hit the registry; everything else falls
            // through to the shell executor (run_with_coverage).
            // Operator command channel, built **before** the actor so the MCP phase verb
            // can emit phase transitions onto the very same channel the cockpit's own phase
            // controls use (the engine loop drains it below). `commands` is moved into the
            // shell at window open; the executor keeps a clone.
            let (commands, command_rx) = mpsc::unbounded_channel::<Command>();

            // App-wide control API (the review queue + gating status over HTTP, for
            // external clients like an editor extension — distinct from the per-session
            // embedded MCP endpoint above). Discovery file, not a fixed port: the
            // listener binds ephemerally and writes its port to `~/.moonlight/control.json`.
            spawn_control_api(
                commands.clone(),
                managed.clone(),
                changes.clone(),
                steer_probe.clone(),
                control_fleet.clone(),
                bus.clone(),
            );

            let run_registry = run::RunRegistry::new();
            // Shared HTTP call history — the `http_request` executor records into it; the
            // Services view's HTTP summary polls it (same one-state-two-worlds shape as the
            // run registry).
            let http_history = http::HttpHistory::new();
            // Operator's auto-phasing toggle (off by default): a shared flag the main
            // toolbar flips and the actor reads — when on, a `request_phase` Prompt is
            // auto-approved instead of waiting on the cockpit gate.
            let auto_phase = Arc::new(std::sync::atomic::AtomicBool::new(false));
            // Executor stack (outer → inner): phase verb (request_phase) → http_request →
            // run verbs → the shell-backed run_with_coverage. Each layer handles its own
            // verbs and delegates the rest.
            let actor: Arc<dyn McpActor> = Arc::new(
                ActorService::new(
                    Arc::new(DefaultPdp),
                    mcp_policy.clone(),
                    Arc::new(moonlight_mcp_server::phase_verbs::PhaseVerbExecutor::new(
                        commands.clone(),
                        mcp_policy.clone(),
                        Arc::new(http_verbs::HttpVerbExecutor::new(
                            http_history.clone(),
                            Arc::new(run_verbs::RunVerbExecutor::new(
                                run_registry.clone(),
                                Arc::new(ShellVerbExecutor::new(mcp_test_command())),
                            )),
                        )),
                    )),
                    Arc::new(StoreAuditSink::new(managed.clone())),
                    // `None` = hold MCP-verb approvals until the operator decides (no auto-deny
                    // — a human approval must not be rushed; see KeystoneApprovalGate).
                    Arc::new(KeystoneApprovalGate::new(
                        pending.clone(),
                        notifier.clone(),
                        None,
                    )),
                )
                .with_auto_phase(auto_phase.clone()),
            );
            let mcp_host = McpHostHandle::build(McpHost::new(actor, mcp_policy));

            // Clones the engine loop owns: it resolves held approvals against `pending`
            // and republishes the resumed status on `bus`.
            let loop_pending = pending.clone();
            let loop_bus = bus.clone();
            // The engine loop also owns a handle to the runtime "always allow" overlay so
            // an `AuthorizeAlwaysTool` command can vouch a pattern (and persist it).
            let loop_runtime_safe = runtime_safe.clone();
            // The shell shares the same store so a restored managed-session tab can be
            // re-resumed (see the SessionMonitor registry arm in `views::workspace`).
            let shell_store = managed.clone();
            // The review surface reads the ledger the hook server writes.
            let shell_changes = changes.clone();

            cx.spawn(async move |cx| {
                cx.open_window(window_options(), |window, cx| {
                    // Build the shared UI state and register dockable panels before any
                    // panel is constructed. `ProjectSpace` is what the file-tree/terminal
                    // rails follow when the operator focuses a session.
                    let focus = cx.new(|_| ProjectSpace::load());
                    let center = cx.new(|_| CenterRequests);
                    let edit_gate = cx.new(|cx| EditGate::new(bus.subscribe(), cx));
                    let active_editor = cx.new(|_| ActiveEditor::default());
                    let active_context = cx.new(|_| ActiveContext::default());
                    let notifications = cx.new(|_| Notifications::default());
                    let editor_commands = cx.new(|_| EditorCommands);
                    let obs_store = cx.new(|_| ObsStore::default());
                    init_shell(
                        cx,
                        focus,
                        bus.clone(),
                        commands,
                        center,
                        edit_gate,
                        active_editor,
                        active_context,
                        notifications,
                        editor_commands,
                        obs_store,
                        shell_store,
                        shell_changes,
                        mcp_host,
                        run_registry,
                        http_history,
                        auto_phase,
                    );

                    let workspace = cx.new(|cx| Workspace::new(window, cx));
                    // The first level inside the window must be a `Root`.
                    cx.new(|cx| Root::new(workspace, window, cx))
                })
                .expect("failed to open MoonlightCode window");

                // Live data + operator commands: poll the detection adapter and drain the
                // command channel, driving both through the supervisor (detection/commands
                // â engine â bus â UI). Reads the operator's real `~/.claude/projects`.
                run_engine_loop(
                    supervisor,
                    // Observe BOTH backends: Claude's `~/.claude/projects` and AGY's
                    // `~/.gemini/antigravity-cli/brain`.
                    CompositeDetectionSource::new(vec![
                        Box::new(JsonlDetectionSource::default()),
                        Box::new(AntigravityDetectionSource::default()),
                    ]),
                    command_rx,
                    loop_pending,
                    loop_bus,
                    loop_runtime_safe,
                    cx.clone(),
                )
                .await;
            })
            .detach();
        });
}

/// Drive the supervisor from two inbound sources: the detection adapter (polled
/// every [`DETECTION_INTERVAL`] on GPUI's own timer) and operator commands from the
/// UI. Wakes early to service a command rather than waiting for the next poll tick.
/// A failing poll is logged and retried on the next tick.
async fn run_engine_loop(
    mut supervisor: SessionSupervisor,
    source: impl DetectionSource,
    mut command_rx: mpsc::UnboundedReceiver<Command>,
    pending: Arc<PendingApprovals>,
    bus: EventBus,
    runtime_safe: RuntimeSafeTools,
    cx: gpui::AsyncApp,
) {
    // Rehydrate the managed fleet from the durable store before the first poll, so a
    // restart shows the operator's previously-launched sessions immediately instead of
    // an empty grid (the grid only grows from live bus deltas). The window — and thus
    // the fleet grid's bus subscription — is already built above, so these upserts are
    // received. Idempotent, so it is harmless if detection later re-observes a session.
    supervisor.hydrate_from_store();
    loop {
        // Apply any queued operator commands first.
        while let Ok(command) = command_rx.try_recv() {
            dispatch_command(&mut supervisor, &pending, &bus, &runtime_safe, command).await;
        }
        // Then reconcile against the detection source.
        match source.poll().await {
            Ok(events) => {
                for event in events {
                    supervisor.on_detection(event).await;
                }
            }
            Err(err) => tracing::warn!(error = %err, "detection poll failed"),
        }
        // Wait for the next poll tick, but wake early to service a command.
        tokio::select! {
            _ = cx.background_executor().timer(DETECTION_INTERVAL) => {}
            Some(command) = command_rx.recv() => {
                dispatch_command(&mut supervisor, &pending, &bus, &runtime_safe, command).await;
            }
        }
    }
}

/// Deliver injected feedback (Option C) into the target session's embedded
/// terminal. The `SteerControl` adapter (held by the supervisor) queues `Feedback`
/// here; this drainer — on GPUI's executor, where the terminals live — looks the
/// session up in the [`ShellDeps::session_io`] registry and writes the message as
/// an operator steer. A session with no live terminal drops the message with a
/// warning (the `inject` already succeeded from the engine's view: it was queued).
async fn run_steer_drain(mut rx: mpsc::UnboundedReceiver<Feedback>, cx: gpui::AsyncApp) {
    while let Some(feedback) = rx.recv().await {
        let _ = cx.update(|cx| {
            let Some(io) = cx.try_global::<ShellDeps>().map(|d| d.session_io.clone()) else {
                tracing::warn!(session = %feedback.session_id, "steer feedback dropped — shell not ready");
                return;
            };
            match io.terminal(&feedback.session_id) {
                Some(term) => {
                    // As a paste, not raw text: a batched review is many lines, and
                    // each `\n` would otherwise submit a separate truncated turn.
                    let _ = term.update(cx, |t, _| t.send_paste(&feedback.message));
                }
                None => tracing::warn!(
                    session = %feedback.session_id,
                    "steer feedback dropped — no live terminal for this session"
                ),
            }
        });
    }
}

/// Route one operator command: an approve/deny that resolves a *held* approval is
/// handled by the keystone (the control server's hook is waiting on it); anything
/// else goes to the supervisor. Keeping this split here means the held-hook path
/// never touches the `ControlPort` (which can't reach the blocked hook anyway).
async fn dispatch_command(
    supervisor: &mut SessionSupervisor,
    pending: &PendingApprovals,
    bus: &EventBus,
    runtime_safe: &RuntimeSafeTools,
    command: Command,
) {
    // The durable half of "always allow" (a user-config write) is a filesystem side
    // effect kept out of the unit-tested `route_approval` core — do it here.
    if let Command::AuthorizeAlwaysTool { pattern, .. } = &command {
        persist_always_allow(pattern);
    }
    if let Some(command) = route_approval(pending, bus, runtime_safe, command) {
        supervisor.handle_command(command).await;
    }
}

/// Persist an "always allow" tool pattern to the user config (`~/.moonlight/config.json`)
/// so it survives a restart. Best-effort: the runtime overlay already gives the decision
/// immediate effect, so a missing `HOME` or a write error only costs durability (logged).
fn persist_always_allow(pattern: &str) {
    match std::env::var_os("HOME") {
        Some(home) => {
            let path = user_config_path(home);
            if let Err(e) = append_safe_tool(&path, pattern) {
                tracing::warn!(error = %e, %pattern,
                    "failed to persist always-allow to user config (still active this session)");
            }
        }
        None => {
            tracing::warn!(%pattern, "no HOME — always-allow not persisted (active this session)")
        }
    }
}

/// If `command` resolves a pending held approval, resolve it (and republish the
/// session as running) and return `None`. Otherwise return the command unchanged
/// for the supervisor to handle. Pure except for the registry/bus/config side effects,
/// so the routing logic is unit-testable.
///
/// `AuthorizeAlwaysTool` is the "always allow" decision on a held external-MCP
/// authorization: it vouches the pattern in the runtime overlay (immediate effect) and
/// persists it to the user config (durable), then approves the held call. The persist is
/// best-effort — a failed write still leaves the vouch active for the session.
fn route_approval(
    pending: &PendingApprovals,
    bus: &EventBus,
    runtime_safe: &RuntimeSafeTools,
    command: Command,
) -> Option<Command> {
    if let Command::AuthorizeAlwaysTool { session, pattern } = &command {
        // Vouch the pattern for the rest of the session (immediate effect); the durable
        // user-config write is done by the caller (`dispatch_command`).
        runtime_safe
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(pattern.clone());
        // Approve the held call now; the overlay makes future calls pass without a prompt.
        if pending.resolve(session, Decision::Approve) {
            bus.publish(EngineEvent::SessionStateChanged {
                session: session.clone(),
                status: SessionStatus::Running,
            });
        }
        return None;
    }

    let (session, decision) = match &command {
        Command::ApproveAction { session } => (session.clone(), Decision::Approve),
        Command::DenyAction { session, reason } => (
            session.clone(),
            Decision::Deny {
                reason: reason.clone(),
            },
        ),
        _ => return Some(command),
    };

    if pending.resolve(&session, decision) {
        // The held hook now returns; the session is no longer waiting on us.
        bus.publish(EngineEvent::SessionStateChanged {
            session,
            status: SessionStatus::Running,
        });
        None
    } else {
        // No held approval for this session â fall back to the supervisor's
        // existing approve/deny behaviour (feedback injection, resume).
        Some(command)
    }
}

/// Open the durable store under Application Support, falling back to an ephemeral
/// in-memory store if the file can't be opened (no home dir, a read-only disk, …)
/// so the app still runs — it just won't survive a restart.
///
/// Returns the concrete [`Store`] rather than one port object because it backs
/// **two** ports — [`ManagedSessionStore`] and [`SessionChangeStore`] — and they
/// must share one connection; callers coerce to whichever port they need.
fn open_store() -> Arc<Store> {
    let opened = store_path()
        .ok_or_else(|| "no home dir for store path".to_string())
        .and_then(|path| {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            Store::open(&path).map_err(|e| e.to_string())
        });
    match opened {
        Ok(store) => Arc::new(store),
        Err(err) => {
            tracing::warn!(error = %err, "opening managed-session store failed — using in-memory");
            Arc::new(Store::open_in_memory().expect("in-memory SQLite store"))
        }
    }
}

/// On-disk location of the managed-session database, beside the dock layout in
/// the platform state directory. `pub(crate)`: the workspace's DB observer (right
/// dock) opens this same database by default.
pub(crate) fn store_path() -> Option<PathBuf> {
    support::support_path("moonlight.db")
}

/// Path of the control IPC socket the hook CLI and server rendez-vous on.
/// `pub(crate)`: the Services tool window probes it for its status lamp.
pub(crate) fn control_socket_path() -> PathBuf {
    moonlight_dir().join("control.sock")
}

/// Discovery file for the control API's ephemeral port (see [`spawn_control_api`]).
/// An external client (an editor extension) reads this to find the API — there is
/// no fixed port, so nothing else could know it. Same file (and same resolver) the
/// headless `moonlightd` daemon writes, so a client never has to know which of the
/// two is actually running.
fn control_api_discovery_path() -> PathBuf {
    moonlight_dir().join("control.json")
}

/// `~/.moonlight`, via the resolver every MoonlightCode process shares (see
/// `moonlight_core::support::moonlight_dir`) — falls back to a relative path only
/// when `$HOME` is unset, matching this function's previous behavior.
fn moonlight_dir() -> PathBuf {
    moonlight_core::support::moonlight_dir().unwrap_or_else(|| PathBuf::from(".moonlight"))
}

/// On-disk crash log, next to the dock layout in the platform state directory.
fn crash_log_path() -> Option<PathBuf> {
    support::support_path("crash.log")
}

/// Install a global panic hook: log the message + location + backtrace via `tracing`
/// and append a timestamped entry to `<support>/crash.log`, then chain to the previous
/// hook. A panic on GPUI's main thread (e.g. a re-entrant entity update, or a failure
/// deep in restore) takes the whole process down with no native trace — this gives a
/// durable post-mortem so the next launch isn't a mystery. Best-effort throughout.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("<non-string panic payload>");
        tracing::error!(location = %location, message = %message, "PANIC");
        if let Some(path) = crash_log_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                use std::io::Write;
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                let _ = writeln!(
                    file,
                    "=== panic @ {now_ms}ms — {location}\n{message}\n{backtrace}\n"
                );
            }
        }
        default(info);
    }));
}

/// Build the per-cwd AI-workspace allowlist resolver from the user-level config
/// (`~/.moonlight/config.json`, empty when absent). Each session's workspace config is
/// then layered on per request by the resolver inside the control server.
fn ai_workspace_resolver() -> AiWorkspaceResolver {
    let user = std::env::var_os("HOME")
        .and_then(|home| load_config(&user_config_path(home)))
        .unwrap_or_default();
    AiWorkspaceResolver::layered(user)
}

/// The test command the MCP actor's `run_with_coverage` shells in a session's repo:
/// `test_command` from `~/.moonlight/config.json` (same file as the AI-workspace
/// allowlist; unknown keys there are ignored both ways), defaulting to `cargo test`.
fn mcp_test_command() -> Vec<String> {
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct TestCommandConfig {
        test_command: Vec<String>,
    }
    let configured = std::env::var_os("HOME")
        .map(user_config_path)
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<TestCommandConfig>(&bytes).ok())
        .map(|cfg| cfg.test_command)
        .unwrap_or_default();
    if configured.is_empty() {
        vec!["cargo".into(), "test".into()]
    } else {
        configured
    }
}

/// Publishes held-approval requests onto the engine bus so the cockpit can surface
/// them. Lives in the composition root because `control` (where the server runs)
/// stays free of the engine/event types â it only knows the [`ApprovalNotifier`]
/// trait. Emitting `PlanProposed` here makes the hook (not the JSONL tail) the
/// source of the plan text, so the plan tab opens with no polling race.
struct BusNotifier {
    bus: EventBus,
}

impl ApprovalNotifier for BusNotifier {
    fn approval_requested(
        &self,
        session: &SessionId,
        what: &str,
        plan: Option<&str>,
        mcp_tool: Option<&str>,
    ) {
        if let Some(plan) = plan {
            self.bus.publish(EngineEvent::PlanProposed {
                session: session.clone(),
                plan: plan.to_string(),
            });
        }
        self.bus.publish(EngineEvent::ApprovalRequested {
            session: session.clone(),
            what: what.to_string(),
            authorize_tool: mcp_tool.map(str::to_string),
        });
        // Float the tile into the "needs you" queue while it's blocked on us.
        self.bus.publish(EngineEvent::SessionStateChanged {
            session: session.clone(),
            status: SessionStatus::WaitingInput,
        });
    }
}

/// Run the hook `ControlServer` on a dedicated tokio runtime thread. Unix-socket
/// I/O requires tokio's reactor; the rest of the app runs on GPUI's executor. The
/// server holds approval-required actions in `pending` and announces them via the
/// bus (so the operator can approve/deny in the cockpit).
fn spawn_control_server(
    gate_view: GateView,
    pending: Arc<PendingApprovals>,
    notifier: Arc<dyn ApprovalNotifier>,
    runtime_safe: RuntimeSafeTools,
    store: Arc<dyn ManagedSessionStore>,
    changes: Arc<dyn SessionChangeStore>,
) {
    std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            tracing::error!("control server: failed to build tokio runtime");
            return;
        };
        rt.block_on(async move {
            let path = control_socket_path();
            let listener = match moonlight_control::bind_singleton_unix_socket(&path).await {
                Some(l) => l,
                None => return,
            };
            let store_clone = store.clone();
            let resolver = Arc::new(move |conversation_id: &str| {
                if let Ok(all) = store_clone.all_managed() {
                    all.into_iter()
                        .find(|m| m.conversation_id.as_deref() == Some(conversation_id))
                        .map(|m| m.id)
                } else {
                    None
                }
            });
            Arc::new(
                ControlServer::with_approvals(
                    gate_view,
                    Arc::new(DefaultPdp),
                    pending,
                    notifier,
                    // `None` = hold plan/danger approvals until the operator
                    // decides (no auto-deny). CC's own per-hook timeout (set high
                    // at install) is the only ceiling.
                    None,
                )
                .with_ai_resolver(ai_workspace_resolver())
                .with_runtime_safe(runtime_safe)
                .with_id_resolver(resolver)
                // Every allowed file write is recorded here, with the file's
                // pre-image, so the review surface can show what THIS session
                // changed instead of whatever is dirty in the tree.
                .with_change_ledger(changes)
                // …and git makes the writes that never named a file — a
                // `sed -i`, a redirect, a formatter — attributable too.
                .with_workspace_probe(Arc::new(crate::git_probe::GitProbe)),
            )
            .serve(listener)
            .await;
        });
    });
}

/// Run the app-wide control API (`crates/mcp-server::control_api`) on a dedicated
/// tokio runtime thread, same rationale as [`spawn_control_server`]: it needs
/// tokio's reactor, which GPUI's executor does not provide. Binds an ephemeral
/// loopback port and writes it to [`control_api_discovery_path`] so an external
/// client (an editor extension) can find it — overwritten on every start, so a
/// stale entry from a prior run is never left behind for long.
fn spawn_control_api(
    commands: mpsc::UnboundedSender<Command>,
    sessions: Arc<dyn ManagedSessionStore>,
    changes: Arc<dyn SessionChangeStore>,
    // A clone of the steering sender, used only to answer "can a reject's feedback
    // actually reach the session?" — it is closed once `run_steer_drain`'s receiver
    // is gone, i.e. once nothing can write into a terminal any more.
    steer_closed: mpsc::UnboundedSender<Feedback>,
    fleet: Arc<moonlight_mcp_server::BusFleetView>,
    // The engine bus, streamed to clients over `/control/events`. The same bus the
    // cockpit subscribes to, so an editor extension and the desktop UI never see a
    // different view of what the fleet is doing.
    events: EventBus,
) {
    std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            tracing::error!("control API: failed to build tokio runtime");
            return;
        };
        rt.block_on(async move {
            let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!(error = %e, "control API bind failed");
                    return;
                }
            };
            let port = match listener.local_addr() {
                Ok(addr) => addr.port(),
                Err(e) => {
                    tracing::error!(error = %e, "control API: no local address");
                    return;
                }
            };
            let path = control_api_discovery_path();
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(&path, serde_json::json!({ "port": port }).to_string()) {
                tracing::warn!(error = %e, "control API: failed to write discovery file");
            }
            tracing::info!(port, "control API listening");
            let state = ControlApiState::new(
                commands,
                sessions,
                changes,
                moonlight_control::hook_status_entries,
                // The cockpit holds the steer receiver for as long as it can write
                // into a session's embedded terminal; if it has gone, say so rather
                // than reporting a delivery that silently didn't happen.
                move || {
                    if steer_closed.is_closed() {
                        moonlight_mcp_server::control_api::FeedbackDelivery::Undeliverable {
                            reason: "the cockpit is no longer accepting steering — \
                                     its terminal sink has gone away"
                                .into(),
                        }
                    } else {
                        moonlight_mcp_server::control_api::FeedbackDelivery::Queued
                    }
                },
                fleet,
                events,
            );
            if let Err(e) = moonlight_mcp_server::control_api::serve(state, listener).await {
                tracing::error!(error = %e, "control API stopped");
            }
        });
    });
}

/// Serve one Claude Code hook call -- the shared client in
/// `moonlight_control::run_hook_client` (identical behavior for every binary that
/// can answer hooks; see its doc comment).
fn run_hook() {
    moonlight_control::run_hook_client(&control_socket_path());
}

/// Serve one **Antigravity** (`agy`) `PreToolUse` hook call. Same control-server query
/// as [`run_hook`], but AGY's payload shape and verdict format differ: we normalize the
/// payload onto the shared [`HookRequest`] (see [`agy_hook`]) and emit AGY's
/// `{"decision": …, "systemMessage": …}` verdict. Fail-open (allow) on any error.
fn run_hook_agy() {
    use std::io::Read;

    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        emit_agy_allow();
        return;
    }
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&input) else {
        emit_agy_allow();
        return;
    };
    let request = agy_hook::normalize(&payload);
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        emit_agy_allow();
        return;
    };
    let response = rt.block_on(query_hook(
        &control_socket_path(),
        &request,
        moonlight_control::HOOK_TIMEOUT,
    ));

    match response {
        HookResponse::Deny { reason } => {
            // AGY's hook block schema is Claude's **classic** PreToolUse form:
            // `{"decision":"block","reason":…}`. NOT `{"decision":"deny",…}` — AGY does
            // not recognize "deny" and falls through to allow, so the tool would run.
            // `decision:"block"` is the load-bearing field; we carry the message in BOTH
            // `reason` (the binary's struct tag) and `systemMessage` (what OMA's hooks
            // emit) so it surfaces whichever AGY reads — extra keys are ignored.
            println!(
                "{}",
                serde_json::json!({
                    "decision": "block",
                    "reason": &reason,
                    "systemMessage": &reason,
                })
            );
        }
        // AGY registers us on `PreToolUse` only, so a `Context` response cannot
        // arrive here; allow rather than emit a Claude-shaped context block AGY
        // would not understand.
        HookResponse::Allow | HookResponse::Context { .. } => emit_agy_allow(),
    }
}

/// AGY's allow verdict. Emitted explicitly (AGY's own hooks do too) on the allow path
/// and on every fail-open error, so a broken or absent MoonlightCode never blocks `agy`.
fn emit_agy_allow() {
    println!("{}", serde_json::json!({ "decision": "allow" }));
}

/// CC's `statusLine` command for an app-launched session: read the statusline JSON
/// from stdin, drop a per-session obs snapshot the app polls (see [`obs`]), and then
/// **chain to the operator's own status line** (their global `statusLine`, e.g. the
/// OMC HUD) and echo its output — we hijacked `statusLine` per-session only to capture
/// obs, so the operator keeps their HUD in the embedded terminal. Falls back to our
/// own compact line if there's no chainable statusline. Fail-quiet throughout.
fn run_statusline() {
    use std::io::Read;

    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let obs = obs::ingest(&input, now_ms);
    if let Some((session_id, snap)) = &obs {
        obs::write_obs(session_id, snap);
    }
    // The operator's HUD wins; only fall back to our compact line if they have none.
    match obs::chain_statusline(&input) {
        Some(line) => println!("{line}"),
        None => {
            if let Some((_, snap)) = &obs {
                println!("{}", obs::render_line(snap));
            }
        }
    }
}

/// The product name a desktop environment shows for the window.
const APP_NAME: &str = "MoonlightCode";

/// Desktop-environment identity: the X11 `WM_CLASS` and the Wayland `app_id`.
/// A desktop matches this against the installed `.desktop` entry of the same
/// name (`packaging/linux/`) to find the app's icon and label, and groups our
/// windows under one taskbar entry.
const APP_ID: &str = "dev.moonlightcode.MoonlightCode";

fn window_options() -> WindowOptions {
    WindowOptions {
        // Transparent titlebar hosting the macOS traffic lights, so the workspace's
        // own main toolbar (JetBrains-style) occupies that row instead of a wasted
        // native title. `TitleBar::title_bar_options()` sets `appears_transparent` and
        // the traffic-light inset that the `TitleBar` chrome reserves 80px for.
        titlebar: Some(TitlebarOptions {
            // Linux WMs put this in the window list and Alt-Tab; without it the
            // window is labelled after the `moonlight` binary. macOS keeps hiding
            // it (`appears_transparent` sets `NSWindowTitleHidden`), but it still
            // names the window in the Window menu and Mission Control.
            title: Some(APP_NAME.into()),
            ..gpui_component::TitleBar::title_bar_options()
        }),
        app_id: Some(APP_ID.into()),
        // X11 only; the other platforms are served by `icon::set_app_icon` or the
        // `.desktop` entry (see `icon`).
        icon: icon::window_icon(),
        ..Default::default()
    }
}

/// Exercise the permission authority once so the domain graph is provably wired
/// before the UI starts. Keeps `control`/`trust` adapters honest at the root.
fn startup_self_check() {
    let pdp = DefaultPdp;
    let control = ObserveOnlyControl;
    tracing::info!(control_level = ?control.level(), "control surface ready");

    let demo = PermissionRequest {
        session: SessionId::new("demo"),
        phase: Phase::AutoImplement,
        trust_tier: TrustTier::ReadOnly,
        verb: Some(McpVerb::RunWithCoverage),
        danger: DangerClass::Safe,
        write_scope: None,
        prompt_on_project_freeze: false,
        description: "run tests with coverage".into(),
    };
    match pdp.decide(&demo) {
        Ok(outcome) => tracing::info!(?outcome, "PDP verdict for demo action"),
        Err(e) => tracing::error!(error = %e, "PDP evaluation failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_approval_resolves_held_approval_and_resumes() {
        let pending = PendingApprovals::new();
        let bus = EventBus::new(16);
        let mut bus_rx = bus.subscribe();
        let s1 = SessionId::new("s1");

        // A held approval is registered (the hook is waiting on this receiver).
        let mut hook_rx = pending.register(s1.clone());

        let handled = route_approval(
            &pending,
            &bus,
            &RuntimeSafeTools::default(),
            Command::ApproveAction {
                session: s1.clone(),
            },
        );

        // Consumed by the keystone — not forwarded to the supervisor.
        assert!(handled.is_none());
        assert_eq!(hook_rx.try_recv().unwrap(), Decision::Approve);
        // …and the session was republished as running.
        assert!(matches!(
            bus_rx.try_recv(),
            Ok(EngineEvent::SessionStateChanged {
                status: SessionStatus::Running,
                ..
            })
        ));
    }

    #[test]
    fn route_approval_deny_carries_reason() {
        let pending = PendingApprovals::new();
        let bus = EventBus::new(16);
        let s1 = SessionId::new("s1");
        let mut hook_rx = pending.register(s1.clone());

        let handled = route_approval(
            &pending,
            &bus,
            &RuntimeSafeTools::default(),
            Command::DenyAction {
                session: s1.clone(),
                reason: "revise".into(),
            },
        );

        assert!(handled.is_none());
        assert_eq!(
            hook_rx.try_recv().unwrap(),
            Decision::Deny {
                reason: "revise".into()
            }
        );
    }

    #[test]
    fn route_approval_without_a_hold_forwards_to_supervisor() {
        let pending = PendingApprovals::new();
        let bus = EventBus::new(16);
        let forwarded = route_approval(
            &pending,
            &bus,
            &RuntimeSafeTools::default(),
            Command::ApproveAction {
                session: SessionId::new("ghost"),
            },
        );
        assert!(matches!(forwarded, Some(Command::ApproveAction { .. })));
    }

    #[test]
    fn authorize_always_vouches_pattern_and_approves_held_call() {
        let pending = PendingApprovals::new();
        let bus = EventBus::new(16);
        let runtime_safe = RuntimeSafeTools::default();
        let s1 = SessionId::new("s1");
        let mut hook_rx = pending.register(s1.clone());

        let handled = route_approval(
            &pending,
            &bus,
            &runtime_safe,
            Command::AuthorizeAlwaysTool {
                session: s1.clone(),
                pattern: "mcp__phoenix__*".into(),
            },
        );

        // Consumed (never forwarded to the supervisor)…
        assert!(handled.is_none());
        // …the held call is approved…
        assert_eq!(hook_rx.try_recv().unwrap(), Decision::Approve);
        // …and the pattern is vouched in the runtime overlay for the rest of the session.
        assert!(runtime_safe.read().unwrap().contains("mcp__phoenix__*"));
    }
}
