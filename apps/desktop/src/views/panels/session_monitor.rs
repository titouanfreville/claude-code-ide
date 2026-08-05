//! Session monitor — the "focus mode" view for one session (center dock tab).
//!
//! Opening a session zooms into it here: a live read-model of that one session
//! (status, phase, mode, trust tier, attached path) fed by the engine `EventBus`,
//! beside the `Sessions` grid and any open files. A region is reserved for the
//! live terminal/transcript, which is engine-owned (FR10 session multiplexing) and
//! lands once that feed exists.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    actions, div, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla,
    MouseDownEvent, ScrollHandle, Task, WeakEntity, Window,
};
use gpui_component::dock::{Panel, PanelEvent, PanelInfo, PanelState, PanelView, TabPanel};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::PopupMenu;
use gpui_component::text::TextView;
use gpui_component::Placement;

use super::CloseTab;

actions!(
    moonlight_session,
    [SplitRight, SplitLeft, SplitUp, SplitDown]
);

use moonlight_domain::agent::AgentKind;
use moonlight_domain::ids::{SessionId, Timestamp};
use moonlight_domain::phase::Phase;
use moonlight_domain::session::{AttentionKind, Session, SessionStatus};
use moonlight_domain::trust::TrustTier;
use moonlight_engine::{Command, EngineEvent};
use tokio::sync::broadcast;

use super::terminal::TerminalPanel;
use crate::views::active_context::ActiveContext;
use crate::views::auto_compact;
use crate::views::center_requests::OpenRequest;
use crate::views::notifications::NotificationKind;
use crate::views::obs_store::ObsStore;
use crate::views::session_meta::{self, NameSource, SessionColor, SessionMeta, SessionMetaCache};
use crate::views::theme;
use crate::views::workspace::ShellDeps;

/// The prompt injected into a stalled session when its project opted into auto-resume
/// (see [`SessionMonitor::maybe_auto_resume`]). Phrased to make CC re-read its own
/// recent context and finish the in-flight work rather than start something new.
const AUTO_RESUME_PROMPT: &str =
    "You appear to have stopped mid-task. Re-read your recent context and continue where \
     you left off until the work is complete.";

/// How often a managed session polls its terminal for process exit (auto-resume watch).
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(1500);

/// Cap on auto-relaunches after a CC process exit, so a session whose CC dies on launch
/// can't relaunch in an unbounded loop. Beyond this the operator resumes manually.
const MAX_EXIT_RESUMES: u8 = 3;

/// Refusal reason sent to the agent when the operator refuses an external-MCP tool.
const MCP_REFUSE_REASON: &str =
    "External tool call refused by operator in this phase — request a phase change or use a different approach.";
/// Refusal reason sent when the operator refuses a non-tool held approval (a sensitive
/// phase change, a danger-zone command).
const ACTION_REFUSE_REASON: &str = "Refused by operator.";

/// A held operator authorization awaiting a verdict in this session's bottom-right
/// popup (the small overlay on the focus view — the once/always/refuse gate that used
/// to steal a whole center tab). Folded from [`EngineEvent::ApprovalRequested`] and
/// cleared once the session resumes / changes phase / is removed.
#[derive(Clone)]
enum PendingAuthorize {
    /// An external MCP tool a frozen phase blocked: once / always-tool / always-server /
    /// refuse. Carries the full `mcp__server__tool` name.
    McpTool(String),
    /// A non-tool held approval (a sensitive phase change, a danger-zone command): a
    /// plain approve / refuse. Carries the short `what` description shown in the prompt.
    Action(String),
}

/// The `mcp__server__*` glob for an external MCP tool name (the "always allow this
/// server" pattern). Falls back to the exact name when it isn't the expected
/// `mcp__server__tool` shape, so a malformed name never becomes an over-broad glob.
fn server_glob(tool: &str) -> String {
    let parts: Vec<&str> = tool.split("__").collect();
    if parts.len() >= 3 && parts[0] == "mcp" {
        format!("{}__{}__*", parts[0], parts[1])
    } else {
        tool.to_string()
    }
}

/// Friendly title + body for a non-tool held approval. A `request_phase` hold arrives
/// as `"RequestPhase <token>"` (the verb's Debug name + the payload), so render it as a
/// readable phase-change prompt; anything else (a danger-zone command) is shown verbatim.
fn action_prompt(what: &str) -> (&'static str, String) {
    if let Some(token) = what.strip_prefix("RequestPhase") {
        let token = token.trim();
        let target = if token.is_empty() || token.eq_ignore_ascii_case("next") {
            "the next phase".to_string()
        } else {
            Phase::from_token(token)
                .map(|p| p.label().to_string())
                .unwrap_or_else(|| token.to_string())
        };
        (
            "Authorize phase change",
            format!("The agent wants to switch to {target} (this grants project-file writes)."),
        )
    } else {
        ("Approve request", what.to_string())
    }
}

pub struct SessionMonitor {
    id: SessionId,
    /// Which agent CLI backend drives this session (`claude` / `agy`). Set at
    /// construction from the managed record (or the new-session request) and used
    /// by [`try_resume`](Self::try_resume) / [`relaunch_terminal`](Self::relaunch_terminal)
    /// so a resume relaunches the same backend it was created with.
    agent: AgentKind,
    /// Live read-model of the focused session; `None` until an upsert arrives
    /// (e.g. after layout rehydrate, where only the id is known).
    session: Option<Session>,
    /// For an **app-launched (managed) session**, the live terminal running
    /// `claude --session-id <id>`: its scrollback is the message history and typing
    /// into it is the dialog. `None` for discovered/external sessions (we didn't
    /// spawn their process, so there is no PTY to embed) — those show the read-only
    /// [`messages`](Self::messages) transcript instead.
    terminal: Option<Entity<TerminalPanel>>,
    /// Repo root for resuming an observed session (`claude --resume <id>`). `Some`
    /// when the session has a known repo; drives the "Resume in terminal" action,
    /// which is only allowed while the agent isn't actively working (resuming a
    /// running session would put two clients on one transcript). `None` once resumed.
    resume_root: Option<PathBuf>,
    /// Read-only conversation parsed from the session's JSONL transcript. Only
    /// populated for non-managed sessions (the terminal is the history otherwise).
    messages: Vec<crate::transcript::Message>,
    /// Scroll position of the transcript, so new messages auto-scroll to the newest.
    scroll: ScrollHandle,
    focus_handle: FocusHandle,
    /// The tab panel this monitor lives in (captured in [`Panel::on_added_to`]), so
    /// the tab bar's "×" can close this session — see [`super::tab_title`].
    tab_panel: Option<WeakEntity<TabPanel>>,
    /// Open right-click tab menu, rendered from the panel body (see [`super::TabMenuHost`]).
    tab_menu: Option<super::TabMenu>,
    /// A pending phase-advance the engine asked us to confirm because the session
    /// is **pinned** (`EngineEvent::PhaseAdvanceRequested`). `Some(to)` renders an
    /// "Advance to <to>?" banner; cleared once the phase moves.
    pending_advance: Option<Phase>,
    /// A held operator authorization (external-MCP tool / sensitive phase change)
    /// awaiting a verdict, shown as the bottom-right popup on this session's focus
    /// view. `None` when nothing is pending. See [`Self::authorize_popup`].
    pending_authorize: Option<PendingAuthorize>,
    /// Whether this is the frontmost center tab (set in `set_active`). Gates the
    /// status-bar context announces and keeps them fresh as the session updates.
    active: bool,
    /// Operator-set display metadata (custom name + color) for this session, loaded
    /// from the space's `.moonlight/session-meta.json` and refreshed on edit.
    meta: SessionMeta,
    /// In-progress rename: the inline input shown in place of the title. `None` = idle.
    rename_input: Option<Entity<InputState>>,
    /// Whether the color-swatch palette is expanded in the header.
    color_open: bool,
    /// Whether the condensed-header Phase selector dropdown is open.
    phase_open: bool,
    /// Whether the condensed-header Trust selector dropdown is open.
    trust_open: bool,
    /// Whether the info header is condensed: the whole facts card (phase stepper,
    /// trust selector, Path) plus the advance/resume affordances fold away, and Phase
    /// + Trust collapse into two compact dropdown buttons in the header top row, so the
    /// embedded CC terminal claims the reclaimed height. Operator-toggled from the
    /// header chevron. The terminal itself is always shown — condensing only trims chrome.
    header_collapsed: bool,
    /// Whether a `/compact` injection is currently armed on the embedded terminal
    /// (the auto-compact hysteresis state — see [`Self::check_auto_compact`]).
    compact_armed: bool,
    /// The space's auto-compact policy (`.moonlight/config.json`), loaded at build.
    compact_cfg: auto_compact::AutoCompactConfig,
    /// The most recent attention alert folded for this session, so the auto-resume
    /// trigger fires only on the *transition* into `Incomplete` (a stall while handed a
    /// turn), not on every repeated alert. See [`Self::maybe_auto_resume`].
    last_alert: Option<AttentionKind>,
    /// One-shot arm for the auto-resume nudge. Set **only** when this managed session is
    /// rebuilt as part of an **IDE-restart restore** under an auto-resume project (see
    /// [`Self::arm_restore_resume`]); cleared once the nudge fires. A live session that
    /// merely stalls is never armed, so auto-resume no longer fires on natural stalls —
    /// only on restart. See [`Self::maybe_auto_resume`].
    resume_armed: bool,
    /// Whether the persisted project trust tier has been applied to this session yet.
    /// Set the first time the engine row arrives (or it's confirmed there's nothing to
    /// apply), so the project's remembered trust is brought to a fresh/restored session
    /// exactly once, without fighting later operator overrides. See
    /// [`Self::maybe_apply_project_trust`].
    applied_project_trust: bool,
    /// Whether the *current* terminal's process-exit has already been auto-resumed, so
    /// the exit watcher acts once per exit (re-armed when a fresh terminal is live).
    exit_handled: bool,
    /// How many times this session has been auto-relaunched after its CC process exited,
    /// capped at [`MAX_EXIT_RESUMES`] so a CC that dies on launch can't loop forever.
    exit_resumes: u8,
    /// Holds the bus subscription alive for the panel's lifetime.
    _subscription: Option<Task<()>>,
    /// Holds the transcript-refresh poll alive (non-managed sessions only).
    _history: Option<Task<()>>,
    /// Holds the process-exit watcher alive (managed sessions only) — drives
    /// [`Self::check_terminal_exit`].
    _exit_watch: Option<Task<()>>,
}

impl SessionMonitor {
    /// Build a monitor for `id`, seeded with `initial` (the snapshot at open time)
    /// and kept live by folding `EngineEvent`s for this session off the bus.
    pub fn new(
        id: SessionId,
        initial: Option<Session>,
        rx: broadcast::Receiver<EngineEvent>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Observed/rehydrated sessions are external — `claude` by default (AGY sessions
        // aren't launched this way). A managed restore overrides via the record's agent.
        Self::build(id, initial, None, None, AgentKind::ClaudeCode, rx, cx)
    }

    /// Build a **new managed** session that embeds a live terminal running `command`
    /// (`claude --session-id <id>`) rooted at `root`. Always opens the terminal — a
    /// brand-new session can't be live anywhere else.
    pub fn new_managed(
        id: SessionId,
        initial: Option<Session>,
        root: PathBuf,
        command: String,
        agent: AgentKind,
        phase: Phase,
        rx: broadcast::Receiver<EngineEvent>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Seed the header/facts card immediately for a new session (the real record
        // arrives on discovery), in the phase CC was launched in.
        let initial = initial.or_else(|| Some(seed_session(&id, &root, phase)));
        // Chromeless, pinned to the session's repo and not following focus: this
        // terminal *is* the session's content region.
        let terminal =
            cx.new(|cx| TerminalPanel::new_running_in(root.clone(), &command, cx).embedded());
        Self::build(id, initial, Some(terminal), Some(root), agent, rx, cx)
    }

    /// Build a monitor for an **observed** session rooted at `root`. If the agent
    /// isn't actively working (any status but `Running`) it is taken over immediately
    /// (`claude --resume <id>`); while it *is* working it shows the read-only
    /// transcript plus a "Resume in terminal" action that unlocks once the agent
    /// stops (resuming a *running* session would put two clients on one transcript).
    pub fn new_observed(
        id: SessionId,
        initial: Option<Session>,
        root: PathBuf,
        rx: broadcast::Receiver<EngineEvent>,
        cx: &mut Context<Self>,
    ) -> Self {
        let auto = initial
            .as_ref()
            .map(|s| resumable(s.status))
            .unwrap_or(false);
        let terminal = auto.then(|| {
            let phase = initial
                .as_ref()
                .map(|s| s.phase)
                .unwrap_or_else(Phase::on_done);
            let mcp = crate::views::mcp_host::url_for_session(&id, cx);
            // Observed sessions are external Claude — no AGY conversation to resume.
            let command = attach_command(&id, AgentKind::ClaudeCode, phase, mcp.as_deref(), None);
            cx.new(|cx| TerminalPanel::new_running_in(root.clone(), &command, cx).embedded())
        });
        // Observed sessions are external `claude` — see [`new`].
        Self::build(
            id,
            initial,
            terminal,
            Some(root),
            AgentKind::ClaudeCode,
            rx,
            cx,
        )
    }

    fn build(
        id: SessionId,
        initial: Option<Session>,
        terminal: Option<Entity<TerminalPanel>>,
        resume_root: Option<PathBuf>,
        agent: AgentKind,
        rx: broadcast::Receiver<EngineEvent>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.spawn(async move |weak, cx| {
            let mut rx = rx;
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let applied = weak.update(cx, |this, cx| {
                            // Auto-resume watches alerts (which `apply_event` doesn't
                            // fold), so check it on every event before the row fold.
                            this.maybe_auto_resume(&event, cx);
                            if this.apply_event(&event) {
                                // First engine row is our cue to apply the project's
                                // remembered trust tier (once) to this session.
                                this.maybe_apply_project_trust(cx);
                                // Keep the status bar fresh while this session is the
                                // frontmost tab (status/phase/title can change live).
                                if this.active {
                                    this.announce_context(cx);
                                }
                                cx.notify();
                            }
                        });
                        if applied.is_err() {
                            break; // panel dropped
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // Observed (non-managed) sessions: refresh the read-only transcript from the
        // on-disk JSONL on a slow timer (off the engine path). Managed sessions show
        // their live terminal instead, so they skip this.
        let history = terminal.is_none().then(|| {
            let sid = id.as_str().to_string();
            cx.spawn(async move |weak, cx| loop {
                let sid = sid.clone();
                let msgs = cx
                    .background_executor()
                    .spawn(async move { crate::transcript::load_messages(&sid) })
                    .await;
                let keep = weak
                    .update(cx, |this, cx| {
                        if this.messages != msgs {
                            this.messages = msgs;
                            // Keep the newest message in view.
                            this.scroll.scroll_to_bottom();
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !keep {
                    break; // panel dropped
                }
                cx.background_executor()
                    .timer(Duration::from_millis(1500))
                    .await;
            })
        });

        // Managed sessions: watch for the CC process exiting (PTY closed) so a project
        // opted into auto-resume can relaunch it in place (`claude --resume` + a continue
        // prompt). Distinct from the stall nudge — an *exited* process is unambiguously a
        // session that stopped while working, so this fires for live sessions too (not
        // just restored ones). See [`Self::check_terminal_exit`].
        let exit_watch = terminal.is_some().then(|| {
            cx.spawn(async move |weak, cx| loop {
                cx.background_executor().timer(EXIT_POLL_INTERVAL).await;
                let keep = weak
                    .update(cx, |this, cx| this.check_terminal_exit(cx))
                    .is_ok();
                if !keep {
                    break; // panel dropped
                }
            })
        });

        // Register the embedded terminal so other panels (the Plan tab) can drive
        // this session's CC TUI — e.g. send Enter to accept a plan's continuation.
        if let Some(term) = &terminal {
            if let Some(io) = cx.try_global::<ShellDeps>().map(|d| d.session_io.clone()) {
                io.register(id.clone(), term.downgrade());
            }
        }

        // Auto-compact: watch the obs read-model (statusline-fed ctx usage) and arm
        // a `/compact` injection on the embedded terminal when the context window
        // crosses the configured threshold — it fires before the operator's next
        // message (see [`auto_compact`] for the policy, [`TerminalPanel::arm_injection`]
        // for the delivery). Managed sessions only: there's no PTY to drive otherwise.
        if terminal.is_some() {
            if let Some(obs) = cx.try_global::<ShellDeps>().map(|d| d.obs_store.clone()) {
                cx.observe(&obs, |this, obs, cx| this.check_auto_compact(&obs, cx))
                    .detach();
            }
        }

        // Resolve the space root (for `.moonlight/session-meta.json`) from the resume
        // root, else the session's attached path, and load any custom name/color.
        let root0 = resume_root.clone().or_else(|| {
            initial
                .as_ref()
                .and_then(|s| s.attached_path.as_deref())
                .map(crate::views::project_space::expand_home)
        });
        let meta = root0
            .as_ref()
            .map(|r| session_meta::get(r, &id))
            .unwrap_or_default();
        let compact_cfg = root0
            .as_deref()
            .map(auto_compact::load_config)
            .unwrap_or_default();

        // Recover an outstanding approval hold: this monitor may have been opened
        // *after* the one-shot `ApprovalRequested` (e.g. via the bell notification, or
        // by switching back to the session's space), which the live subscription above
        // could not have seen. The retained hold rebuilds the popup so the operator can
        // still act. See [`crate::views::approvals`].
        let pending_authorize = cx
            .try_global::<ShellDeps>()
            .and_then(|d| d.approvals.get(&id))
            .map(|hold| match hold.tool {
                Some(tool) => PendingAuthorize::McpTool(tool),
                None => PendingAuthorize::Action(hold.what),
            });

        Self {
            id,
            agent,
            session: initial,
            terminal,
            resume_root,
            messages: Vec::new(),
            scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            tab_panel: None,
            tab_menu: None,
            pending_advance: None,
            pending_authorize,
            active: false,
            meta,
            rename_input: None,
            color_open: false,
            phase_open: false,
            trust_open: false,
            // Default to a collapsed header so the session view opens compact;
            // the operator expands it via the ▾/▴ collapse button when needed.
            header_collapsed: true,
            compact_armed: false,
            compact_cfg,
            last_alert: None,
            resume_armed: false,
            applied_project_trust: false,
            exit_handled: false,
            exit_resumes: 0,
            _subscription: Some(subscription),
            _history: history,
            _exit_watch: exit_watch,
        }
    }

    /// The persisted Antigravity `conversationId` for this session, read **fresh** from
    /// the managed store each call (so a conversation correlated after launch is picked
    /// up on the next relaunch). `None` for Claude (its managed id *is* its conversation)
    /// and for an AGY session whose conversation hasn't been discovered yet. Feeds
    /// [`attach_command`]'s AGY resume target.
    fn agy_conversation(&self, cx: &Context<Self>) -> Option<String> {
        if self.agent != AgentKind::Antigravity {
            return None;
        }
        cx.try_global::<ShellDeps>()?
            .store
            .managed(&self.id)
            .ok()
            .flatten()?
            .conversation_id
    }

    /// Take over an observed session in a terminal (`claude --resume <id>`), if it
    /// is currently idle/done and has a known repo. No-op otherwise.
    fn try_resume(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminal.is_some() || !self.is_resumable() {
            return;
        }
        let Some(root) = self.resume_root.clone() else {
            return;
        };
        let phase = self
            .session
            .as_ref()
            .map(|s| s.phase)
            .unwrap_or_else(Phase::on_done);
        let mcp = crate::views::mcp_host::url_for_session(&self.id, cx);
        let agy_cid = self.agy_conversation(cx);
        let command = attach_command(
            &self.id,
            self.agent,
            phase,
            mcp.as_deref(),
            agy_cid.as_deref(),
        );
        let terminal = cx.new(|cx| TerminalPanel::new_running_in(root, &command, cx).embedded());
        if let Some(io) = cx.try_global::<ShellDeps>().map(|d| d.session_io.clone()) {
            io.register(self.id.clone(), terminal.downgrade());
        }
        self.terminal = Some(terminal);
        // The fresh terminal carries no armed injection — re-sync the policy state
        // (the obs observer re-arms on its next change if usage is still high).
        self.compact_armed = false;
        // Takeover happens on an already-active tab, so no tab activation fires to
        // run the dock's `focus_active_panel` — focus the new terminal by hand.
        self.focus_terminal(window, cx);
        cx.notify();
    }

    /// Whether this session may be resumed right now (idle/done — not actively live).
    fn is_resumable(&self) -> bool {
        self.session
            .as_ref()
            .map(|s| resumable(s.status))
            .unwrap_or(false)
    }

    /// Fold an event into this session's read-model. Returns `true` if it changed
    /// something for *this* session (so the caller only re-renders when relevant).
    fn apply_event(&mut self, event: &EngineEvent) -> bool {
        match event {
            EngineEvent::SessionUpserted { session } if session.id == self.id => {
                let waiting = matches!(session.status, SessionStatus::WaitingInput);
                self.session = Some(session.clone());
                // A full-row update means the phase (and pin) are current — any
                // pending advance request has been resolved.
                self.pending_advance = None;
                // Unless the row still shows the session blocked on us, any held
                // authorization has been resolved elsewhere — drop the popup.
                if !waiting {
                    self.pending_authorize = None;
                }
                true
            }
            EngineEvent::SessionStateChanged { session, status } if *session == self.id => {
                if let Some(s) = self.session.as_mut() {
                    s.status = *status;
                }
                // A resume (any status but WaitingInput) means an outstanding
                // authorization hold was resolved — dismiss the popup.
                if !matches!(status, SessionStatus::WaitingInput) {
                    self.pending_authorize = None;
                }
                true
            }
            // A held operator authorization (external-MCP tool, or a sensitive phase
            // change / danger-zone command) — surface it in the bottom-right popup.
            EngineEvent::ApprovalRequested {
                session,
                what,
                authorize_tool,
            } if *session == self.id => {
                self.pending_authorize = Some(match authorize_tool {
                    Some(tool) => PendingAuthorize::McpTool(tool.clone()),
                    None => PendingAuthorize::Action(what.clone()),
                });
                true
            }
            EngineEvent::PhaseTransitioned { session, phase } if *session == self.id => {
                if let Some(s) = self.session.as_mut() {
                    s.phase = *phase;
                }
                self.pending_advance = None;
                // A phase move resolves a pending phase-change approval.
                self.pending_authorize = None;
                true
            }
            // A pinned session reached a checkpoint and the workflow wants to move;
            // remember it so the card can offer the operator a one-click advance.
            EngineEvent::PhaseAdvanceRequested { session, to } if *session == self.id => {
                self.pending_advance = Some(*to);
                true
            }
            _ => false,
        }
    }

    /// Bring this session to the project's persisted trust tier, once, when the engine
    /// row first arrives. Honors the operator's remembered "trust this project" decision
    /// for both freshly-launched and restored sessions (the engine seeds every session
    /// at the default-deny `Observed`). Runs at most once — later operator overrides via
    /// the Trust selector are not undone.
    fn maybe_apply_project_trust(&mut self, cx: &mut Context<Self>) {
        if self.applied_project_trust {
            return;
        }
        let Some(current) = self.session.as_ref().map(|s| s.trust_tier) else {
            return; // no row yet — try again on the next upsert
        };
        let Some(deps) = cx.try_global::<ShellDeps>().cloned() else {
            return;
        };
        // A managed session carries its own persisted trust — seeded at launch from the
        // project default, then possibly overridden by the operator's Trust selector — and
        // the engine restores it on discovery/hydrate. That per-session value is
        // authoritative: never re-impose the project default on it, or a session-scoped
        // override would be clobbered every time its monitor reopens. Only discovered /
        // observed sessions (no managed record) inherit the project default live here.
        if deps.store.managed(&self.id).ok().flatten().is_some() {
            self.applied_project_trust = true;
            return;
        }
        let Some(root) = self.space_root() else {
            return;
        };
        let Some(tier) = deps.focus.read(cx).project_trust(&root) else {
            // Project trust isn't decided yet — do NOT mark applied, so a session opened
            // *before* the operator entrusts the project still picks the decision up on a
            // later event (rather than being stuck at `Observed` for its whole lifetime).
            // The launch prompt also `SetTrust`s the launching session directly.
            return;
        };
        // We have a decision: align this session to it, exactly once. Marking applied
        // only now means a manual Trust-selector change (which sets this flag itself) and
        // a restored per-session tier are never clobbered by a later project default.
        self.applied_project_trust = true;
        if current != tier {
            let _ = deps.commands.send(Command::SetTrust {
                session: self.id.clone(),
                tier,
            });
        }
    }

    /// Arm the **restart** auto-resume nudge on this session. Called once, from the
    /// restore path ([`build_center_panel`](crate::views::workspace)), when a managed
    /// session is rebuilt as the IDE comes back up under an auto-resume project. The
    /// nudge itself fires from [`maybe_auto_resume`](Self::maybe_auto_resume) the next
    /// time detection flags the restored session as stalled mid-turn (`Incomplete`).
    /// No terminal ⇒ nothing to nudge, so leave it disarmed.
    pub fn arm_restore_resume(&mut self) {
        if self.terminal.is_some() {
            self.resume_armed = true;
        }
    }

    /// Auto-resume on **IDE restart**: when a managed session was restored at startup
    /// (and so [armed](Self::arm_restore_resume)) and detection then flags it as stalled
    /// mid-turn (`Incomplete` — handed a turn, then silent past the working window), nudge
    /// it to continue by injecting a resume prompt into its embedded terminal. One-shot:
    /// disarmed once fired. Edge-triggered on the transition *into* `Incomplete` (tracked
    /// via [`last_alert`](Self::last_alert)).
    ///
    /// This deliberately does **not** fire for a live session that merely goes quiet —
    /// only restored sessions are ever armed, so a natural mid-session stall is left for
    /// the operator. (A session whose terminal is gone is never armed either; process-level
    /// resume stays the operator's explicit `↻`/resume affordance.)
    fn maybe_auto_resume(&mut self, event: &EngineEvent, cx: &mut Context<Self>) {
        let EngineEvent::SessionAlert { session, alert } = event else {
            return;
        };
        if *session != self.id {
            return;
        }
        let became_incomplete = *alert == Some(AttentionKind::Incomplete)
            && self.last_alert != Some(AttentionKind::Incomplete);
        self.last_alert = *alert;
        // Only a session armed by the restart restore is eligible (gates out live stalls).
        if !became_incomplete || !self.resume_armed {
            return;
        }
        let Some(deps) = cx.try_global::<ShellDeps>().cloned() else {
            return;
        };
        let Some(root) = self.space_root() else {
            return;
        };
        if !deps.focus.read(cx).project_auto_resume(&root) {
            return;
        }
        // The CC process drives its own embedded terminal; nudge it there. No terminal
        // ⇒ nothing to resume in place (leave it to the operator's explicit resume).
        let Some(term) = deps.session_io.terminal(&self.id) else {
            return;
        };
        // One-shot: a restored session is nudged at most once, even if it stalls again.
        self.resume_armed = false;
        tracing::info!(
            session = %self.id.as_str(),
            "auto-resume: restored session stalled mid-turn — injecting continue prompt"
        );
        cx.spawn(async move |_, cx| {
            let _ = term.update(cx, |t, _| t.send_text(&format!("{AUTO_RESUME_PROMPT}\r")));
        })
        .detach();
    }

    /// Poll-driven auto-resume on **process exit**: when a managed session's CC process
    /// ends (its PTY closes), a project opted into auto-resume relaunches the same
    /// conversation in place (`claude --resume`) and nudges it to continue. Unlike the
    /// stall nudge, an exited process is unambiguously a session that stopped while
    /// working, so this fires for live sessions too — capped at [`MAX_EXIT_RESUMES`] so a
    /// CC that dies on launch can't loop. Acts once per exit (re-armed when alive again).
    fn check_terminal_exit(&mut self, cx: &mut Context<Self>) {
        let Some(term) = self.terminal.as_ref() else {
            return;
        };
        if !term.read(cx).has_exited() {
            self.exit_handled = false; // a live (freshly relaunched) terminal → re-arm
            return;
        }
        if self.exit_handled {
            return; // already acted on this exit
        }
        self.exit_handled = true;
        let Some(deps) = cx.try_global::<ShellDeps>().cloned() else {
            return;
        };
        let Some(root) = self.space_root() else {
            return;
        };
        if !deps.focus.read(cx).project_auto_resume(&root) {
            return;
        }
        if self.exit_resumes >= MAX_EXIT_RESUMES {
            tracing::warn!(
                session = %self.id.as_str(),
                "auto-resume: CC exited but the relaunch cap was reached — leaving it to the operator"
            );
            return;
        }
        self.exit_resumes += 1;
        self.relaunch_after_exit(root, &deps, cx);
    }

    /// Relaunch a managed session whose CC process exited, resuming the same conversation
    /// and injecting the continue prompt once the TUI is back. Mirrors [`Self::try_resume`]
    /// but is window-free (poll-driven, no focus steal). Polls the screen so the nudge
    /// isn't fired into a not-yet-ready CC (a lost keystroke).
    fn relaunch_after_exit(&mut self, root: PathBuf, deps: &ShellDeps, cx: &mut Context<Self>) {
        let phase = self
            .session
            .as_ref()
            .map(|s| s.phase)
            .unwrap_or_else(Phase::on_done);
        let mcp = crate::views::mcp_host::url_for_session(&self.id, cx);
        let agy_cid = self.agy_conversation(cx);
        let command = attach_command(
            &self.id,
            self.agent,
            phase,
            mcp.as_deref(),
            agy_cid.as_deref(),
        );
        let terminal = cx.new(|cx| TerminalPanel::new_running_in(root, &command, cx).embedded());
        deps.session_io
            .register(self.id.clone(), terminal.downgrade());
        let weak_term = terminal.downgrade();
        self.terminal = Some(terminal);
        // The fresh terminal carries no armed injection.
        self.compact_armed = false;
        tracing::info!(
            session = %self.id.as_str(),
            "auto-resume: CC process exited — relaunched with --resume and nudging to continue"
        );
        cx.spawn(async move |_, cx| {
            let mut waited = 0u64;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(300))
                    .await;
                waited += 300;
                let Ok(ready) = weak_term.update(cx, |t, _| !t.visible_text().trim().is_empty())
                else {
                    return; // terminal gone
                };
                if ready || waited >= 8000 {
                    break;
                }
            }
            let _ = weak_term.update(cx, |t, _| t.send_text(&format!("{AUTO_RESUME_PROMPT}\r")));
        })
        .detach();
        cx.notify();
    }

    /// Push this session's name / status / phase to the shared
    /// [`ActiveContext`](crate::views::active_context::ActiveContext) so the bottom
    /// status bar reflects it. Falls back to the short id + neutral status/phase
    /// before the engine record arrives.
    fn announce_context(&self, cx: &mut Context<Self>) {
        let Some(ac) = cx
            .try_global::<ShellDeps>()
            .map(|d| d.active_context.clone())
        else {
            return;
        };
        let ctx = match &self.session {
            Some(s) => ActiveContext::Session {
                id: self.id.clone(),
                label: s.label().to_string(),
                status: s.status,
                phase: s.phase,
            },
            None => ActiveContext::Session {
                id: self.id.clone(),
                label: self.title_text(),
                status: SessionStatus::Idle,
                phase: Phase::Plan,
            },
        };
        ac.update(cx, |a, cx| {
            *a = ctx;
            cx.notify();
        });
    }

    fn title_text(&self) -> String {
        if let Some(name) = self.meta.display_name() {
            return name.to_string();
        }
        match &self.session {
            Some(s) => s.label().to_string(),
            None => self.id.as_str().to_string(),
        }
    }

    /// The space root backing this session's `.moonlight/session-meta.json`, if known
    /// (resume root, else the session's attached path). Rename/color persist here.
    fn space_root(&self) -> Option<PathBuf> {
        self.resume_root.clone().or_else(|| {
            self.session
                .as_ref()
                .and_then(|s| s.attached_path.as_deref())
                .map(crate::views::project_space::expand_home)
        })
    }

    /// The shared metadata cache (so a write notifies the fleet grid), if the shell
    /// global is installed (absent in tests / static views).
    fn meta_cache(&self, cx: &App) -> Option<Entity<SessionMetaCache>> {
        cx.try_global::<ShellDeps>().map(|d| d.session_meta.clone())
    }

    /// Reload cached metadata after an edit so the header/tab reflect it immediately.
    fn reload_meta(&mut self, cx: &mut Context<Self>) {
        if let Some(root) = self.space_root() {
            self.meta = session_meta::get(&root, &self.id);
        }
        if self.active {
            self.announce_context(cx);
        }
        cx.notify();
    }

    /// Open the inline rename field, seeded with the current display name.
    fn begin_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.space_root().is_none() {
            return; // nowhere to persist
        }
        let seed = self.title_text();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("Session name");
            state.set_value(seed, window, cx);
            state
        });
        input.focus_handle(cx).focus(window, cx);
        self.rename_input = Some(input);
        self.color_open = false;
        cx.notify();
    }

    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.rename_input = None;
        cx.notify();
    }

    /// Commit the rename: persist the name to the space's metadata, and for a managed
    /// session (one we own a live PTY for) write through to CC's native `/rename` so
    /// its own resume picker matches. An empty value clears the custom name.
    fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.space_root() else {
            self.rename_input = None;
            return;
        };
        let value = self
            .rename_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_string())
            .unwrap_or_default();
        let name = (!value.is_empty()).then(|| value.clone());

        // Push the rename into CC natively so its own `/resume` picker shows it:
        // a live terminal gets the real `/rename`; an idle session gets the same
        // `custom-title` record appended to its transcript (what `/rename` itself
        // persists). Only a ghost (no transcript) stays app-local.
        let source = match (&name, &self.terminal) {
            (Some(_), Some(term)) => {
                term.update(cx, |t, _| t.send_text(&format!("/rename {value}\r")));
                NameSource::Cc
            }
            (Some(_), None) if crate::transcript::append_custom_title(self.id.as_str(), &value) => {
                NameSource::Cc
            }
            _ => NameSource::Local,
        };
        // Write through the shared cache so the fleet grid re-renders; fall back to a
        // direct disk write when no shell global is installed (static/test views).
        match self.meta_cache(cx) {
            Some(cache) => cache.update(cx, |c, cx| {
                c.set_name(&root, &self.id, name, source);
                cx.notify();
            }),
            None => session_meta::set_name(&root, &self.id, name, source),
        }
        self.rename_input = None;
        self.reload_meta(cx);
    }

    fn toggle_color_palette(&mut self, cx: &mut Context<Self>) {
        if self.space_root().is_none() {
            return;
        }
        self.color_open = !self.color_open;
        cx.notify();
    }

    /// Pick (or clear, when `None`) the session's color, persist, and close the palette.
    /// For a **managed** session (we own the PTY) the choice is **written through to
    /// CC's native `/color`** — the palette mirrors CC's accepted tokens, and clearing
    /// maps to `/color default` — so the session's color matches in both surfaces.
    fn pick_color(&mut self, color: Option<SessionColor>, cx: &mut Context<Self>) {
        if let Some(root) = self.space_root() {
            match self.meta_cache(cx) {
                Some(cache) => cache.update(cx, |c, cx| {
                    c.set_color(&root, &self.id, color);
                    cx.notify();
                }),
                None => session_meta::set_color(&root, &self.id, color),
            }
        }
        if let Some(term) = &self.terminal {
            let token = color.map(|c| c.token()).unwrap_or("default");
            term.update(cx, |t, _| t.send_text(&format!("/color {token}\r")));
        }
        self.color_open = false;
        self.reload_meta(cx);
    }
}

/// A session may be taken over (resumed) when it is **not actively working** —
/// i.e. anything except [`SessionStatus::Running`] (which is mid-tool-loop /
/// streaming, where a second client would clash). `WaitingInput` ("your turn") is
/// the common resumable state; `Idle`/`Done` are the rare quiet/stale ones.
fn resumable(status: SessionStatus) -> bool {
    !matches!(status, SessionStatus::Running)
}

/// Build the command to bring session `id` up in an embedded terminal.
///
/// Resumes the existing Claude Code conversation (`claude --resume <id>`) **when a
/// transcript for `id` exists on disk**. If none does — a "ghost" record whose CC
/// session was never persisted (a managed session launched but closed before any
/// interaction, or a launch that failed) — `--resume` aborts with "No conversation
/// found with session ID", so we instead start a fresh session pinned to the *same*
/// id (`claude --session-id <id> --permission-mode <mode>`). That keeps the managed
/// record and its tab id stable instead of dead-ending on resume.
///
/// `mcp_url` (the session's embedded MCP endpoint, see [`crate::views::mcp_host`])
/// appends the `--mcp-config` flag so the agent gets the `moonlight` actor verbs;
/// `None` (host down / static views) launches without MCP.
pub fn attach_command(
    id: &SessionId,
    agent: AgentKind,
    phase: Phase,
    mcp_url: Option<&str>,
    agy_conversation: Option<&str>,
) -> String {
    use crate::agent_backend::{backend_for, LaunchSpec, SessionSelector};
    // Resume the existing conversation when one is known; otherwise start fresh
    // (see the ghost-session note above). Backend-agnostic: the command shape is the
    // backend's job (ClaudeCode is byte-identical to before).
    //
    // The resume *target* differs by backend. Claude pins our managed id, so its
    // conversation IS `id` — resume when a transcript for it exists. Antigravity has no
    // `--session-id`; it mints its own `conversationId` (discovered + persisted post-
    // launch, passed in as `agy_conversation`) — resume *that*, and only when it's known;
    // otherwise launch fresh and let discovery correlate the new conversation.
    let agy_cid = agy_conversation.map(SessionId::new);
    let (selector, permission_mode) = match agent {
        AgentKind::Antigravity => match &agy_cid {
            Some(cid) => (SessionSelector::Resume(cid), None),
            None => (SessionSelector::Fresh(id), Some(phase.cc_permission_mode())),
        },
        AgentKind::ClaudeCode => {
            if crate::transcript::transcript_path(id.as_str()).is_some() {
                (SessionSelector::Resume(id), None)
            } else {
                (SessionSelector::Fresh(id), Some(phase.cc_permission_mode()))
            }
        }
    };
    let backend = backend_for(agent);
    // Backend-specific pre-launch side effects (AGY writes its mcp_config; Claude no-op).
    backend.prepare_launch(mcp_url);
    let command = backend.launch_command(&LaunchSpec {
        selector,
        permission_mode,
        mcp_url,
    });
    // Statusline `--settings` is Claude-only; AGY would reject it (see wrap_statusline).
    crate::agent_backend::wrap_statusline(agent, command)
}

/// A placeholder session record so the header/facts render before discovery, in
/// the phase CC was launched in (so the card matches the `--permission-mode` flag).
fn seed_session(id: &SessionId, root: &std::path::Path, phase: Phase) -> Session {
    let mode = phase.operator_mode();
    Session {
        id: id.clone(),
        title: None,
        status: SessionStatus::Running,
        phase,
        mode,
        trust_tier: TrustTier::Observed,
        attached_path: Some(root.to_string_lossy().into_owned()),
        pinned: false,
        adopted: false,
        paused: false,
        phase_pinned: false,
        hidden: false,
        last_activity: Timestamp::from_millis(0),
    }
}

impl Focusable for SessionMonitor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        // Delegate to the embedded terminal when present: the dock's
        // `focus_active_panel` focuses whatever we return here on tab activation, so
        // entering a managed session's tab lands the caret straight in the live
        // Claude Code TUI — no extra click (mirrors reopening a file restoring its
        // caret). Observed sessions with no terminal keep the panel's own handle.
        match &self.terminal {
            Some(term) => term.read(cx).focus_handle(cx),
            None => self.focus_handle.clone(),
        }
    }
}

impl super::TabMenuHost for SessionMonitor {
    fn tab_menu_slot(&mut self) -> &mut Option<super::TabMenu> {
        &mut self.tab_menu
    }
}

impl EventEmitter<PanelEvent> for SessionMonitor {}

impl Panel for SessionMonitor {
    fn panel_name(&self) -> &'static str {
        "SessionMonitor"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A leading blinking amber caret when this session is awaiting the operator, so a
        // *background* session needing input is spottable across the dock tab bar. Uses
        // the per-tab `title` (rendered for every tab) — not `title_suffix`, which the
        // library renders only for the active tab, off at the far right of the bar.
        let needs_input = self
            .session
            .as_ref()
            .is_some_and(|s| s.status == SessionStatus::WaitingInput);
        let seed = cx.entity_id().as_u64();
        let title = super::tab_title(
            self.title_text(),
            self.meta.palette_color().map(|c| c.hsla()),
            seed,
            self.tab_panel.clone(),
            Arc::new(cx.entity()),
            self.focus_handle.clone(),
            cx,
            |menu| {
                menu.separator()
                    .menu("Split Right", Box::new(SplitRight))
                    .menu("Split Down", Box::new(SplitDown))
            },
        );
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .when(needs_input, |d| {
                d.child(super::needs_input_caret(("dock-tab-caret", seed)))
            })
            // Backend badge on the tab — only non-Claude (Claude is the implicit default),
            // so the operator can tell which runner a session tab uses at a glance.
            .when(self.agent == AgentKind::Antigravity, |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .px(px(4.))
                        .rounded(theme::radius_sm())
                        .bg(theme::tint(theme::accent(), 0.18))
                        .text_color(theme::accent())
                        .text_size(theme::text_xs())
                        .child("AGY"),
                )
            })
            .child(title)
    }

    /// Capture the tab panel so the tab bar's "×" can close this session.
    fn on_added_to(
        &mut self,
        tab_panel: WeakEntity<TabPanel>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.tab_panel = Some(tab_panel);
    }

    /// Add "Split …" entries to the tab's "…" secondary menu. The actions are routed
    /// to *this* panel via `action_context` (the panel's focus handle, which the
    /// render root tracks) so the `on_action` handlers fire; the dock's own Zoom/Close
    /// items still bubble up to the parent `TabPanel`. A session is *moved* into the
    /// split (not duplicated) — see [`Self::split`].
    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> PopupMenu {
        menu.action_context(self.focus_handle.clone())
            .menu("Split Right", Box::new(SplitRight))
            .menu("Split Left", Box::new(SplitLeft))
            .menu("Split Down", Box::new(SplitDown))
            .menu("Split Up", Box::new(SplitUp))
    }

    /// When this session tab becomes frontmost, announce it to the bottom status bar.
    fn set_active(&mut self, active: bool, _window: &mut Window, cx: &mut Context<Self>) {
        self.active = active;
        if active {
            self.announce_context(cx);
        }
    }

    /// Persist which session this tab monitors (+ its repo root, so a rehydrated tab
    /// can still offer resume), so a saved layout can reopen it.
    fn dump(&self, _cx: &App) -> PanelState {
        let mut state = PanelState::new(self);
        state.info = PanelInfo::panel(serde_json::json!({
            "session": self.id.as_str(),
            "root": self.resume_root.as_ref().map(|p| p.to_string_lossy().into_owned()),
        }));
        state
    }
}

impl Render for SessionMonitor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The session's operator-chosen color washes the panel + header (and tints the
        // tab); `None` keeps the neutral surfaces.
        let accent = self.meta.palette_color().map(|c| c.hsla());
        let dismiss = cx.listener(|this, _: &MouseDownEvent, _w, cx| {
            this.tab_menu = None;
            cx.notify();
        });
        let header = match &self.session {
            Some(s) => {
                let color = theme::status_color(s.status);
                // Only a managed session (one we own a live PTY for) can be
                // compacted/reset — both drive CC in its embedded terminal.
                let compact = self.terminal.is_some().then(|| self.compact_button(cx));
                let reset = self.terminal.is_some().then(|| self.reset_button(cx));
                // Per-project auto-resume opt-in toggle — only meaningful for a managed
                // session whose project we can remember the flag against.
                let auto_resume = (self.terminal.is_some() && self.space_root().is_some())
                    .then(|| self.auto_resume_button(cx));
                // Rename/color persist into the space's `.moonlight/`; only offered
                // when we can resolve that root.
                let can_edit = self.space_root().is_some();

                // The name region: the inline rename field, or the name + edit pencil.
                let name_row = match &self.rename_input {
                    Some(input) => div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.))
                        .flex_1()
                        .child(div().flex_1().child(Input::new(input)))
                        .child(self.name_pill(
                            "session-rename-save",
                            "Save",
                            theme::accent(),
                            |this, _w, cx| this.commit_rename(cx),
                            cx,
                        ))
                        .child(self.name_pill(
                            "session-rename-cancel",
                            "Cancel",
                            theme::text_muted(),
                            |this, _w, cx| this.cancel_rename(cx),
                            cx,
                        ))
                        .into_any_element(),
                    None => {
                        let mut row = div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.))
                            .flex_1()
                            .overflow_hidden()
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_size(theme::text_xl())
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text_primary())
                                    .child(self.title_text()),
                            );
                        if can_edit {
                            row = row.child(self.icon_button(
                                "session-rename",
                                "✎",
                                |this, w, cx| this.begin_rename(w, cx),
                                cx,
                            ));
                        }
                        row.into_any_element()
                    }
                };

                // Condensed: Phase + Trust fold into compact dropdown buttons in the
                // header (before the action buttons), since their full rows are hidden.
                let condensed = self.header_collapsed;
                let phase_pill = condensed.then(|| self.phase_pill(s.phase, cx));
                let trust_pill = condensed.then(|| self.trust_pill(s.trust_tier, cx));

                let top = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(7.))
                    .child(
                        div()
                            .text_color(color)
                            .text_size(theme::text_base())
                            .child(s.status.badge().to_string()),
                    )
                    .children(can_edit.then(|| self.color_dot(cx)))
                    .child(name_row)
                    .children(phase_pill)
                    .children(trust_pill)
                    .children(auto_resume)
                    .children(compact)
                    .children(reset)
                    // Condense the header to hand more height to the CC terminal.
                    .children(self.terminal.is_some().then(|| self.collapse_button(cx)));

                let mut head = div().flex().flex_col().gap(px(8.));
                if let Some(a) = accent {
                    head = head
                        .bg(theme::blend(theme::surface_base(), a, 0.22))
                        .rounded(theme::radius_md())
                        .border_1()
                        .border_color(theme::tint(a, 0.5))
                        .px_3()
                        .py(px(8.));
                }
                head.child(top)
                    .children(self.color_open.then(|| self.color_palette(cx)))
                    .children((condensed && self.phase_open).then(|| self.phase_menu(s.phase, cx)))
                    .children(
                        (condensed && self.trust_open).then(|| self.trust_menu(s.trust_tier, cx)),
                    )
            }
            None => div()
                .text_color(theme::text_muted())
                .text_size(theme::text_base())
                .child(format!("waiting for session {}…", self.id.as_str())),
        };

        // The session's facts as a quiet, labelled card (phase stepper, trust
        // selector, Path). The whole card folds away when the header is condensed —
        // Phase + Trust then live as compact dropdowns in the header top row — so the
        // terminal gets that height back.
        let condensed = self.header_collapsed;
        let fields = (!condensed)
            .then(|| self.session.as_ref())
            .flatten()
            .map(|s| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(7.))
                    .rounded(theme::radius_md())
                    .border_1()
                    .border_color(theme::border_subtle())
                    .bg(theme::surface_raised())
                    .px_3()
                    .py(px(11.))
                    .child(self.phase_stepper(s.phase, cx))
                    .child(self.trust_selector(s.trust_tier, cx))
                    .child(field(
                        "Path",
                        s.attached_path.as_deref().unwrap_or("—"),
                        theme::text_secondary(),
                        true,
                    ))
            });

        div()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::split_right))
            .on_action(cx.listener(Self::split_left))
            .on_action(cx.listener(Self::split_up))
            .on_action(cx.listener(Self::split_down))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                super::close_this_tab(&this.tab_panel, cx.entity(), window, cx)
            }))
            .relative()
            .flex()
            .flex_col()
            .gap(px(14.))
            .size_full()
            .bg(accent
                .map(|a| theme::blend(theme::surface_base(), a, 0.10))
                .unwrap_or_else(theme::surface_base))
            .text_color(theme::text_primary())
            .p_4()
            .child(header)
            .children(fields)
            // The advance/resume chrome folds away with the condensed header.
            .children((!condensed).then_some(()).and_then(|()| {
                self.session
                    .as_ref()
                    .map(|s| (s.phase, s.status))
                    .and_then(|(phase, status)| self.advance_affordance(phase, status, cx))
            }))
            .children((!condensed).then(|| self.resume_bar(cx)).flatten())
            // The CC terminal is always shown — condensing only trims header chrome,
            // handing the reclaimed height to the terminal.
            .child(self.transcript())
            // The bottom-right authorization popup floats over the focus view (added
            // last so it paints above the terminal/transcript).
            .children(self.authorize_popup(cx))
            .children(super::tab_menu_overlay(
                self.tab_menu.as_ref(),
                dismiss,
                window,
            ))
    }
}

impl SessionMonitor {
    /// Send an authorization verdict to the engine and dismiss the popup. The optimistic
    /// clear keeps the overlay from lingering while the resumed status makes its bus
    /// round-trip (the fold in [`Self::apply_event`] would clear it too, as a backstop).
    fn resolve_authorize(&mut self, command: Command, cx: &mut Context<Self>) {
        if let Some(deps) = cx.try_global::<ShellDeps>() {
            let _ = deps.commands.send(command);
            // Drop the retained hold now, not just on the resolving event, so a monitor
            // reopened in the gap between this click and the engine's ack doesn't rebuild
            // the popup (and let the operator double-decide). See [`crate::views::approvals`].
            deps.approvals.clear(&self.id);
        }
        self.pending_authorize = None;
        cx.notify();
    }

    /// A compact pill button for the authorization popup.
    fn authorize_button(
        id: &'static str,
        label: String,
        color: Hsla,
        tint: f32,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .id(id)
            .cursor_pointer()
            .px(px(9.))
            .py(px(4.))
            .rounded(theme::radius_sm())
            .bg(theme::tint(color, tint))
            .text_color(color)
            .text_size(px(11.))
            .child(label)
            .on_click(cx.listener(move |this, _ev, _w, cx| on_click(this, cx)))
            .into_any_element()
    }

    /// The bottom-right authorization popup: a small overlay on the focus view that
    /// surfaces a held operator decision (external-MCP tool / sensitive phase change /
    /// danger-zone command) without stealing the whole center. `None` when nothing is
    /// pending. Anchored by the render root's `.relative()`.
    fn authorize_popup(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let pending = self.pending_authorize.as_ref()?;

        let (title, body, buttons): (&str, String, Vec<gpui::AnyElement>) = match pending {
            PendingAuthorize::McpTool(tool) => {
                let tool_pat = tool.clone();
                let glob = server_glob(tool);
                let glob_label = glob.clone();
                let buttons = vec![
                    Self::authorize_button(
                        "auth-allow-once",
                        "✓ Allow once".to_string(),
                        theme::accent(),
                        0.18,
                        |this, cx| {
                            this.resolve_authorize(
                                Command::ApproveAction {
                                    session: this.id.clone(),
                                },
                                cx,
                            )
                        },
                        cx,
                    ),
                    Self::authorize_button(
                        "auth-always-tool",
                        "✓ Always: this tool".to_string(),
                        theme::accent(),
                        0.12,
                        move |this, cx| {
                            this.resolve_authorize(
                                Command::AuthorizeAlwaysTool {
                                    session: this.id.clone(),
                                    pattern: tool_pat.clone(),
                                },
                                cx,
                            )
                        },
                        cx,
                    ),
                    Self::authorize_button(
                        "auth-always-server",
                        format!("✓ Always: {glob_label}"),
                        theme::accent(),
                        0.12,
                        move |this, cx| {
                            this.resolve_authorize(
                                Command::AuthorizeAlwaysTool {
                                    session: this.id.clone(),
                                    pattern: glob.clone(),
                                },
                                cx,
                            )
                        },
                        cx,
                    ),
                    Self::authorize_button(
                        "auth-refuse",
                        "✕ Refuse".to_string(),
                        theme::status_color(SessionStatus::Errored),
                        0.16,
                        |this, cx| {
                            this.resolve_authorize(
                                Command::DenyAction {
                                    session: this.id.clone(),
                                    reason: MCP_REFUSE_REASON.to_string(),
                                },
                                cx,
                            )
                        },
                        cx,
                    ),
                ];
                ("Authorize external tool", tool.clone(), buttons)
            }
            PendingAuthorize::Action(what) => {
                let buttons = vec![
                    Self::authorize_button(
                        "auth-approve",
                        "✓ Approve".to_string(),
                        theme::accent(),
                        0.18,
                        |this, cx| {
                            this.resolve_authorize(
                                Command::ApproveAction {
                                    session: this.id.clone(),
                                },
                                cx,
                            )
                        },
                        cx,
                    ),
                    Self::authorize_button(
                        "auth-action-refuse",
                        "✕ Refuse".to_string(),
                        theme::status_color(SessionStatus::Errored),
                        0.16,
                        |this, cx| {
                            this.resolve_authorize(
                                Command::DenyAction {
                                    session: this.id.clone(),
                                    reason: ACTION_REFUSE_REASON.to_string(),
                                },
                                cx,
                            )
                        },
                        cx,
                    ),
                ];
                let (title, body) = action_prompt(what);
                (title, body, buttons)
            }
        };

        Some(
            div()
                .absolute()
                .bottom(px(16.))
                .right(px(16.))
                .w(px(320.))
                .flex()
                .flex_col()
                .gap_2()
                .rounded(theme::radius_md())
                .border_1()
                .border_color(theme::accent())
                .bg(theme::surface_raised())
                .p_3()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.))
                        .child(div().text_color(theme::accent()).child("◈"))
                        .child(
                            div()
                                .text_size(theme::text_sm())
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title.to_string()),
                        ),
                )
                .child(
                    div()
                        .font_family(theme::mono_font())
                        .text_size(px(11.))
                        .text_color(theme::text_secondary())
                        .child(body),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .items_center()
                        .gap(px(6.))
                        .children(buttons),
                )
                .into_any_element(),
        )
    }

    /// A subtle square glyph button for a header affordance (e.g. the rename pencil).
    fn icon_button(
        &self,
        id: &'static str,
        glyph: &'static str,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .id(id)
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_center()
            .size(px(20.))
            .rounded(px(5.))
            .text_color(theme::text_muted())
            .text_size(px(12.))
            .hover(|d| {
                d.bg(theme::tint(theme::text_muted(), 0.12))
                    .text_color(theme::text_secondary())
            })
            .child(glyph)
            .on_click(cx.listener(move |this, _ev, w, cx| handler(this, w, cx)))
            .into_any_element()
    }

    /// The header chevron that condenses/expands the info header: condensed folds away
    /// the whole facts card and the advance/resume chrome (Phase + Trust survive as
    /// compact header dropdowns), handing that height to the always-visible CC terminal.
    fn collapse_button(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let glyph = if self.header_collapsed { "▾" } else { "▴" };
        self.icon_button(
            "session-collapse",
            glyph,
            |this, _w, cx| {
                this.header_collapsed = !this.header_collapsed;
                // Expanding removes the pills, so any open dropdown would dangle.
                this.phase_open = false;
                this.trust_open = false;
                cx.notify();
            },
            cx,
        )
    }

    /// Compact header button surfacing the current **Phase** (condensed header only).
    /// Click opens the [`Self::phase_menu`] dropdown. Clickable only for a steerable
    /// managed session (one we own a PTY for); otherwise it reads as a static chip.
    fn phase_pill(&self, phase: Phase, cx: &mut Context<Self>) -> gpui::AnyElement {
        let color = theme::phase_color(phase);
        let caret = if self.phase_open { "▴" } else { "▾" };
        let mut pill = div()
            .id("phase-pill")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .px_2()
            .py(px(2.))
            .rounded(theme::radius_sm())
            .bg(theme::tint(color, 0.16))
            .text_color(color)
            .text_size(theme::text_sm())
            .font_weight(FontWeight::MEDIUM)
            .child(format!("Phase: {}", phase.label()))
            .child(div().text_size(theme::text_xs()).child(caret));
        if self.terminal.is_some() {
            pill = pill
                .cursor_pointer()
                .hover(|d| d.bg(theme::tint(color, 0.26)))
                .on_click(cx.listener(|this, _e, _w, cx| this.toggle_phase_menu(cx)));
        }
        pill.into_any_element()
    }

    /// Compact header button surfacing the current **Trust** tier (condensed header
    /// only). Click opens the [`Self::trust_menu`] dropdown. Always operator-settable
    /// (trust feeds our PDP, not CC), so it stays clickable regardless of terminal.
    fn trust_pill(&self, tier: TrustTier, cx: &mut Context<Self>) -> gpui::AnyElement {
        let caret = if self.trust_open { "▴" } else { "▾" };
        div()
            .id("trust-pill")
            .cursor_pointer()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .px_2()
            .py(px(2.))
            .rounded(theme::radius_sm())
            .bg(theme::surface_base())
            .text_color(theme::text_secondary())
            .text_size(theme::text_sm())
            .hover(|d| d.bg(theme::surface_overlay()))
            .child(format!("Trust: {}", trust_label(tier)))
            .child(
                div()
                    .text_size(theme::text_xs())
                    .text_color(theme::text_muted())
                    .child(caret),
            )
            .on_click(cx.listener(|this, _e, _w, cx| this.toggle_trust_menu(cx)))
            .into_any_element()
    }

    /// The Phase dropdown: every workflow phase as a selectable row (label + its
    /// operator-mode hint), the current one highlighted. Picking one routes through
    /// [`Self::request_phase`] and closes the menu.
    fn phase_menu(&self, current: Phase, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut menu = div()
            .flex()
            .flex_col()
            .gap(px(1.))
            .p(px(4.))
            .rounded(theme::radius_sm())
            .border_1()
            .border_color(theme::border_subtle())
            .bg(theme::surface_raised());
        for p in Phase::ALL {
            let active = p == current;
            let color = theme::phase_color(p);
            let mut item = div()
                .id(p.label())
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .py(px(3.))
                .rounded(theme::radius_sm())
                .text_size(theme::text_sm())
                .child(
                    div()
                        .w(px(64.))
                        .text_color(if active {
                            color
                        } else {
                            theme::text_secondary()
                        })
                        .child(p.label()),
                )
                .child(
                    div()
                        .text_size(theme::text_xs())
                        .text_color(theme::text_muted())
                        .child(p.mode_label()),
                );
            if active {
                item = item
                    .bg(theme::tint(color, 0.16))
                    .font_weight(FontWeight::SEMIBOLD);
            } else {
                item = item
                    .cursor_pointer()
                    .hover(|d| d.bg(theme::surface_overlay()))
                    .on_click(cx.listener(move |this, _e, window, cx| {
                        this.phase_open = false;
                        this.request_phase(p, window, cx);
                    }));
            }
            menu = menu.child(item);
        }
        menu.into_any_element()
    }

    /// The Trust dropdown: each tier as a selectable row (label + one-line meaning),
    /// the current one highlighted. Picking one routes through [`Self::request_trust`]
    /// and closes the menu.
    fn trust_menu(&self, current: TrustTier, cx: &mut Context<Self>) -> gpui::AnyElement {
        let opts = [
            (TrustTier::Observed, "Everything prompts"),
            (TrustTier::ReadOnly, "Read-only verbs autonomous"),
            (TrustTier::Standard, "Read + low-risk writes autonomous"),
            (TrustTier::Trusted, "Broad autonomy (danger still prompts)"),
        ];
        let mut menu = div()
            .flex()
            .flex_col()
            .gap(px(1.))
            .p(px(4.))
            .rounded(theme::radius_sm())
            .border_1()
            .border_color(theme::border_subtle())
            .bg(theme::surface_raised());
        for (t, desc) in opts {
            let active = t == current;
            let mut item = div()
                .id(trust_label(t))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .py(px(3.))
                .rounded(theme::radius_sm())
                .text_size(theme::text_sm())
                .child(
                    div()
                        .w(px(64.))
                        .text_color(if active {
                            theme::accent()
                        } else {
                            theme::text_secondary()
                        })
                        .child(trust_label(t)),
                )
                .child(
                    div()
                        .text_size(theme::text_xs())
                        .text_color(theme::text_muted())
                        .child(desc),
                );
            if active {
                item = item
                    .bg(theme::tint(theme::accent(), 0.16))
                    .font_weight(FontWeight::SEMIBOLD);
            } else {
                item = item
                    .cursor_pointer()
                    .hover(|d| d.bg(theme::surface_overlay()))
                    .on_click(cx.listener(move |this, _e, _w, cx| {
                        this.trust_open = false;
                        this.request_trust(t, cx);
                    }));
            }
            menu = menu.child(item);
        }
        menu.into_any_element()
    }

    /// Toggle the condensed-header Phase dropdown (mutually exclusive with the Trust
    /// and color popovers, so only one floats below the header at a time).
    fn toggle_phase_menu(&mut self, cx: &mut Context<Self>) {
        self.phase_open = !self.phase_open;
        self.trust_open = false;
        self.color_open = false;
        cx.notify();
    }

    /// Toggle the condensed-header Trust dropdown (mutually exclusive with the Phase
    /// and color popovers).
    fn toggle_trust_menu(&mut self, cx: &mut Context<Self>) {
        self.trust_open = !self.trust_open;
        self.phase_open = false;
        self.color_open = false;
        cx.notify();
    }

    /// A small text pill (Save / Cancel) for the rename row.
    fn name_pill(
        &self,
        id: &'static str,
        label: &'static str,
        color: Hsla,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .id(id)
            .cursor_pointer()
            .px_2()
            .py(px(3.))
            .rounded(px(6.))
            .bg(theme::tint(color, 0.14))
            .text_color(color)
            .text_size(px(11.))
            .child(label)
            .on_click(cx.listener(move |this, _ev, w, cx| handler(this, w, cx)))
            .into_any_element()
    }

    /// The current-color swatch button; click toggles the palette.
    fn color_dot(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let fill = self
            .meta
            .palette_color()
            .map(|c| c.hsla())
            .unwrap_or_else(theme::surface_overlay);
        div()
            .id("session-color-dot")
            .cursor_pointer()
            .size(px(14.))
            .rounded_full()
            .bg(fill)
            .border_1()
            .border_color(theme::border_strong())
            .hover(|d| d.border_color(theme::text_secondary()))
            .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_color_palette(cx)))
            .into_any_element()
    }

    /// The expandable swatch palette: every color plus a "clear" option.
    fn color_palette(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let current = self.meta.palette_color();
        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_wrap()
            .gap(px(6.))
            .px_2()
            .py(px(6.))
            .rounded(theme::radius_sm())
            .bg(theme::surface_raised())
            .child(
                div()
                    .min_w(px(48.))
                    .text_size(px(11.))
                    .text_color(theme::text_muted())
                    .child(current.map(|c| c.label()).unwrap_or("None")),
            );
        for color in SessionColor::ALL {
            let selected = current == Some(color);
            row = row.child(
                div()
                    .id(("session-swatch", color as usize))
                    .cursor_pointer()
                    .size(px(18.))
                    .rounded_full()
                    .bg(color.hsla())
                    .border_2()
                    .border_color(if selected {
                        theme::text_primary()
                    } else {
                        theme::surface_raised()
                    })
                    .hover(|d| d.border_color(theme::text_secondary()))
                    .on_click(
                        cx.listener(move |this, _ev, _w, cx| this.pick_color(Some(color), cx)),
                    ),
            );
        }
        row.child(
            div()
                .id("session-swatch-clear")
                .cursor_pointer()
                .flex()
                .items_center()
                .justify_center()
                .size(px(18.))
                .rounded_full()
                .border_1()
                .border_color(theme::border_strong())
                .text_color(theme::text_muted())
                .text_size(px(11.))
                .child("✕")
                .on_click(cx.listener(|this, _ev, _w, cx| this.pick_color(None, cx))),
        )
        .into_any_element()
    }

    /// A small toggle pill for the project's **auto-resume** opt-in (managed sessions
    /// only). When on, a stall under this project re-prompts the session to continue
    /// (see [`Self::maybe_auto_resume`]). State is the project's, so it reads/writes the
    /// space flag — flipping it affects every session under the same root.
    fn auto_resume_button(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let on = match (self.space_root(), cx.try_global::<ShellDeps>().cloned()) {
            (Some(root), Some(deps)) => deps.focus.read(cx).project_auto_resume(&root),
            _ => false,
        };
        let (bg, fg) = if on {
            (theme::tint(theme::accent(), 0.18), theme::accent())
        } else {
            (
                theme::tint(theme::text_muted(), 0.12),
                theme::text_secondary(),
            )
        };
        div()
            .id("session-auto-resume")
            .cursor_pointer()
            .px_2()
            .py(px(3.))
            .rounded(px(6.))
            .bg(bg)
            .text_color(fg)
            .text_size(px(11.))
            .child(if on {
                "⟳ auto-resume: on"
            } else {
                "⟳ auto-resume: off"
            })
            .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_auto_resume(cx)))
            .into_any_element()
    }

    /// Flip the project's auto-resume opt-in (persists to the project space).
    fn toggle_auto_resume(&mut self, cx: &mut Context<Self>) {
        let Some(deps) = cx.try_global::<ShellDeps>().cloned() else {
            return;
        };
        let Some(root) = self.space_root() else {
            return;
        };
        let now = deps.focus.read(cx).project_auto_resume(&root);
        deps.focus
            .update(cx, |ps, _cx| ps.set_project_auto_resume(&root, !now));
        cx.notify();
    }

    /// A small "↻ Reset" pill in the card header (managed sessions only).
    fn reset_button(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .id("session-reset")
            .cursor_pointer()
            .px_2()
            .py(px(3.))
            .rounded(px(6.))
            .bg(theme::tint(theme::text_muted(), 0.12))
            .text_color(theme::text_secondary())
            .text_size(px(11.))
            .child("↻ Reset")
            .on_click(cx.listener(|this, _ev, window, cx| this.reset_session(window, cx)))
            .into_any_element()
    }

    /// Reset the session: replace it with a **fresh managed session under a new id**.
    ///
    /// NOT `/clear` — CC's `/clear` continues the same terminal under a *new* session
    /// id (verified against real transcripts), so the old managed record would forever
    /// point at the dead pre-clear conversation and a later restore would come up
    /// empty. Instead: carry the operator's name/color to a new id, forget the old
    /// session (record + fleet tile; the transcript on disk survives), launch the
    /// successor through the normal managed-launch path, and close this tab.
    fn reset_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(deps) = cx.try_global::<ShellDeps>().cloned() else {
            return;
        };
        let new_id = SessionId::new(uuid::Uuid::new_v4().to_string());
        let phase = self
            .session
            .as_ref()
            .map(|s| s.phase)
            .unwrap_or(Phase::Plan);

        if let Some(root) = self.space_root() {
            // The operator named/colored the *session concept* — the successor keeps it.
            let meta = session_meta::get(&root, &self.id);
            if meta != SessionMeta::default() {
                deps.session_meta.update(cx, |c, cx| {
                    c.put(&root, &new_id, meta);
                    cx.notify();
                });
            }
            // Drop the old roster entry (the launch arm records the successor's).
            crate::views::space_sessions::remove(&root, &self.id);
        }
        // Old record + fleet entry + grid tile go; transcript on disk stays.
        let _ = deps.commands.send(Command::ForgetSession {
            session: self.id.clone(),
        });
        // Launch the successor (records store/roster, opens its tab + terminal). The
        // emit is queued and flushed at the end of *this* effect cycle, so the new tab
        // is added before the spawned close below runs on the next tick.
        deps.center.update(cx, |_c, cx| {
            // The successor keeps the same agent backend as the session being reset.
            cx.emit(OpenRequest::NewManagedSession {
                id: new_id,
                phase,
                agent: self.agent,
            });
        });
        // …then retire this tab (it shows the dead id). Deferred to the next tick rather
        // than closed synchronously here: closing it inline would leave the tab panel
        // momentarily empty (the successor opens via the deferred emit above), and an
        // empty dock tears the window down — the "Reset closes the window" crash. By the
        // time this spawn resumes, the successor tab exists, so the panel is never empty.
        //
        // The close runs inside `cx.update` (window + App), **not** inside a
        // `SessionMonitor` lease: `remove_panel` fires `PanelView::on_removed` on this
        // very panel (which *is* the `SessionMonitor` entity), so re-entering through
        // `this.update_in` here double-leases the entity and aborts the process
        // ("cannot update SessionMonitor while it is already being updated"). Capturing
        // the tab-panel handle + our own entity up-front lets the removal lease the
        // entity itself. Mirrors `PlanReviewPanel::close_and_return_to_session`.
        let tab_panel = self.tab_panel.clone();
        let me = cx.entity();
        cx.spawn_in(window, async move |_, cx| {
            let _ = cx.update(|window, cx| {
                super::close_this_tab(&tab_panel, me, window, cx);
            });
        })
        .detach();
    }

    /// A small "⊜ Compact" pill beside ↻ Reset (managed sessions only) — runs CC's
    /// `/compact` in the embedded terminal. Unlike Reset, `/compact` keeps the
    /// session id (it summarizes the conversation in place), so no record or tab
    /// surgery is needed: the terminal, transcript, and managed record all survive.
    fn compact_button(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .id("session-compact")
            .cursor_pointer()
            .px_2()
            .py(px(3.))
            .rounded(px(6.))
            .bg(theme::tint(theme::text_muted(), 0.12))
            .text_color(theme::text_secondary())
            .text_size(px(11.))
            .child("⊜ Compact")
            .on_click(cx.listener(|this, _ev, _window, cx| this.compact_session(cx)))
            .into_any_element()
    }

    /// Send `/compact` to the live CC TUI (the same write-through pattern as
    /// `/rename` and `/color`). Releases any armed auto-injection first so the
    /// next message can't trigger a second compaction.
    fn compact_session(&mut self, cx: &mut Context<Self>) {
        let Some(term) = &self.terminal else {
            return;
        };
        term.update(cx, |t, _| {
            t.disarm_injection();
            t.send_text("/compact\r");
        });
        self.compact_armed = false;
        cx.notify();
    }

    /// Fold the obs read-model into the auto-compact policy ([`auto_compact`]):
    /// arm the embedded terminal's `/compact` injection when context usage crosses
    /// the configured threshold (it fires before the operator's next message), and
    /// release it once compaction brings usage back under the re-arm level.
    fn check_auto_compact(&mut self, obs: &Entity<ObsStore>, cx: &mut Context<Self>) {
        let Some(term) = self.terminal.clone() else {
            return;
        };
        // Context % exactly as the status bar shows it (CC's native figure when
        // the payload carries one, else tokens ÷ window).
        let Some(pct) = obs
            .read(cx)
            .get(&self.id)
            .and_then(crate::obs::SessionObs::ctx_pct)
        else {
            return;
        };
        match auto_compact::decide(pct, self.compact_armed, &self.compact_cfg) {
            auto_compact::Decision::Arm => {
                term.update(cx, |t, _| t.arm_injection("/compact\r".to_string()));
                self.compact_armed = true;
                if let Some(n) = cx
                    .try_global::<ShellDeps>()
                    .map(|d| d.notifications.clone())
                {
                    let text = format!(
                        "Context {pct}% — compacting before your next message · {}",
                        self.title_text()
                    );
                    n.update(cx, |n, cx| {
                        n.push(NotificationKind::Compact, text, Some(self.id.clone()));
                        cx.notify();
                    });
                }
            }
            auto_compact::Decision::Disarm => {
                term.update(cx, |t, _| t.disarm_injection());
                self.compact_armed = false;
            }
            auto_compact::Decision::Hold => {}
        }
    }

    /// The **Phase** row: the six-stage workflow as a clickable pipeline
    /// (Plan › Auto › Test › Review › Commit) with the current stage
    /// lit and a trailing "→ next" hint (the *incoming* phase). Distinct from the
    /// Mode row: Mode is CC's coarse permission substrate, Phase is **our**
    /// fine-grained workflow authority — and the operator may jump to any stage,
    /// including the engine-only Test/Review/Commit no auto-transition reaches.
    /// Interactive only for a steerable managed session (one we own a PTY for);
    /// observed/external sessions show a read-only reflection.
    fn phase_stepper(&self, phase: Phase, cx: &mut Context<Self>) -> gpui::AnyElement {
        let label = div()
            .w(px(52.))
            .text_color(theme::text_muted())
            .child("Phase");

        let interactive = self.terminal.is_some();
        let seg = |p: Phase, cx: &mut Context<Self>| {
            let active = p == phase;
            let mut s = div()
                .id(p.label())
                .px_2()
                .py(px(2.))
                .rounded(theme::radius_sm())
                .text_size(theme::text_sm());
            s = if active {
                s.bg(theme::tint(theme::phase_color(p), 0.18))
                    .text_color(theme::phase_color(p))
                    .font_weight(FontWeight::SEMIBOLD)
            } else {
                s.text_color(theme::text_muted())
            };
            s = s.child(p.label());
            if interactive && !active {
                s = s.cursor_pointer().on_click(
                    cx.listener(move |this, _e, window, cx| this.request_phase(p, window, cx)),
                );
            }
            s
        };

        let mut pipeline = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(1.))
            .p(px(2.))
            .rounded(theme::radius_sm())
            .bg(theme::surface_base());
        for (i, p) in Phase::ALL.iter().enumerate() {
            if i > 0 {
                pipeline = pipeline.child(
                    div()
                        .text_size(theme::text_xs())
                        .text_color(theme::text_muted())
                        .child("›"),
                );
            }
            pipeline = pipeline.child(seg(*p, cx));
        }

        // The "incoming phase" hint — the suggested forward step (cyclic: after
        // Commit it loops back to Plan).
        let next_hint = div()
            .text_size(theme::text_xs())
            .text_color(theme::text_muted())
            .child(format!("→ next: {}", phase.next().label()));

        // When the operator has pinned the phase (manual override of auto-advance),
        // show a lock chip that resumes auto in place.
        let pinned = self
            .session
            .as_ref()
            .map(|s| s.phase_pinned)
            .unwrap_or(false);
        let pin_chip = (pinned && interactive).then(|| {
            div()
                .id("phase-unpin")
                .cursor_pointer()
                .text_size(theme::text_xs())
                .text_color(theme::accent())
                .child("🔒 pinned — resume auto")
                .on_click(cx.listener(|this, _e, _w, cx| this.unpin_phase(cx)))
        });

        let value = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(pipeline)
            .child(next_hint)
            .children(pin_chip)
            .into_any_element();

        mode_row(label, value)
    }

    /// Clear the operator pin (resume auto-advance in place). Sends
    /// [`Command::SetPhasePinned`]`(false)` + an optimistic local flip.
    fn unpin_phase(&mut self, cx: &mut Context<Self>) {
        if let Some(tx) = cx.try_global::<ShellDeps>().map(|d| d.commands.clone()) {
            let _ = tx.send(Command::SetPhasePinned {
                session: self.id.clone(),
                pinned: false,
            });
        }
        if let Some(s) = self.session.as_mut() {
            s.phase_pinned = false;
        }
        cx.notify();
    }

    /// Confirm the operator-gated advance to the next phase (Test→Review,
    /// Commit→new cycle, or a pinned-advance approval). Sends
    /// [`Command::AdvancePhase`]; the engine moves the phase and clears the pin.
    fn confirm_advance(&mut self, cx: &mut Context<Self>) {
        if let Some(tx) = cx.try_global::<ShellDeps>().map(|d| d.commands.clone()) {
            let _ = tx.send(Command::AdvancePhase {
                session: self.id.clone(),
            });
        }
        self.pending_advance = None;
        cx.notify();
    }

    /// The operator-confirmed advance affordance for this session's card:
    /// the pinned "Advance to <to>?" prompt, or the `Test`/`Commit` "done →"
    /// button (enabled only when CC isn't actively working). `None` otherwise.
    fn advance_affordance(
        &self,
        phase: Phase,
        status: SessionStatus,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        // Only a steerable managed session is advanceable from the cockpit.
        if self.terminal.is_none() {
            return None;
        }
        let (text, enabled): (String, bool) = if let Some(to) = self.pending_advance {
            (format!("Advance to {}? (pinned)", to.label()), true)
        } else {
            // CC must be idle (proxy for "tasks resolved / no more work").
            let idle = resumable(status);
            match phase {
                Phase::Test => ("Tests done → Review".to_string(), idle),
                Phase::Commit => ("Committed → new cycle".to_string(), idle),
                _ => return None,
            }
        };

        let mut btn = div()
            .id("phase-advance")
            .px_3()
            .py(px(5.))
            .rounded(theme::radius_sm())
            .text_size(theme::text_sm());
        btn = if enabled {
            btn.cursor_pointer()
                .bg(theme::tint(theme::accent(), 0.16))
                .text_color(theme::accent())
                .child(text)
                .on_click(cx.listener(|this, _e, _w, cx| this.confirm_advance(cx)))
        } else {
            btn.bg(theme::surface_raised())
                .text_color(theme::text_muted())
                .child(format!("{text} (available when the agent isn't working)"))
        };
        Some(div().flex().flex_row().child(btn).into_any_element())
    }

    /// Set the session's workflow phase (operator authority, any of the five).
    /// Routed to the engine via [`Command::SetPhase`] + an optimistic card flip.
    ///
    /// **No relaunch, ever.** Every phase runs the agent in the same native `auto`
    /// substrate ([`Phase::cc_permission_mode`] is constant), so a phase change is a
    /// PDP-only flip — nothing about the running process has to change. The engine
    /// tells the agent what a new phase *means* (the Plan aim) by injecting it into
    /// the live session, not by restarting it.
    fn request_phase(&mut self, target: Phase, _window: &mut Window, cx: &mut Context<Self>) {
        if self.session.as_ref().map(|s| s.phase) == Some(target) {
            return;
        }

        if let Some(tx) = cx.try_global::<ShellDeps>().map(|d| d.commands.clone()) {
            let _ = tx.send(Command::SetPhase {
                session: self.id.clone(),
                phase: target,
            });
        }
        if let Some(s) = self.session.as_mut() {
            s.phase = target;
            s.mode = target.operator_mode();
        }
        cx.notify();
    }

    /// The **Trust** row: an operator-settable tier (Observe · Read · Std · Trusted).
    /// Unlike Mode, this doesn't steer CC — it feeds *our* PDP. **Reserved for now:**
    /// the PDP only consults the tier for embedded-MCP *actor verbs* (run-with-coverage,
    /// query-db, open-review, …), and that server isn't wired yet, so the live CC hook
    /// path (`gate::evaluate`, `verb: None`) never reads it. CC's real gating today is
    /// adoption + phase + danger, not trust. Surfaced with a muted hint so the dial
    /// doesn't read as modulating CC's permissions when it currently doesn't.
    fn trust_selector(&self, tier: TrustTier, cx: &mut Context<Self>) -> gpui::AnyElement {
        let label = div()
            .w(px(52.))
            .text_color(theme::text_muted())
            .child("Trust");
        let seg = |t: TrustTier, text: &'static str, id: &'static str, cx: &mut Context<Self>| {
            let active = tier == t;
            let mut s = div()
                .id(id)
                .px_2()
                .py(px(2.))
                .rounded(theme::radius_sm())
                .text_size(theme::text_sm());
            s = if active {
                s.bg(theme::tint(theme::accent(), 0.18))
                    .text_color(theme::accent())
            } else {
                s.text_color(theme::text_muted())
            };
            s = s.child(text);
            if !active {
                s = s
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _e, _w, cx| this.request_trust(t, cx)));
            }
            s
        };
        let segmented = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.))
            .p(px(2.))
            .rounded(theme::radius_sm())
            .bg(theme::surface_base())
            .child(seg(TrustTier::Observed, "Observe", "trust-observed", cx))
            .child(seg(TrustTier::ReadOnly, "Read", "trust-read", cx))
            .child(seg(TrustTier::Standard, "Std", "trust-standard", cx))
            .child(seg(TrustTier::Trusted, "Trusted", "trust-trusted", cx));
        // Reserved dial (see doc above): flag that it doesn't gate CC's tools yet.
        let hint = div()
            .text_size(theme::text_sm())
            .text_color(theme::text_muted())
            .child("· reserved (gates MCP verbs)");
        let value = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(segmented)
            .child(hint);
        mode_row(label, value.into_any_element())
    }

    /// Set the session's trust tier — engine policy (`Command::SetTrust`), with an
    /// optimistic card update so it reflects instantly.
    fn request_trust(&mut self, tier: TrustTier, cx: &mut Context<Self>) {
        if self.session.as_ref().map(|s| s.trust_tier) == Some(tier) {
            return;
        }
        if let Some(s) = self.session.as_mut() {
            s.trust_tier = tier;
        }
        if let Some(deps) = cx.try_global::<ShellDeps>().cloned() {
            let _ = deps.commands.send(Command::SetTrust {
                session: self.id.clone(),
                tier,
            });
            // The alignment is already done for this session, so block the one-shot re-apply.
            self.applied_project_trust = true;
            // Trust scope: the *project* default is only established once. When no global
            // trust exists yet, this first pick sets it (so later sessions and restarts
            // inherit it and we don't re-prompt). But when a global already exists, a
            // per-session change is a **session-scoped override** — it must NOT rewrite the
            // project default (that would flip every other session under this root). The
            // override persists on its own via the engine's per-session trust store.
            if let Some(root) = self.space_root() {
                let has_global = deps.focus.read(cx).project_trust(&root).is_some();
                if !has_global {
                    deps.focus
                        .update(cx, |ps, _cx| ps.set_project_trust(&root, tier));
                }
            }
        }
        cx.notify();
    }

    /// Focus the embedded terminal's handle — drops the caret straight into the live
    /// Claude Code TUI. No-op when there's no terminal (observed session). Used on
    /// takeover/relaunch, where the terminal is swapped onto an already-active tab and
    /// no tab activation fires to trigger the dock's `focus_active_panel`.
    fn focus_terminal(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(term) = &self.terminal {
            let handle = term.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    /// Move this session tab into a new pane split off toward `placement` — the menu
    /// equivalent of dragging the tab to that edge. Unlike the editor split, a session
    /// is **moved**, not duplicated: it owns a live terminal (a running `claude`), so a
    /// second copy would put two clients on one transcript. We detach from the current
    /// tab panel and re-add via a split (mirroring gpui-component's own drop path).
    fn split(&mut self, placement: Placement, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab_panel) = self.tab_panel.as_ref().and_then(|w| w.upgrade()) else {
            return;
        };
        // `Entity<SessionMonitor>` is itself a `PanelView`, so we can hand ourselves
        // back to the tab panel to relocate (the same entity → same live terminal).
        let me: Arc<dyn PanelView> = Arc::new(cx.entity());
        tab_panel.update(cx, |tp, cx| {
            tp.remove_panel(me.clone(), window, cx);
            tp.add_panel_at(me, placement, None, window, cx);
        });
    }

    fn split_right(&mut self, _: &SplitRight, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Placement::Right, window, cx);
    }

    fn split_left(&mut self, _: &SplitLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Placement::Left, window, cx);
    }

    fn split_up(&mut self, _: &SplitUp, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Placement::Top, window, cx);
    }

    fn split_down(&mut self, _: &SplitDown, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Placement::Bottom, window, cx);
    }

    /// The "Resume in terminal" action for an observed session not yet taken over.
    /// Returns `None` once resumed (or when there's no repo to resume into). The
    /// button is enabled only while the session is idle/done; otherwise it shows a
    /// disabled hint (resuming a live session would conflict).
    fn resume_bar(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.terminal.is_some() || self.resume_root.is_none() {
            return None;
        }
        let enabled = self.is_resumable();
        let mut button = div()
            .id("resume-session")
            .px_3()
            .py(px(5.))
            .rounded(theme::radius_sm())
            .text_size(theme::text_sm());
        button = if enabled {
            button
                .cursor_pointer()
                .bg(theme::tint(theme::accent(), 0.16))
                .text_color(theme::accent())
                .child("▶ Resume in terminal")
                .on_click(cx.listener(|this, _ev, window, cx| this.try_resume(window, cx)))
        } else {
            button
                .bg(theme::surface_raised())
                .text_color(theme::text_muted())
                .child("▶ Resume in terminal (available when the agent isn't working)")
        };
        Some(div().flex().flex_row().child(button).into_any_element())
    }

    /// The live region under the facts: the embedded terminal (managed session),
    /// the read-only message history (observed session), or a waiting placeholder.
    fn transcript(&self) -> gpui::AnyElement {
        if let Some(term) = &self.terminal {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(160.))
                .rounded(theme::radius_md())
                .border_1()
                .border_color(theme::border_subtle())
                .overflow_hidden()
                .child(term.clone())
                .into_any_element();
        }
        if self.messages.is_empty() {
            return transcript_placeholder().into_any_element();
        }
        div()
            .id("transcript")
            .flex()
            .flex_col()
            .gap(px(8.))
            .flex_1()
            .min_h(px(120.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .rounded(theme::radius_md())
            .border_1()
            .border_color(theme::border_subtle())
            .bg(theme::surface_raised())
            .p_3()
            .children(
                self.messages
                    .iter()
                    .enumerate()
                    .map(|(i, m)| message_row(i, m)),
            )
            .into_any_element()
    }
}

/// Longest single message rendered in full; the rest is summarized so one giant
/// turn doesn't dominate the history.
const MAX_MESSAGE_LINES: usize = 30;

/// Render one transcript message as a card with a role-colored left accent and a
/// small role label. Assistant turns render as markdown; the body is capped at
/// [`MAX_MESSAGE_LINES`] so one giant turn can't dominate the history.
fn message_row(idx: usize, msg: &crate::transcript::Message) -> impl IntoElement {
    use crate::transcript::Role;
    let (label, color) = match msg.role {
        Role::User => ("you", theme::accent()),
        Role::Assistant => ("claude", theme::text_secondary()),
    };

    let lines: Vec<&str> = msg.text.lines().collect();
    let overflow = lines.len().saturating_sub(MAX_MESSAGE_LINES);
    let body_src = lines
        .iter()
        .take(MAX_MESSAGE_LINES)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");

    // Assistant turns are markdown (headings/lists/code/bold); user turns are shown
    // verbatim (they're plain prose, and markdown could misrender pasted content).
    let body = match msg.role {
        Role::Assistant => TextView::markdown(("msg", idx), body_src)
            .style(theme::markdown_style())
            .into_any_element(),
        Role::User => div()
            .text_size(theme::text_sm())
            .text_color(theme::text_primary())
            .children(body_src.lines().map(|l| {
                div().child(if l.is_empty() {
                    " ".to_string()
                } else {
                    l.to_string()
                })
            }))
            .into_any_element(),
    };

    let mut card = div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .border_l_2()
        .border_color(color)
        .pl_2()
        .child(
            div()
                .text_size(theme::text_2xs())
                .text_color(color)
                .child(label),
        )
        .child(body);
    if overflow > 0 {
        card = card.child(
            div()
                .text_size(theme::text_2xs())
                .text_color(theme::text_muted())
                .child(format!("… {overflow} more line(s)")),
        );
    }
    card
}

/// A labelled value row. `mono` renders the value in the monospace font (for
/// addresses like paths).
fn field(label: &str, value: &str, value_color: Hsla, mono: bool) -> impl IntoElement {
    let value_el = div()
        .flex_1()
        .overflow_hidden()
        .text_color(value_color)
        .child(value.to_string());
    div()
        .flex()
        .flex_row()
        .gap_2()
        .text_size(theme::text_sm())
        .child(
            div()
                .w(px(52.))
                .text_color(theme::text_muted())
                .child(label.to_string()),
        )
        .child(if mono {
            value_el.font_family(theme::mono_font())
        } else {
            value_el
        })
}

/// The short operator-facing label for a trust tier (mirrors the segmented
/// `trust_selector` text, reused by the condensed-header pill + dropdown).
fn trust_label(tier: TrustTier) -> &'static str {
    match tier {
        TrustTier::Observed => "Observe",
        TrustTier::ReadOnly => "Read",
        TrustTier::Standard => "Std",
        TrustTier::Trusted => "Trusted",
    }
}

/// A labelled row carrying an arbitrary value element (the Mode selector), laid
/// out like [`field`] so it lines up with the other facts.
fn mode_row(label: gpui::Div, value: gpui::AnyElement) -> gpui::AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .text_size(theme::text_sm())
        .child(label)
        .child(value)
        .into_any_element()
}

/// Reserved area for the live session terminal/transcript (engine-owned feed).
fn transcript_placeholder() -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(80.))
        .rounded(theme::radius_md())
        .border_1()
        .border_color(theme::border_subtle())
        .bg(theme::surface_raised())
        .p_3()
        .child(
            div()
                .text_size(theme::text_xs())
                .text_color(theme::text_muted())
                .child("Live transcript — coming with the session feed"),
        )
}

#[cfg(test)]
mod tests {
    use super::{action_prompt, server_glob};

    #[test]
    fn server_glob_covers_the_whole_server() {
        assert_eq!(
            server_glob("mcp__phoenix__run_select_query"),
            "mcp__phoenix__*"
        );
        // A tool with extra `__` in its leaf still globs at the server boundary.
        assert_eq!(
            server_glob("mcp__phoenix__open_snowflake_request"),
            "mcp__phoenix__*"
        );
        // A non-MCP-shaped name falls back to itself (never an over-broad glob).
        assert_eq!(server_glob("weird"), "weird");
        assert_eq!(server_glob("mcp__only"), "mcp__only");
    }

    #[test]
    fn action_prompt_reads_request_phase_holds_as_phase_changes() {
        // A `request_phase` hold ("RequestPhase <token>") renders as a phase prompt
        // naming the target phase's label.
        let (title, body) = action_prompt("RequestPhase auto");
        assert_eq!(title, "Authorize phase change");
        assert!(body.contains("Auto"), "{body}");
        // `next` / empty resolve to a generic "next phase".
        let (_, body) = action_prompt("RequestPhase next");
        assert!(body.contains("next phase"), "{body}");
        let (_, body) = action_prompt("RequestPhase");
        assert!(body.contains("next phase"), "{body}");
        // Anything else is shown verbatim under a generic title.
        let (title, body) = action_prompt("rm -rf /tmp/scratch");
        assert_eq!(title, "Approve request");
        assert_eq!(body, "rm -rf /tmp/scratch");
    }
}
