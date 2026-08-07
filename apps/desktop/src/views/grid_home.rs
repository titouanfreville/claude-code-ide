//! Grid-home — the fleet overview and triage home base (UX spec §center-pane
//! inversion + needs-you queue). Renders the whole fleet as a wrapping grid of
//! session tiles, with a header that surfaces the "who needs me" count.
//!
//! In the IDE-classic shell this is the **center dock panel** (a "dockable
//! plugin": a [`gpui_component::dock::Panel`]). Clicking a tile focuses that
//! session — the file-tree and terminal panels follow that focus via the shared
//! [`ProjectSpace`] (UX spec: "follow focused session"; FR8).
//!
//! The pure read-model lives in [`FleetModel`] so it is unit-testable without a
//! GPUI context; `GridHome` is the thin view/panel wrapper around it. `FleetModel`
//! folds engine `EngineEvent`s — the seam the supervisor's `EventBus` drives.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    div, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, SharedString,
    Task, Window,
};
use gpui_component::dock::{Panel, PanelEvent};

use std::collections::HashMap;

use moonlight_domain::agent::AgentKind;
use moonlight_domain::ids::SessionId;
use moonlight_domain::phase::Phase;
use moonlight_domain::ports::store::{ManagedSession, ManagedSessionStore, SessionChangeStore};
use moonlight_domain::session::{AttentionKind, Session, SessionStatus};
use moonlight_engine::{Command, EngineEvent};
use tokio::sync::broadcast;
use tokio::sync::mpsc::UnboundedSender;

use super::center_requests::OpenRequest;
use super::project_space::ProjectSpace;
use super::session_meta::{SessionMeta, SessionMetaCache};
use super::session_tile::session_tile;
use super::theme;
use super::workspace::ShellDeps;

/// How the fleet grid orders its tiles. The header toggle flips between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortMode {
    /// "Needs you" first: triage rank (waiting/errored float to the top), arrival
    /// order breaking ties. The default — triage is the home surface's whole point.
    #[default]
    Triage,
    /// Most-recently-active first (`last_activity` descending). For "what was I just
    /// working on" rather than "what needs me".
    Recent,
}

/// Pure fleet read-model: the `Vec<Session>` plus the folding/ordering logic.
/// No GPUI — directly unit-testable.
#[derive(Default)]
pub struct FleetModel {
    sessions: Vec<Session>,
    /// Live attention overlay (the ⚠ "did not complete / is stuck" signal), folded
    /// from [`EngineEvent::SessionAlert`]. Transient — not persisted (like status);
    /// cleared when a session starts working again. Overlays `AttentionKind::from_status`.
    alerts: HashMap<SessionId, AttentionKind>,
}

impl FleetModel {
    #[cfg(test)]
    fn from_sessions(sessions: Vec<Session>) -> Self {
        Self {
            sessions,
            alerts: HashMap::new(),
        }
    }

    /// Add any persisted managed records the model doesn't already hold (boot
    /// pull-seed + ⟳ Refresh re-sync). The grid is fed by the engine's one-shot
    /// rehydration *broadcast*, which a late-subscribing panel misses — so the grid
    /// also pulls the fleet straight from the store. **Merge-only**: an existing
    /// session (carrying live status from detection) is never downgraded back to the
    /// record's resting `Idle`.
    fn seed_missing(&mut self, records: &[ManagedSession]) {
        for m in records {
            if !self.sessions.iter().any(|s| s.id == m.id) {
                self.sessions.push(m.to_session());
            }
        }
    }

    /// Fill in missing titles via `lookup` (the transcript's latest custom/ai title),
    /// so tiles show a name instead of the raw id for records persisted before title
    /// storage existed — and for idle observed sessions detection isn't tailing.
    fn backfill_titles(&mut self, lookup: impl Fn(&str) -> Option<String>) {
        for s in self.sessions.iter_mut().filter(|s| s.title.is_none()) {
            s.title = lookup(s.id.as_str());
        }
    }

    /// Fold one engine event into the read-model.
    ///
    /// `SessionUpserted` carries a full `Session`, so it can *add* a tile or
    /// replace one wholesale (new session, title change); the thin events update
    /// a single field of an already-known session.
    pub fn apply_event(&mut self, event: &EngineEvent) {
        match event {
            EngineEvent::SessionUpserted { session } => {
                self.clear_alert_if_working(&session.id, session.status);
                match self.session_mut(&session.id) {
                    Some(existing) => *existing = session.clone(),
                    None => self.sessions.push(session.clone()),
                }
            }
            EngineEvent::SessionStateChanged { session, status } => {
                self.clear_alert_if_working(session, *status);
                if let Some(s) = self.session_mut(session) {
                    s.status = *status;
                }
            }
            // A forgotten session (e.g. replaced by ↻ Reset) loses its tile + alert.
            EngineEvent::SessionRemoved { session } => {
                self.sessions.retain(|s| &s.id != session);
                self.alerts.remove(session);
            }
            // The attention overlay: raise (Stuck/Incomplete) or clear the ⚠ signal.
            EngineEvent::SessionAlert { session, alert } => match alert {
                Some(kind) => {
                    self.alerts.insert(session.clone(), *kind);
                }
                None => {
                    self.alerts.remove(session);
                }
            },
            EngineEvent::PhaseTransitioned { session, phase } => {
                if let Some(s) = self.session_mut(session) {
                    s.phase = *phase;
                }
            }
            // Surfaced by other views (plan/code review, needs-you queue, audit,
            // HUD); they don't change the tile read-model itself.
            EngineEvent::PlanProposed { .. }
            | EngineEvent::SummaryObserved { .. }
            | EngineEvent::ReviewReady { .. }
            | EngineEvent::ApprovalRequested { .. }
            | EngineEvent::PhaseAdvanceRequested { .. }
            | EngineEvent::AuditAppended { .. }
            | EngineEvent::GovernorActed { .. } => {}
        }
    }

    fn session_mut(&mut self, id: &SessionId) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| &s.id == id)
    }

    /// Clear a session's attention overlay when it **transitions into** `Running` (it
    /// recovered / was restarted and is working again). Edge-triggered against the
    /// session's *current* status so a self-reported `Stuck` raised mid-turn (already
    /// Running) doesn't immediately flicker away on the next Running poll — call this
    /// **before** updating the stored status.
    fn clear_alert_if_working(&mut self, id: &SessionId, new_status: SessionStatus) {
        if new_status == SessionStatus::Running {
            let was_running = self
                .sessions
                .iter()
                .find(|s| &s.id == id)
                .is_some_and(|s| s.status == SessionStatus::Running);
            if !was_running {
                self.alerts.remove(id);
            }
        }
    }

    /// The attention signal for a session: the live overlay (Stuck/Incomplete) if any,
    /// else what its resting [`SessionStatus`] implies (NeedsInput/Errored). The badge
    /// the tile draws.
    fn attention(&self, session: &Session) -> Option<AttentionKind> {
        self.alerts
            .get(&session.id)
            .copied()
            .or_else(|| AttentionKind::from_status(session.status))
    }

    #[cfg(test)]
    fn session(&self, id: &SessionId) -> Option<&Session> {
        self.sessions.iter().find(|s| &s.id == id)
    }

    /// Sessions in triage order: "what needs me" first. Stable, so arrival order
    /// breaks ties within a rank.
    #[cfg(test)]
    fn triage_ordered(&self) -> Vec<&Session> {
        self.visible(None, SortMode::Triage, false)
    }

    /// Whether a session belongs to the active space (overview = always true).
    fn in_scope(s: &Session, active_root: Option<&Path>) -> bool {
        match active_root {
            None => true,
            Some(root) => s
                .attached_path
                .as_deref()
                .map(|p| super::project_space::expand_home(p).as_path() == root)
                .unwrap_or(false),
        }
    }

    /// Sessions visible for the active space, ordered per `sort`. `active_root = None`
    /// (overview) shows the whole fleet; `Some(root)` scopes to sessions whose
    /// `attached_path` resolves to that space's root. Soft-hidden sessions are filtered
    /// out unless `show_hidden`. Stable sort, so arrival order breaks ties within a rank.
    fn visible(
        &self,
        active_root: Option<&Path>,
        sort: SortMode,
        show_hidden: bool,
    ) -> Vec<&Session> {
        let mut ordered: Vec<&Session> = self
            .sessions
            .iter()
            .filter(|s| show_hidden || !s.hidden)
            .filter(|s| Self::in_scope(s, active_root))
            .collect();
        match sort {
            SortMode::Triage => ordered.sort_by_key(|s| s.status.triage_rank()),
            // Newest activity first; the sort is stable so equal timestamps keep
            // arrival order.
            SortMode::Recent => {
                ordered.sort_by_key(|s| std::cmp::Reverse(s.last_activity.as_millis()))
            }
        }
        ordered
    }

    /// How many in-scope sessions are currently soft-hidden — drives the "show
    /// hidden (N)" toggle label (and lets the grid hide the toggle when N is 0).
    fn hidden_count(&self, active_root: Option<&Path>) -> usize {
        self.sessions
            .iter()
            .filter(|s| s.hidden && Self::in_scope(s, active_root))
            .count()
    }
}

pub struct GridHome {
    model: FleetModel,
    /// Shared focus state the tree/terminal follow; `None` for test/static views.
    focus: Option<Entity<ProjectSpace>>,
    /// Channel to send operator intents (e.g. adoption) to the engine supervisor;
    /// `None` for test/static views.
    commands: Option<UnboundedSender<Command>>,
    /// Starting phase for the next "＋ New session" launch, set by the toggle
    /// beside the button. Default `Plan` (the default-deny safety posture).
    new_session_phase: Phase,
    /// Agent CLI backend for the next "＋ New session" launch, set by the toggle
    /// beside the button. Default [`AgentKind::ClaudeCode`].
    new_session_agent: AgentKind,
    /// How the grid orders tiles (header toggle). Default triage ("needs you" first).
    sort_mode: SortMode,
    /// Whether soft-hidden sessions are revealed (header toggle). Default off.
    show_hidden: bool,
    focus_handle: FocusHandle,
    /// Observable cache of per-session custom name/color, so tiles render metadata
    /// with an in-memory lookup (no per-tile disk read). `None` for test/static views
    /// (no shell global). Observed in [`Self::new`] so an edit re-renders the grid.
    meta_cache: Option<Entity<SessionMetaCache>>,
    /// Durable managed-session store, used to **pull-seed** the fleet at construction
    /// (and re-sync on ⟳ Refresh) independent of the engine's one-shot rehydration
    /// broadcast, which this panel — built lazily by the dock — would otherwise miss.
    /// `None` for test/static views.
    store: Option<Arc<dyn ManagedSessionStore>>,
    /// Per-session count of files the agent has written, polled from the change
    /// ledger so a tile can show "there is something to review here" without
    /// opening anything. Absent sessions have written nothing.
    changed: HashMap<SessionId, u32>,
    /// Reads [`changed`](Self::changed); `None` for test/static views.
    changes: Option<Arc<dyn SessionChangeStore>>,
    /// Holds the bus-subscription task alive for the view's lifetime (dropping it
    /// cancels the subscription). `None` for views built without a bus.
    _subscription: Option<Task<()>>,
    /// Holds the ledger poll alive for the view's lifetime.
    _changed_poll: Option<Task<()>>,
}

impl GridHome {
    /// Live view: starts empty and mirrors the engine fleet by folding every
    /// `EngineEvent` from the bus into the read-model, re-rendering on each.
    pub fn new(
        rx: broadcast::Receiver<EngineEvent>,
        focus: Option<Entity<ProjectSpace>>,
        commands: Option<UnboundedSender<Command>>,
        store: Option<Arc<dyn ManagedSessionStore>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.spawn(async move |weak, cx| {
            let mut rx = rx;
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let applied = weak.update(cx, |grid, cx| {
                            grid.model.apply_event(&event);
                            cx.notify();
                        });
                        if applied.is_err() {
                            break; // the view was dropped
                        }
                    }
                    // Drop-oldest: a lagging UI skips missed events and keeps going.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // Re-render when the active space changes so the fleet re-scopes to it.
        if let Some(focus) = focus.as_ref() {
            cx.observe(focus, |_grid, _focus, cx| cx.notify()).detach();
        }

        // Re-render when session metadata (custom name/color) changes, so a rename or
        // recolor in the focus view reflects on the tiles live.
        let meta_cache = cx.try_global::<ShellDeps>().map(|d| d.session_meta.clone());
        if let Some(cache) = meta_cache.as_ref() {
            cx.observe(cache, |_grid, _cache, cx| cx.notify()).detach();
        }

        // Pull-seed the fleet from the durable store so it shows immediately, without
        // waiting to catch the engine's one-shot rehydration broadcast (this panel is
        // built lazily by the dock and would miss it). Live bus deltas refine it after.
        let mut model = FleetModel::default();
        if let Some(store) = store.as_ref() {
            model.seed_missing(&store.all_managed().unwrap_or_default());
            // Records persisted before title storage (or named while the app was
            // closed) still know their name — their transcript has it.
            model.backfill_titles(crate::transcript::latest_title);
        }

        // The ledger is written by the hook server on another runtime, so there is no
        // event to fold — poll it, at the same cadence as the other status polls.
        let changes = cx.try_global::<ShellDeps>().map(|d| d.changes.clone());
        let changed_poll = changes.as_ref().map(|_| {
            cx.spawn(async move |weak, cx| loop {
                let alive = weak.update(cx, |grid: &mut Self, cx| grid.refresh_changed(cx));
                if alive.is_err() {
                    break; // the view was dropped
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(2))
                    .await;
            })
        });

        Self {
            model,
            focus,
            commands,
            new_session_phase: Phase::Plan,
            new_session_agent: AgentKind::ClaudeCode,
            sort_mode: SortMode::default(),
            show_hidden: false,
            focus_handle: cx.focus_handle(),
            meta_cache,
            store,
            changed: HashMap::new(),
            changes,
            _subscription: Some(subscription),
            _changed_poll: changed_poll,
        }
    }

    /// Re-read the per-session changed-file counts, re-rendering only when a count
    /// actually moved (the grid repaints many tiles; a no-op tick shouldn't).
    fn refresh_changed(&mut self, cx: &mut Context<Self>) {
        let Some(changes) = &self.changes else {
            return;
        };
        let counts: HashMap<SessionId, u32> = match changes.touched_counts() {
            Ok(rows) => rows.into_iter().collect(),
            Err(err) => {
                tracing::warn!(error = %err, "reading changed-file counts failed");
                return;
            }
        };
        if counts != self.changed {
            self.changed = counts;
            cx.notify();
        }
    }

    /// Send an adoption toggle for `id` to the engine (no-op for static views).
    /// The resulting `SessionUpserted` flows back through the bus, updating both
    /// this grid and the control gate.
    fn toggle_adoption(&self, id: SessionId) {
        if let Some(tx) = &self.commands {
            let _ = tx.send(Command::ToggleAdoption { session: id });
        }
    }

    /// Pause/resume a session (operator safety halt). A paused adopted session has
    /// every tool denied by the gate until resumed.
    fn toggle_pause(&self, id: SessionId) {
        if let Some(tx) = &self.commands {
            let _ = tx.send(Command::TogglePause { session: id });
        }
    }

    /// Soft-hide / unhide a session — mask a "not relevant anymore" tile from the
    /// default view (recoverable via the header "show hidden" toggle). The resulting
    /// `SessionUpserted` flows back through the bus and re-renders the grid.
    fn set_hidden(&self, id: SessionId, hidden: bool) {
        if let Some(tx) = &self.commands {
            let _ = tx.send(Command::SetHidden {
                session: id,
                hidden,
            });
        }
    }

    /// Open a session in focus mode: re-root the rails onto its path (FR8) *and*
    /// open/activate its monitor tab in the center. No-op for the focus/center parts
    /// when this view is test/static (no shared state).
    fn open_session(&mut self, session: Session, cx: &mut Context<Self>) {
        // Rails follow the focused session.
        if let Some(focus) = self.focus.clone() {
            let id = session.id.clone();
            let path = session.attached_path.clone();
            focus.update(cx, |focus, cx| {
                focus.focus(id, path.as_deref());
                // Notify the space's observers (file tree + terminal) so they re-root.
                cx.notify();
            });
        }
        // Seed the editability gate with this session's phase so open editors
        // lock (read-only) while it's implementing and unlock otherwise.
        if let Some(gate) = cx.try_global::<ShellDeps>().map(|d| d.edit_gate.clone()) {
            let id = session.id.clone();
            let path = session.attached_path.clone();
            let phase = session.phase;
            gate.update(cx, |g, cx| {
                g.set_focus(id, path.as_deref(), phase);
                cx.notify();
            });
        }
        // Zoom into the session as a center tab (focus mode).
        if let Some(center) = cx.try_global::<ShellDeps>().map(|d| d.center.clone()) {
            center.update(cx, |_c, cx| cx.emit(OpenRequest::Session(session)));
        }
        cx.notify();
    }

    /// The active space's root, or `None` for the overview (whole fleet). Drives the
    /// fleet scoping: a space shows only its own sessions.
    fn active_root(&self, cx: &App) -> Option<PathBuf> {
        let ps = self.focus.as_ref()?.read(cx);
        ps.active().is_some().then(|| ps.root())
    }
}

impl Focusable for GridHome {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for GridHome {}

impl Panel for GridHome {
    fn panel_name(&self) -> &'static str {
        "GridHome"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("Sessions")
    }

    /// The center fleet view is the home surface — not closable.
    fn closable(&self, _cx: &App) -> bool {
        false
    }
}

impl Render for GridHome {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // Scope the fleet to the active space (overview = whole fleet).
        let active_root = self.active_root(_cx);
        let ordered = self
            .model
            .visible(active_root.as_deref(), self.sort_mode, self.show_hidden);
        let needs = ordered
            .iter()
            .filter(|s| s.status.needs_attention())
            .count();
        let total = ordered.len();
        let hidden_count = self.model.hidden_count(active_root.as_deref());

        // Resolve each visible session's custom name/color from the shared cache,
        // loading each session's space once (in-memory thereafter — no per-tile disk
        // read). Empty for test/static views with no shell global.
        let metas: std::collections::HashMap<SessionId, SessionMeta> = match self.meta_cache.clone()
        {
            Some(cache) => cache.update(_cx, |c, _| {
                ordered
                    .iter()
                    .map(|s| {
                        if let Some(root) = s
                            .attached_path
                            .as_deref()
                            .map(super::project_space::expand_home)
                        {
                            c.ensure_space(&root);
                        }
                        (s.id.clone(), c.get(&s.id))
                    })
                    .collect()
            }),
            None => std::collections::HashMap::new(),
        };

        // Which agent backend each managed session runs (for the tile badge). Read from
        // the store; sessions absent from it (external/observed) default to Claude.
        let agents: std::collections::HashMap<SessionId, AgentKind> = self
            .store
            .as_ref()
            .and_then(|s| s.all_managed().ok())
            .map(|rows| rows.into_iter().map(|m| (m.id, m.agent)).collect())
            .unwrap_or_default();

        // Capture session + adopted/paused per tile so click handlers are 'static.
        let tiles: Vec<_> = ordered
            .into_iter()
            .map(|s| {
                let meta = metas.get(&s.id).cloned().unwrap_or_default();
                let attention = self.model.attention(s);
                let agent = agents.get(&s.id).copied().unwrap_or_default();
                let changed = self.changed.get(&s.id).copied().unwrap_or(0);
                (
                    s.clone(),
                    s.adopted,
                    s.paused,
                    session_tile(s, &meta, attention, agent, changed),
                )
            })
            .collect();

        // Starting-phase toggle beside the button. Both phases launch CC in `auto`;
        // what differs is our PDP (Plan denies project writes) and the aim in the
        // system prompt (Plan asks for a plan via `present_plan`). Default Plan (safe).
        let chosen = self.new_session_phase;
        let seg = |phase: Phase, text: &'static str, id: &'static str, cx: &mut Context<Self>| {
            let active = chosen == phase;
            let mut s = div()
                .id(id)
                .px_2()
                .py(px(3.))
                .cursor_pointer()
                .rounded(theme::radius_sm())
                .text_size(theme::text_xs());
            s = if active {
                s.bg(theme::tint(theme::accent(), 0.18))
                    .text_color(theme::accent())
            } else {
                s.text_color(theme::text_muted())
            };
            s.child(text)
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    this.new_session_phase = phase;
                    cx.notify();
                }))
        };
        let mode_toggle = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.))
            .p(px(2.))
            .rounded(theme::radius_sm())
            .bg(theme::surface_raised())
            .child(seg(Phase::Plan, "Plan", "new-phase-plan", _cx))
            .child(seg(Phase::AutoImplement, "Auto", "new-phase-auto", _cx));

        // Backend toggle beside the phase toggle: which agent CLI the new session runs
        // (`claude` / `agy`). Default Claude Code. The chosen backend rides the launch
        // request and is persisted on the managed record.
        let chosen_agent = self.new_session_agent;
        let agent_seg =
            |agent: AgentKind, text: &'static str, id: &'static str, cx: &mut Context<Self>| {
                let active = chosen_agent == agent;
                let mut s = div()
                    .id(id)
                    .px_2()
                    .py(px(3.))
                    .cursor_pointer()
                    .rounded(theme::radius_sm())
                    .text_size(theme::text_xs());
                s = if active {
                    s.bg(theme::tint(theme::accent(), 0.18))
                        .text_color(theme::accent())
                } else {
                    s.text_color(theme::text_muted())
                };
                s.child(text)
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.new_session_agent = agent;
                        cx.notify();
                    }))
            };
        let agent_toggle = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.))
            .p(px(2.))
            .rounded(theme::radius_sm())
            .bg(theme::surface_raised())
            .child(agent_seg(
                AgentKind::ClaudeCode,
                "Claude",
                "new-agent-claude",
                _cx,
            ))
            .child(agent_seg(
                AgentKind::Antigravity,
                "AGY",
                "new-agent-agy",
                _cx,
            ));

        // "＋ New session" — the primary action: a solid accent button.
        let new_session_btn = div()
            .id("new-session")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(5.))
            .cursor_pointer()
            .px_3()
            .py(px(5.))
            .rounded(theme::radius_sm())
            .bg(theme::accent())
            .text_color(theme::on_accent())
            .text_size(theme::text_sm())
            .font_weight(FontWeight::MEDIUM)
            .hover(|d| d.bg(theme::accent_hover()))
            .child("＋")
            .child("New session")
            .on_click(_cx.listener(|this, _ev, _window, cx| {
                if let Some(center) = cx.try_global::<ShellDeps>().map(|d| d.center.clone()) {
                    // Pin a session id up front so the launched terminal and the grid
                    // tile (discovered from JSONL) are the same managed session.
                    let id = SessionId::new(uuid::Uuid::new_v4().to_string());
                    let phase = this.new_session_phase;
                    let agent = this.new_session_agent;
                    center.update(cx, |_center, cx| {
                        cx.emit(OpenRequest::NewManagedSession { id, phase, agent });
                    });
                }
            }));

        // "⟳ Refresh" — reload the managed fleet from the durable store on demand
        // (the same path run at boot). Useful when the grid looks emptier than the
        // store, or after sessions were launched/managed out-of-band. A subtle
        // secondary button so it doesn't compete with the primary "＋ New session".
        let refresh_btn = div()
            .id("refresh-fleet")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(5.))
            .cursor_pointer()
            .px_3()
            .py(px(5.))
            .rounded(theme::radius_sm())
            .bg(theme::surface_raised())
            .text_color(theme::text_muted())
            .text_size(theme::text_sm())
            .hover(|d| d.text_color(theme::text_primary()))
            .child("⟳")
            .child("Refresh")
            .on_click(_cx.listener(|this, _ev, _window, cx| {
                // Re-pull the grid straight from the store (immediate, local), and ask
                // the engine to rehydrate too (re-sync the supervisor fleet + gate for
                // any session created out-of-band). The pull is what the operator sees.
                if let Some(store) = this.store.as_ref() {
                    this.model
                        .seed_missing(&store.all_managed().unwrap_or_default());
                    cx.notify();
                }
                if let Some(tx) = &this.commands {
                    let _ = tx.send(Command::RehydrateFleet);
                }
            }));

        // Sort toggle: "Needs you" (triage rank) vs "Recent" (last activity desc).
        let sort_now = self.sort_mode;
        let sort_seg =
            |mode: SortMode, text: &'static str, id: &'static str, cx: &mut Context<Self>| {
                let active = sort_now == mode;
                let mut s = div()
                    .id(id)
                    .px_2()
                    .py(px(3.))
                    .cursor_pointer()
                    .rounded(theme::radius_sm())
                    .text_size(theme::text_xs());
                s = if active {
                    s.bg(theme::tint(theme::accent(), 0.18))
                        .text_color(theme::accent())
                } else {
                    s.text_color(theme::text_muted())
                };
                s.child(text)
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.sort_mode = mode;
                        cx.notify();
                    }))
            };
        let sort_toggle = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.))
            .p(px(2.))
            .rounded(theme::radius_sm())
            .bg(theme::surface_raised())
            .child(sort_seg(SortMode::Triage, "Needs you", "sort-triage", _cx))
            .child(sort_seg(SortMode::Recent, "Recent", "sort-recent", _cx));

        // "hidden (N)" — reveal/conceal soft-hidden tiles. Only present when the active
        // scope actually has hidden sessions, so it stays out of the way otherwise.
        let show_hidden = self.show_hidden;
        let hidden_toggle = (hidden_count > 0).then(|| {
            let glyph = if show_hidden { "⊙" } else { "⊘" };
            let mut b = div()
                .id("toggle-hidden")
                .flex()
                .flex_row()
                .items_center()
                .gap(px(5.))
                .cursor_pointer()
                .px_3()
                .py(px(5.))
                .rounded(theme::radius_sm())
                .text_size(theme::text_sm());
            b = if show_hidden {
                b.bg(theme::tint(theme::accent(), 0.18))
                    .text_color(theme::accent())
            } else {
                b.bg(theme::surface_raised())
                    .text_color(theme::text_muted())
                    .hover(|d| d.text_color(theme::text_primary()))
            };
            b.child(format!("{glyph} hidden ({hidden_count})"))
                .on_click(_cx.listener(|this, _ev, _window, cx| {
                    this.show_hidden = !this.show_hidden;
                    cx.notify();
                }))
        });

        let new_session = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .child(sort_toggle)
            .children(hidden_toggle)
            .child(refresh_btn)
            .child(mode_toggle)
            .child(agent_toggle)
            .child(new_session_btn);

        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::surface_base())
            .text_color(theme::text_primary())
            .child(header(total, needs, new_session))
            .child(
                div()
                    .id("fleet-scroll")
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .content_start()
                    .gap(px(14.))
                    .p_4()
                    .size_full()
                    .overflow_y_scroll()
                    .children(tiles.into_iter().enumerate().map(
                        |(i, (session, adopted, paused, tile))| {
                            let adopt_id = session.id.clone();
                            let open = session.clone();
                            let chip_color = if adopted {
                                theme::status_color(SessionStatus::Running)
                            } else {
                                theme::text_muted()
                            };
                            // Pause chip only for adopted sessions (the gate halts only those).
                            let pause_chip = adopted.then(|| {
                                let pause_id = session.id.clone();
                                let pc = if paused {
                                    theme::status_color(SessionStatus::Paused)
                                } else {
                                    theme::text_muted()
                                };
                                div()
                                    .id(("pause", i))
                                    .px(px(7.))
                                    .py(px(2.))
                                    .rounded(theme::radius_sm())
                                    .cursor_pointer()
                                    .text_size(theme::text_2xs())
                                    .font_weight(FontWeight::MEDIUM)
                                    .bg(theme::tint(pc, 0.18))
                                    .text_color(pc)
                                    .hover(|d| d.bg(theme::tint(pc, 0.28)))
                                    .child(if paused { "‖ paused" } else { "‖ pause" })
                                    .on_click(_cx.listener(move |this, _ev, _window, cx| {
                                        cx.stop_propagation();
                                        this.toggle_pause(pause_id.clone());
                                        cx.notify();
                                    }))
                            });
                            // Soft-hide chip: mask a "not relevant anymore" tile from the
                            // default view (or restore it when "show hidden" is on).
                            let hidden = session.hidden;
                            let hide_id = session.id.clone();
                            let hide_chip = div()
                                .id(("hide", i))
                                .px(px(7.))
                                .py(px(2.))
                                .rounded(theme::radius_sm())
                                .cursor_pointer()
                                .text_size(theme::text_2xs())
                                .font_weight(FontWeight::MEDIUM)
                                .bg(theme::tint(theme::text_muted(), 0.18))
                                .text_color(theme::text_muted())
                                .hover(|d| d.bg(theme::tint(theme::text_muted(), 0.28)))
                                .child(if hidden { "⊙ show" } else { "⊘ hide" })
                                .on_click(_cx.listener(move |this, _ev, _window, cx| {
                                    cx.stop_propagation();
                                    this.set_hidden(hide_id.clone(), !hidden);
                                    cx.notify();
                                }));
                            div()
                                .id(("session-tile", i))
                                .relative()
                                .cursor_pointer()
                                .on_click(_cx.listener(move |this, _ev, _window, cx| {
                                    this.open_session(open.clone(), cx);
                                }))
                                .child(tile)
                                // Top-right control chips (pause + adopt). Each click acts
                                // on the session and stops propagation so it doesn't focus.
                                .child(
                                    div()
                                        .id(("chips", i))
                                        .absolute()
                                        .top(px(8.))
                                        .right(px(8.))
                                        .flex()
                                        .flex_col()
                                        .items_end()
                                        .gap(px(5.))
                                        .child(hide_chip)
                                        .children(pause_chip)
                                        .child(
                                            div()
                                                .id(("adopt", i))
                                                .px(px(7.))
                                                .py(px(2.))
                                                .rounded(theme::radius_sm())
                                                .cursor_pointer()
                                                .text_size(theme::text_2xs())
                                                .font_weight(FontWeight::MEDIUM)
                                                .bg(theme::tint(chip_color, 0.18))
                                                .text_color(chip_color)
                                                .hover(|d| d.bg(theme::tint(chip_color, 0.28)))
                                                .child(if adopted {
                                                    "● governed"
                                                } else {
                                                    "○ adopt"
                                                })
                                                .on_click(_cx.listener(
                                                    move |this, _ev, _window, cx| {
                                                        cx.stop_propagation();
                                                        this.toggle_adoption(adopt_id.clone());
                                                        cx.notify();
                                                    },
                                                )),
                                        ),
                                )
                        },
                    )),
            )
    }
}

fn header(total: usize, needs: usize, action: impl IntoElement) -> impl IntoElement {
    let count_label = if total == 1 {
        "1 session".to_string()
    } else {
        format!("{total} sessions")
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .w_full()
        .px_4()
        .py(px(11.))
        .bg(theme::surface_sunken())
        .border_b_1()
        .border_color(theme::border_subtle())
        .child(
            div()
                .flex()
                .flex_row()
                .items_baseline()
                .gap(px(9.))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .text_size(theme::text_sm())
                                .text_color(theme::accent())
                                .child("☾"),
                        )
                        .child(
                            div()
                                .text_size(theme::text_lg())
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::text_primary())
                                .child("MoonlightCode"),
                        ),
                )
                .child(
                    div()
                        .text_color(theme::text_muted())
                        .text_size(theme::text_sm())
                        .child(count_label),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(needs_badge(needs))
                .child(action),
        )
}

/// The triage pulse: amber "N need you" when work is waiting, a calm green
/// "all clear" when nothing does.
fn needs_badge(needs: usize) -> impl IntoElement {
    let (color, label) = if needs == 0 {
        (
            theme::status_color(SessionStatus::Done),
            "✓ all clear".to_string(),
        )
    } else {
        (
            theme::status_color(SessionStatus::WaitingInput),
            format!("◐ {needs} need you"),
        )
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_3()
        .py(px(5.))
        .rounded(theme::radius_sm())
        .bg(theme::tint(color, 0.16))
        .text_color(color)
        .text_size(theme::text_sm())
        .font_weight(FontWeight::MEDIUM)
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_event_updates_known_session_status() {
        let mut model = FleetModel::from_sessions(crate::seed::mock_sessions());
        let id = model.sessions[0].id.clone();

        model.apply_event(&EngineEvent::SessionStateChanged {
            session: id.clone(),
            status: SessionStatus::Errored,
        });

        assert_eq!(model.session(&id).unwrap().status, SessionStatus::Errored);
    }

    #[test]
    fn apply_event_for_unknown_session_is_ignored() {
        let mut model = FleetModel::from_sessions(crate::seed::mock_sessions());
        let before = model.sessions.len();

        model.apply_event(&EngineEvent::SessionStateChanged {
            session: SessionId::new("does-not-exist"),
            status: SessionStatus::Running,
        });

        assert_eq!(model.sessions.len(), before);
    }

    #[test]
    fn visible_scopes_to_the_active_space_root() {
        let base = crate::seed::mock_sessions().remove(0);
        let mut api = base.clone();
        api.id = SessionId::new("api-1");
        api.attached_path = Some("/tmp/api".to_string());
        let mut web = base.clone();
        web.id = SessionId::new("web-1");
        web.attached_path = Some("/tmp/web".to_string());
        let mut rootless = base.clone();
        rootless.id = SessionId::new("none-1");
        rootless.attached_path = None;

        let model = FleetModel::from_sessions(vec![api, web, rootless]);

        // Overview (None) → the whole fleet.
        assert_eq!(model.visible(None, SortMode::Triage, false).len(), 3);

        // Scoped to /tmp/api → only the session attached there.
        let scoped = model.visible(Some(Path::new("/tmp/api")), SortMode::Triage, false);
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].id, SessionId::new("api-1"));
    }

    #[test]
    fn hidden_sessions_are_filtered_unless_shown() {
        let base = crate::seed::mock_sessions().remove(0);
        let mut visible = base.clone();
        visible.id = SessionId::new("visible");
        visible.hidden = false;
        let mut masked = base.clone();
        masked.id = SessionId::new("masked");
        masked.hidden = true;

        let model = FleetModel::from_sessions(vec![visible, masked]);

        // Default view hides the masked tile…
        let shown = model.visible(None, SortMode::Triage, false);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].id, SessionId::new("visible"));
        // …and the count reflects it, so the "show hidden (N)" toggle can appear.
        assert_eq!(model.hidden_count(None), 1);

        // "show hidden" reveals it again.
        let all = model.visible(None, SortMode::Triage, true);
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn recent_sort_orders_by_last_activity_desc() {
        use moonlight_domain::ids::Timestamp;
        let base = crate::seed::mock_sessions().remove(0);
        let mk = |id: &str, at: i64| {
            let mut s = base.clone();
            s.id = SessionId::new(id);
            s.last_activity = Timestamp::from_millis(at);
            s
        };
        // Insert out of recency order to prove the sort, not arrival, drives it.
        let model = FleetModel::from_sessions(vec![mk("mid", 200), mk("new", 300), mk("old", 100)]);

        let ids: Vec<_> = model
            .visible(None, SortMode::Recent, false)
            .iter()
            .map(|s| s.id.clone())
            .collect();
        assert_eq!(
            ids,
            vec![
                SessionId::new("new"),
                SessionId::new("mid"),
                SessionId::new("old")
            ],
            "newest activity first"
        );
    }

    #[test]
    fn triage_ordered_floats_needs_you_to_the_top() {
        let model = FleetModel::from_sessions(crate::seed::mock_sessions());
        let ranks: Vec<u8> = model
            .triage_ordered()
            .iter()
            .map(|s| s.status.triage_rank())
            .collect();
        // Ranks must be non-decreasing, and the first tile must need attention.
        assert!(
            ranks.windows(2).all(|w| w[0] <= w[1]),
            "not sorted: {ranks:?}"
        );
        assert!(model.triage_ordered()[0].status.needs_attention());
    }

    #[test]
    fn upsert_adds_a_new_tile_then_replaces_it() {
        let mut model = FleetModel::from_sessions(crate::seed::mock_sessions());
        let before = model.sessions.len();
        let id = SessionId::new("new-1");

        let mut session = model.sessions[0].clone();
        session.id = id.clone();
        session.title = Some("Fresh session".to_string());
        session.status = SessionStatus::Running;

        // First upsert adds the tile.
        model.apply_event(&EngineEvent::SessionUpserted {
            session: session.clone(),
        });
        assert_eq!(model.sessions.len(), before + 1);

        // Second upsert (same id) replaces in place — no duplicate.
        session.status = SessionStatus::Done;
        model.apply_event(&EngineEvent::SessionUpserted { session });
        assert_eq!(model.sessions.len(), before + 1);
        assert_eq!(model.session(&id).unwrap().status, SessionStatus::Done);
    }

    #[test]
    fn session_alert_overlays_status_and_clears_on_resume() {
        let mut model = FleetModel::from_sessions(crate::seed::mock_sessions());
        let id = model.sessions[0].id.clone();
        // Park the session Idle so status implies no attention of its own.
        model.apply_event(&EngineEvent::SessionStateChanged {
            session: id.clone(),
            status: SessionStatus::Idle,
        });
        assert_eq!(model.attention(model.session(&id).unwrap()), None);

        // The agent self-reports Stuck → ⚠ overlay, even though status is Idle.
        model.apply_event(&EngineEvent::SessionAlert {
            session: id.clone(),
            alert: Some(AttentionKind::Stuck),
        });
        assert_eq!(
            model.attention(model.session(&id).unwrap()),
            Some(AttentionKind::Stuck)
        );

        // Transitioning into Running (recovered / restarted) clears the overlay…
        model.apply_event(&EngineEvent::SessionStateChanged {
            session: id.clone(),
            status: SessionStatus::Running,
        });
        assert_eq!(model.attention(model.session(&id).unwrap()), None);
    }

    #[test]
    fn a_running_session_reporting_stuck_does_not_flicker_away() {
        // Edge-triggered clear: a Running→Running repeat must NOT wipe a just-raised
        // Stuck (the agent reports mid-turn, still Running).
        let mut model = FleetModel::from_sessions(crate::seed::mock_sessions());
        let id = model.sessions[0].id.clone();
        model.apply_event(&EngineEvent::SessionStateChanged {
            session: id.clone(),
            status: SessionStatus::Running,
        });
        model.apply_event(&EngineEvent::SessionAlert {
            session: id.clone(),
            alert: Some(AttentionKind::Stuck),
        });
        // Another Running poll arrives — not a fresh transition, so the alert survives.
        model.apply_event(&EngineEvent::SessionStateChanged {
            session: id.clone(),
            status: SessionStatus::Running,
        });
        assert_eq!(
            model.attention(model.session(&id).unwrap()),
            Some(AttentionKind::Stuck)
        );
        // An explicit clear (or SessionRemoved) takes it down.
        model.apply_event(&EngineEvent::SessionAlert {
            session: id.clone(),
            alert: None,
        });
        assert_eq!(model.attention(model.session(&id).unwrap()), None);
    }

    #[test]
    fn errored_status_derives_a_warning_without_an_explicit_alert() {
        let mut model = FleetModel::from_sessions(crate::seed::mock_sessions());
        let id = model.sessions[0].id.clone();
        model.apply_event(&EngineEvent::SessionStateChanged {
            session: id.clone(),
            status: SessionStatus::Errored,
        });
        assert_eq!(
            model.attention(model.session(&id).unwrap()),
            Some(AttentionKind::Errored)
        );
    }

    #[test]
    fn session_removed_drops_the_tile() {
        let mut model = FleetModel::from_sessions(crate::seed::mock_sessions());
        let id = model.sessions[0].id.clone();
        let before = model.sessions.len();

        model.apply_event(&EngineEvent::SessionRemoved {
            session: id.clone(),
        });

        assert_eq!(model.sessions.len(), before - 1);
        assert!(model.session(&id).is_none());
    }

    #[test]
    fn backfill_titles_fills_only_missing_ones() {
        use moonlight_domain::ids::Timestamp;
        use moonlight_domain::session::Mode;

        let untitled = ManagedSession {
            id: SessionId::new("untitled"),
            agent: moonlight_domain::AgentKind::ClaudeCode,
            conversation_id: None,
            trust_tier: moonlight_domain::trust::TrustTier::Observed,
            root: Some("/repo".to_string()),
            title: None,
            mode: Mode::Auto,
            phase: Phase::AutoImplement,
            adopted: true,
            paused: false,
            phase_pinned: false,
            hidden: false,
            created_at: Timestamp::from_millis(0),
            last_seen: Timestamp::from_millis(0),
        };
        let mut titled = untitled.clone();
        titled.id = SessionId::new("titled");
        titled.title = Some("Kept name".to_string());

        let mut model = FleetModel::default();
        model.seed_missing(&[untitled, titled]);
        model.backfill_titles(|_| Some("From transcript".to_string()));

        assert_eq!(
            model
                .session(&SessionId::new("untitled"))
                .unwrap()
                .title
                .as_deref(),
            Some("From transcript"),
            "missing title backfilled from the transcript lookup"
        );
        assert_eq!(
            model
                .session(&SessionId::new("titled"))
                .unwrap()
                .title
                .as_deref(),
            Some("Kept name"),
            "an existing title is never overwritten"
        );
    }

    #[test]
    fn seed_missing_pull_seeds_without_clobbering_live_sessions() {
        use moonlight_domain::ids::Timestamp;
        use moonlight_domain::session::Mode;

        let rec = |id: &str| ManagedSession {
            id: SessionId::new(id),
            agent: moonlight_domain::AgentKind::ClaudeCode,
            conversation_id: None,
            trust_tier: moonlight_domain::trust::TrustTier::Observed,
            root: Some("/repo".to_string()),
            title: Some("Seeded title".to_string()),
            mode: Mode::Auto,
            phase: Phase::AutoImplement,
            adopted: true,
            paused: false,
            phase_pinned: false,
            hidden: false,
            created_at: Timestamp::from_millis(0),
            last_seen: Timestamp::from_millis(0),
        };

        // A live session already known (Running, e.g. from detection) before the pull.
        let mut live = crate::seed::mock_sessions().remove(0);
        live.id = SessionId::new("a");
        live.status = SessionStatus::Running;
        let mut model = FleetModel::from_sessions(vec![live]);

        model.seed_missing(&[rec("a"), rec("b")]);

        // "a" already present → kept Running (not downgraded to the record's Idle).
        assert_eq!(model.sessions.len(), 2);
        assert_eq!(
            model.session(&SessionId::new("a")).unwrap().status,
            SessionStatus::Running
        );
        // "b" was missing → added at rest from the record.
        let b = model.session(&SessionId::new("b")).unwrap();
        assert_eq!(b.status, SessionStatus::Idle);
        assert_eq!(b.phase, Phase::AutoImplement);
        assert!(b.adopted);
    }
}
