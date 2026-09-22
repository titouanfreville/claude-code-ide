//! Split out of `apps/desktop` so the headless half of MoonlightCode can build
//! and run without the GPUI renderer.
//!
//! `moonlightd` — the headless control API *and* the hook `ControlServer`:
//! session discovery, adoption, the review queue, gating status, and — now — the
//! actual phase/trust enforcement, all with no GPUI dependency, so this builds and
//! runs anywhere the rest of the workspace does (including a machine without the
//! Metal Toolchain the desktop app's renderer needs).
//!
//! Reuses the exact same [`SessionSupervisor`] + detection sources apps/desktop
//! drives (so "what counts as a session, and how adoption works" can never drift
//! into two implementations), the exact same SQLite store + hook-registration
//! status check, and — as of this pass — the exact same [`ControlServer`] apps/
//! desktop hosts, bound to the exact same Unix socket via
//! [`moonlight_control::bind_singleton_unix_socket`], which refuses to hijack a
//! live one: whichever MoonlightCode process starts first hosts the gate, and a
//! later one **exits**. That socket is the single-instance lock for the whole
//! daemon, claimed before the control-API port and before the discovery file —
//! because IDEs now autostart this binary, so two of them opening at once race to
//! spawn it. A loser that kept running would republish `control.json` with its own
//! port and every client would then talk to a process holding none of the
//! approvals. All of it wired through the shared
//! resolvers in `moonlight-core` and `moonlight-control` so nothing here has its
//! own copy of "what counts as installed / adopted / gated" to drift out of sync.
//!
//! Also runs `moonlightd hooks {install,uninstall,status}` and `moonlightd hook
//! <event>` — the exact same shared implementation `moonlight hooks`/`moonlight
//! hook` use, so registering hooks from this binary (no desktop app installed at
//! all, say, on a headless machine) works identically.
//!
//! **Known gaps, deliberately not papered over:**
//! - **No operator UI to answer a held approval.** A plan review or danger-zone
//!   action normally *holds* until a human clicks Approve/Deny in the desktop
//!   cockpit. This daemon has no such UI, so it uses [`ControlServer::new`]'s
//!   default degraded posture — [`moonlight_control::DEFAULT_HOLD`] (45s) instead
//!   of an indefinite hold — so a held action fails safe (denies) instead of
//!   hanging until Claude Code's own hook timeout. A real approval surface (e.g.
//!   over this same control API) is a follow-up, not solved here.
//! - **Rejection-as-feedback needs a live session's PTY**, which only the desktop
//!   app owns (`SteerControl` → the embedded terminal). This daemon still records
//!   a reject (marks the file reviewed) but has nowhere to deliver the feedback
//!   *into* — see `moonlight_mcp_server::control_api`'s `reject` handler.

mod mcp_shim;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};

use moonlight_control::{bind_singleton_unix_socket, ControlServer, SteerControl};
use moonlight_detection::{
    AntigravityDetectionSource, CompositeDetectionSource, JsonlDetectionSource,
};
use moonlight_domain::phase::Verbs;
use moonlight_domain::ports::{DetectionSource, ManagedSessionStore, SessionChangeStore};
use moonlight_engine::{Command, EventBus, SessionSupervisor};
use moonlight_mcp_server::{control_api, BusFleetView, ControlApiState};
use moonlight_persistence::Store;
use moonlight_trust::DefaultPdp;

/// How often the detection adapter is polled for new transcript activity — same
/// interval `apps/desktop` uses.
const DETECTION_INTERVAL: Duration = Duration::from_millis(750);

fn main() {
    // CLI subcommands (handled before any tracing/service init, mirroring
    // apps/desktop's `main`): `moonlightd hook <event>` is what a registered hook
    // actually invokes per tool call; `moonlightd hooks {install,uninstall,status}`
    // manages that registration. Both delegate to `moonlight_control`, the exact
    // same implementation apps/desktop's `moonlight hooks`/`moonlight hook` use —
    // no `agy` (Antigravity) target here, that's desktop-app-only.
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("hook") => return moonlight_control::run_hook_client(&control_socket_path()),
        Some("hooks") => return hooks_cli(&args),
        // Spawned by Claude Code as an MCP server, for a session no IDE launched —
        // see `mcp_shim`. Handled here, before tracing init, because its stdout is the
        // MCP channel and must carry nothing but protocol frames.
        Some("mcp") => return mcp_shim::run(),
        // Answered rather than ignored: every unrecognised argument used to fall
        // through and start a full daemon, so `moonlightd --version` silently began
        // governing the machine's sessions.
        Some("--version" | "-V") => return println!("moonlightd {}", env!("CARGO_PKG_VERSION")),
        Some("--help" | "-h") => return print_usage(),
        Some(other) => {
            eprintln!("unknown argument: {other}");
            print_usage();
            // Non-zero: an IDE autostarts this binary, and something that exits 0
            // without becoming a daemon reads as "started fine" to whatever launched
            // it.
            std::process::exit(2);
        }
        None => {}
    }

    moonlight_core::init_tracing();
    tracing::info!("moonlightd starting — headless control API (no desktop/GPUI dependency)");

    let store = open_store();
    let sessions: Arc<dyn ManagedSessionStore> = store.clone();
    let changes: Arc<dyn SessionChangeStore> = store.clone();

    // Sized for the boot burst: hydrate plus detection replaying transcript history
    // publishes well past 256 before the read-model fold is first polled. Lag is still
    // handled below — this only makes it rare rather than routine.
    let bus = EventBus::new(4096);
    let (commands, command_rx) = mpsc::unbounded_channel::<Command>();
    // No PTY to deliver into — see the module doc. `SteerControl::inject_feedback`
    // reports `Unavailable` once the receiver drops, which the supervisor logs and
    // moves past (the same path apps/desktop takes when its UI isn't ready yet).
    //
    // The receiver is dropped *immediately and explicitly*. Binding it to a name —
    // even `_steer_rx` — keeps it alive for the whole of `main`, which silently
    // turns this into a black hole: the channel is unbounded, so every rejection
    // accumulates forever while `inject_feedback` returns `Ok` and the supervisor
    // audits the feedback as delivered. Closing it makes every layer tell the truth.
    let (steer_tx, steer_rx) = mpsc::unbounded_channel();
    drop(steer_rx);
    // Same sender, kept to answer "can a reject's feedback actually land?" over the
    // control API instead of reporting a bare success for a message that goes nowhere.
    let steer_closed = steer_tx.clone();
    // Opt-in auto-adoption (allowlist, empty by default) — see
    // `moonlight_control::auto_adopt_roots`. Read once at startup like the rest of
    // the user config.
    let auto_adopt = std::env::var_os("HOME")
        .map(moonlight_control::auto_adopt_roots)
        .unwrap_or_default();
    if !auto_adopt.is_empty() {
        tracing::info!(roots = ?auto_adopt, "auto-adopt is configured for these roots");
    }
    let supervisor = SessionSupervisor::with_store(
        Arc::new(SteerControl::new(steer_tx)),
        bus.clone(),
        Some(sessions.clone()),
    )
    .with_auto_adopt_roots(auto_adopt);

    // Fleet read-model for `/control/discoverable-sessions` — see `BusFleetView`'s
    // doc comment: an *unadopted*, merely-detected session lives only here, never
    // in the durable store.
    let fleet = Arc::new(BusFleetView::new());

    // The hook ControlServer's gate read-model — folded from the same bus, same
    // shape, same `apply_gate_event` fold apps/desktop uses.
    let gate_view: moonlight_control::GateView = Arc::new(RwLock::new(HashMap::new()));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async move {
        // The hook socket is this whole process's mutex, and it is claimed *first*:
        // before the control-API port, and before the discovery file. A second
        // daemon that loses the race must not have published itself as the one to
        // talk to — IDEs read `control.json`, so a loser that wrote it would send
        // every client to a process that governs nothing while the winner holds the
        // approvals.
        let Some(hook_listener) = bind_singleton_unix_socket(&control_socket_path()).await else {
            tracing::info!(
                "another MoonlightCode host already owns the control socket — nothing to do here"
            );
            return;
        };

        // Only the daemon that won the socket repairs the registrations: a loser is
        // about to exit, and two processes rewriting `~/.claude/settings.json` in the
        // same breath is how a config ends up naming the binary that is leaving.
        // Also decides what the per-turn brief may name: the verbs exist for a new
        // session exactly when this registration is in place.
        let verbs = repair_registrations();

        // `tokio::spawn` needs a runtime under it — everything that spawns a
        // background task lives inside this `block_on`, not before it.
        {
            let mut rx = bus.subscribe();
            let fleet = fleet.clone();
            let gate_view = gate_view.clone();
            let resync = commands.clone();
            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(event) => {
                            fleet.apply(&event);
                            moonlight_mcp_server::apply_gate_event(&gate_view, &event);
                        }
                        Err(broadcast::error::RecvError::Lagged(missed)) => {
                            // Dropped events are gone, and nothing republishes a
                            // session that then goes idle. Skipping them would leave
                            // this fold permanently wrong — and a gate view missing a
                            // session allows everything that session does. So ask the
                            // supervisor to republish and rebuild from that.
                            tracing::warn!(missed, "read-model lagged — resynchronising the fleet");
                            let _ = resync.send(Command::RepublishFleet);
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }
        tokio::spawn(run_supervisor_loop(supervisor, command_rx));

        // One approval path for both the hook gate and the MCP verbs: `present_plan`
        // and a held `PreToolUse` are the same question to the operator, and two
        // registries would mean a verdict answering one and not the other.
        let pending = Arc::new(moonlight_control::PendingApprovals::new());
        let notifier: Arc<dyn moonlight_control::ApprovalNotifier> =
            Arc::new(BusNotifier { bus: bus.clone() });

        let mcp_host = build_mcp_host(
            &bus,
            commands.clone(),
            store.clone(),
            pending.clone(),
            notifier.clone(),
        );
        let api_pending = pending.clone();

        tokio::spawn(run_control_server(
            hook_listener,
            gate_view,
            pending,
            notifier,
            changes.clone(),
            verbs,
        ));

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
        write_discovery_file(port);
        tracing::info!(port, "moonlightd listening");
        let state = ControlApiState::new(
            commands,
            sessions,
            changes,
            moonlight_control::hook_status_entries,
            // Asks the steer channel itself rather than hardcoding "headless can't
            // deliver": the receiver is dropped above, so this reports undeliverable
            // for the right reason, and would start reporting otherwise the day this
            // binary grows somewhere to deliver into.
            move || {
                if steer_closed.is_closed() {
                    control_api::FeedbackDelivery::Undeliverable {
                        reason: "moonlightd is headless — it has no terminal to write \
                                 feedback into. Run the desktop app to steer this session."
                            .into(),
                    }
                } else {
                    control_api::FeedbackDelivery::Queued
                }
            },
            fleet,
            bus.clone(),
        )
        // The same registry the gate holds on, so a client's verdict resolves the hook
        // actually waiting rather than a second, unrelated one.
        .with_pending(api_pending)
        .with_mcp_host(mcp_host);
        if let Err(e) = control_api::serve(state, listener).await {
            tracing::error!(error = %e, "control API stopped");
        }
        // Reached when the API stops on its own. A signal kill skips this, which is why
        // clients must still treat a stale file as possible — see `remove_discovery_file`.
        remove_discovery_file(port);
    });
}

/// Point Claude Code's hook registrations at *this* binary, at every start.
///
/// The daemon is expected to be launched by whichever IDE opened first, and an IDE
/// ships it in a versioned directory — so the path a previous install wrote is stale
/// as soon as the extension updates. Doing this on the way up means the first window
/// opened after an update repairs the gate, with no operator step and no CLI run.
///
/// Never fatal: a daemon that governs sessions with a stale hook registration is still
/// worth more than one that refused to start over a config file it could not rewrite.
fn repair_registrations() -> Verbs {
    match moonlight_control::ensure_registered() {
        Ok(outcomes) => {
            for (event, repair) in outcomes {
                match repair {
                    moonlight_control::Repair::Unchanged => {}
                    moonlight_control::Repair::Added => {
                        tracing::info!(%event, "registered the Claude Code hook")
                    }
                    moonlight_control::Repair::Refreshed => {
                        tracing::info!(%event, "repointed a stale Claude Code hook at this binary")
                    }
                    // Warn, not info: this event may have no working registration, and
                    // an unregistered gate fails open. Silence would read as health.
                    moonlight_control::Repair::Unreadable => tracing::warn!(
                        %event,
                        "the hook entry for this event is shaped unexpectedly — left alone; \
                         gating for it may be inactive"
                    ),
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "could not verify the Claude Code hook registrations"),
    }

    // The `mcpServers` half — what gives the moonlight verbs to a session no IDE
    // launched. Same repair rule, separate file (`~/.claude.json`), so it is reported
    // separately: a machine can perfectly well have working hooks and no verbs, which
    // is exactly the state this exists to end.
    match moonlight_control::ensure_mcp_registered() {
        Ok(moonlight_control::Repair::Unchanged) => Verbs::Available,
        Ok(moonlight_control::Repair::Added) => {
            tracing::info!("registered the moonlight MCP server for new Claude Code sessions");
            Verbs::Available
        }
        Ok(moonlight_control::Repair::Refreshed) => {
            tracing::info!("repointed the stale moonlight MCP server registration at this binary");
            Verbs::Available
        }
        Ok(moonlight_control::Repair::Unreadable) => {
            tracing::warn!("~/.claude.json holds an mcpServers value we cannot read — left alone");
            Verbs::Unavailable
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not verify the moonlight MCP registration");
            // Could not register, so cannot promise the verbs. The brief then tells a
            // session to say what it needs in prose, which is the honest fallback.
            Verbs::Unavailable
        }
    }
}

fn print_usage() {
    println!("usage: moonlightd                       run the control API and hook gate");
    println!("       moonlightd hooks [install|uninstall|status]");
    println!("       moonlightd hook <event>          invoked by a registered hook");
    println!(
        "       moonlightd mcp                   MCP server over stdio (spawned by Claude Code)"
    );
    println!("       moonlightd --version | --help");
}

/// `moonlightd hooks <install|uninstall|status>` — same shared implementation
/// `moonlight hooks` uses. No `agy` target: Antigravity support is desktop-app-only.
fn hooks_cli(args: &[String]) {
    match args.get(2).map(String::as_str) {
        Some("install") => moonlight_control::install_hooks(),
        Some("uninstall") => moonlight_control::uninstall_hooks(),
        Some("status") | None => moonlight_control::print_hook_status(),
        Some(other) => {
            eprintln!("unknown hooks subcommand: {other}");
            eprintln!("usage: moonlightd hooks [install|uninstall|status]");
        }
    }
}

/// Discover sessions (JSONL + Antigravity transcripts) and apply operator
/// commands — today just `SetAdopted`, sent by `POST /control/adopt`. The same two
/// inbound flows `apps/desktop`'s engine loop drives, minus anything that needs a
/// live terminal (steering, embedded MCP per session).
async fn run_supervisor_loop(
    mut supervisor: SessionSupervisor,
    mut command_rx: mpsc::UnboundedReceiver<Command>,
) {
    // Restores previously-adopted sessions from the store immediately, rather than
    // waiting for detection to re-observe them — same rationale as apps/desktop.
    supervisor.hydrate_from_store();
    let source = CompositeDetectionSource::new(vec![
        Box::new(JsonlDetectionSource::default()),
        Box::new(AntigravityDetectionSource::default()),
    ]);
    loop {
        while let Ok(command) = command_rx.try_recv() {
            supervisor.handle_command(command).await;
        }
        match source.poll().await {
            Ok(events) => {
                for event in events {
                    supervisor.on_detection(event).await;
                }
            }
            Err(err) => tracing::warn!(error = %err, "detection poll failed"),
        }
        tokio::select! {
            _ = tokio::time::sleep(DETECTION_INTERVAL) => {}
            Some(command) = command_rx.recv() => {
                supervisor.handle_command(command).await;
            }
        }
    }
}

/// Publishes approval requests onto the engine bus so a connected client can raise a
/// prompt. The daemon has no UI of its own; it is the clients that ask the operator.
struct BusNotifier {
    bus: EventBus,
}

impl moonlight_control::ApprovalNotifier for BusNotifier {
    fn approval_requested(
        &self,
        session: &moonlight_domain::ids::SessionId,
        what: &str,
        plan: Option<&str>,
        mcp_tool: Option<&str>,
    ) {
        // The plan rides its own event: `ApprovalRequested` has no field for it, and a
        // prompt with no plan to read cannot be answered.
        if let Some(plan) = plan {
            self.bus
                .publish(moonlight_engine::EngineEvent::PlanProposed {
                    session: session.clone(),
                    plan: plan.to_string(),
                });
        }
        self.bus
            .publish(moonlight_engine::EngineEvent::ApprovalRequested {
                session: session.clone(),
                what: what.to_string(),
                authorize_tool: mcp_tool.map(str::to_string),
            });
        self.bus
            .publish(moonlight_engine::EngineEvent::SessionStateChanged {
                session: session.clone(),
                status: moonlight_domain::session::SessionStatus::WaitingInput,
            });
    }
}

/// Build the MCP host that serves the **workflow verbs** to sessions.
///
/// Workflow only: `run_*` addresses an IDE's Run console, which a daemon does not have,
/// so offering those here would advertise tools that always fail. Each IDE serves those
/// itself — see `VerbScope`.
fn build_mcp_host(
    bus: &EventBus,
    commands: mpsc::UnboundedSender<Command>,
    store: Arc<moonlight_persistence::Store>,
    pending: Arc<moonlight_control::PendingApprovals>,
    notifier: Arc<dyn moonlight_control::ApprovalNotifier>,
) -> Arc<moonlight_mcp_server::McpHost> {
    let policy = Arc::new(moonlight_mcp_server::BusPolicyView::new());
    {
        let policy = policy.clone();
        let mut rx = bus.subscribe();
        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                policy.apply(&event);
            }
        });
    }

    let actor: Arc<dyn moonlight_domain::ports::mcp::McpActor> =
        Arc::new(moonlight_mcp_server::ActorService::new(
            Arc::new(DefaultPdp),
            policy.clone(),
            Arc::new(moonlight_mcp_server::phase_verbs::PhaseVerbExecutor::new(
                commands,
                policy.clone(),
                Arc::new(moonlight_mcp_server::UnsupportedVerbExecutor),
            )),
            Arc::new(moonlight_mcp_server::StoreAuditSink::new(store)),
            // `None` = wait for the operator indefinitely. An MCP-verb approval is a
            // human decision; bounding it would deny plans for being thought about.
            Arc::new(moonlight_control::KeystoneApprovalGate::new(
                pending, notifier, None,
            )),
        ));

    Arc::new(moonlight_mcp_server::McpHost::with_scope(
        actor,
        policy,
        moonlight_mcp_server::VerbScope::WorkflowOnly,
    ))
}

/// Host the hook `ControlServer` — the actual phase/trust enforcement, answering
/// `moonlight hook <event>` regardless of which binary's path was registered for
/// it (the hook client only needs the socket, not a specific server process; see
/// `bind_singleton_unix_socket`'s doc comment).
///
/// Takes an already-bound listener rather than binding here: claiming the socket is
/// what decides whether this process runs at all, so it happens in `main` before
/// anything else is started.
async fn run_control_server(
    listener: tokio::net::UnixListener,
    gate_view: moonlight_control::GateView,
    pending: Arc<moonlight_control::PendingApprovals>,
    notifier: Arc<dyn moonlight_control::ApprovalNotifier>,
    changes: Arc<dyn SessionChangeStore>,
    verbs: Verbs,
) {
    // `ControlServer::new` is the degraded-standalone posture: `NoopNotifier` +
    // `DEFAULT_HOLD` (45s) — see the module doc on why that's the right default
    // with no operator UI to answer a hold.
    let server = Arc::new(
        ControlServer::with_approvals(
            gate_view,
            Arc::new(DefaultPdp),
            pending,
            notifier,
            // `None` = hold until the operator answers. A client is expected to be
            // attached; the short degraded hold this used to take denied plans for
            // nobody having looked yet.
            None,
        )
        .with_ai_resolver(ai_workspace_resolver())
        .with_change_ledger(changes)
        // Attributes a shell command's writes (`sed -i`, a redirect, a
        // generated file) to the ledger — the same `GitProbe` apps/desktop
        // uses, shared via `moonlight-control` (see its doc comment on why
        // it's a separate,  minimal implementation from that crate's own
        // richer UI-facing git module).
        .with_workspace_probe(Arc::new(moonlight_control::GitProbe))
        // What the per-turn phase brief may tell a session it can call. Resolved once,
        // from whether the MCP registration is actually in place: naming `present_plan`
        // to a session that has no such tool costs it a turn and ends in a confused
        // retry, and saying it has "no tool to ask with" when it does is how an agent
        // sits in Plan writing prose at an operator who is waiting for a plan panel.
        .with_mcp_verbs(Arc::new(move |_| verbs)),
    );
    server.serve(listener).await;
}

/// `~/.moonlight/control.sock` — the same path (and same resolver) apps/desktop's
/// `control_socket_path` uses.
fn control_socket_path() -> std::path::PathBuf {
    moonlight_core::support::moonlight_dir()
        .unwrap_or_else(|| std::path::PathBuf::from(".moonlight"))
        .join("control.sock")
}

/// The AI-workspace allowlist from `~/.moonlight/config.json` — same resolution
/// apps/desktop's `ai_workspace_resolver` performs (duplicated rather than shared:
/// five lines, pure, no state to drift).
fn ai_workspace_resolver() -> moonlight_control::AiWorkspaceResolver {
    let user = std::env::var_os("HOME")
        .and_then(|home| moonlight_control::load_config(&moonlight_control::user_config_path(home)))
        .unwrap_or_default();
    moonlight_control::AiWorkspaceResolver::layered(user)
}

/// The same DB the desktop app opens (see `moonlight_core::support`) — one fleet,
/// seen by whichever process is running, never two.
fn open_store() -> Arc<Store> {
    let opened = moonlight_core::support::support_path("moonlight.db")
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

/// Same discovery file (and same `~/.moonlight` resolver) the desktop app's
/// `spawn_control_api` writes — a client never has to know which of the two
/// processes is actually running.
/// Remove the discovery file we published.
///
/// A `control.json` left behind by a dead daemon is worse than none: every client reads
/// it, dials a port nobody is listening on, and reports a backend error — when the
/// correct behaviour on finding no file is to autostart a daemon. Best-effort, and
/// guarded on the port: a second daemon may have started and republished since, and
/// deleting *its* file on our way out would strand every client.
fn remove_discovery_file(port: u16) {
    let Some(dir) = moonlight_core::support::moonlight_dir() else {
        return;
    };
    let path = dir.join("control.json");
    let ours = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get("port").and_then(serde_json::Value::as_u64))
        .is_some_and(|p| p == u64::from(port));
    if ours {
        let _ = std::fs::remove_file(&path);
        tracing::info!("removed the control API discovery file");
    }
}

fn write_discovery_file(port: u16) {
    let Some(dir) = moonlight_core::support::moonlight_dir() else {
        tracing::warn!("no $HOME — cannot write the control API discovery file");
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(error = %e, "control API: failed to create ~/.moonlight");
        return;
    }
    let path = dir.join("control.json");
    if let Err(e) = std::fs::write(&path, serde_json::json!({ "port": port }).to_string()) {
        tracing::warn!(error = %e, "control API: failed to write discovery file");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::sync::RwLock;
    use std::time::Duration;

    use moonlight_control::{query_hook, GateState, HookRequest, HookResponse};
    use moonlight_domain::ids::SessionId;
    use moonlight_domain::phase::Phase;
    use moonlight_domain::trust::TrustTier;
    use moonlight_persistence::Store;

    fn temp_socket_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("moonlightd-test-{}-{tag}.sock", std::process::id()))
    }

    fn edit_request(session_id: &str) -> HookRequest {
        HookRequest {
            event: "PreToolUse".to_string(),
            session_id: session_id.to_string(),
            tool_name: "Edit".to_string(),
            tool_input: serde_json::json!({ "file_path": "/repo/src/main.rs" }),
            cwd: "/repo".to_string(),
        }
    }

    /// End-to-end proof of this binary's own wiring (not `ControlServer`'s gating
    /// logic — that's already covered in `moonlight-control`'s own tests): a
    /// session this process considers adopted and in `Plan` is actually denied a
    /// project-file write over the real Unix socket `run_control_server` binds.
    #[tokio::test]
    async fn an_adopted_plan_session_is_denied_over_the_real_socket() {
        let path = temp_socket_path("denies");
        let _ = std::fs::remove_file(&path);
        let gate_view: moonlight_control::GateView = Arc::new(RwLock::new(HashMap::new()));
        gate_view.write().unwrap().insert(
            SessionId::new("s1"),
            GateState {
                phase: Phase::Plan,
                trust: TrustTier::Trusted,
                adopted: true,
                paused: false,
            },
        );
        let changes: Arc<dyn SessionChangeStore> =
            Arc::new(Store::open_in_memory().expect("in-memory store"));

        serve_gate(&path, gate_view, changes).await;

        let response = query_hook(&path, &edit_request("s1"), Duration::from_secs(5)).await;
        assert!(
            matches!(response, HookResponse::Deny { .. }),
            "{response:?}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// The PDP's day-one safety rule — an unadopted session is never denied — holds
    /// through this binary's wiring too.
    #[tokio::test]
    async fn an_unadopted_session_is_never_denied() {
        let path = temp_socket_path("allows");
        let _ = std::fs::remove_file(&path);
        let gate_view: moonlight_control::GateView = Arc::new(RwLock::new(HashMap::new()));
        gate_view.write().unwrap().insert(
            SessionId::new("s2"),
            GateState {
                phase: Phase::Plan,
                trust: TrustTier::Observed,
                adopted: false,
                paused: false,
            },
        );
        let changes: Arc<dyn SessionChangeStore> =
            Arc::new(Store::open_in_memory().expect("in-memory store"));

        serve_gate(&path, gate_view, changes).await;

        let response = query_hook(&path, &edit_request("s2"), Duration::from_secs(5)).await;
        assert_eq!(response, HookResponse::Allow, "{response:?}");

        let _ = std::fs::remove_file(&path);
    }

    /// Bind and serve the gate the way `main` does, and return once it answers.
    ///
    /// The bind is awaited here rather than inside the spawned task: a test that
    /// queried before the listener existed would connect to nothing and read the
    /// failure as a policy decision.
    async fn serve_gate(
        path: &std::path::Path,
        gate_view: moonlight_control::GateView,
        changes: Arc<dyn SessionChangeStore>,
    ) {
        let listener = bind_singleton_unix_socket(path)
            .await
            .expect("the test socket is free");
        let notifier: Arc<dyn moonlight_control::ApprovalNotifier> = Arc::new(BusNotifier {
            bus: EventBus::new(8),
        });
        tokio::spawn(run_control_server(
            listener,
            gate_view,
            Arc::new(moonlight_control::PendingApprovals::new()),
            notifier,
            changes,
            // These tests exercise gating, not the brief's wording; the verbs a real
            // daemon reports depend on a registration written to the operator's $HOME.
            Verbs::Unavailable,
        ));
        wait_for_socket(path).await;
    }

    async fn wait_for_socket(path: &std::path::Path) {
        for _ in 0..200 {
            if tokio::net::UnixStream::connect(path).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("control server never bound {path:?}");
    }
}
