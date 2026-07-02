//! The IDE-classic dockable workspace shell.
//!
//! A JetBrains-style `DockArea`: the center holds the fleet [`GridHome`], the left
//! rail stacks the [`FileTreePanel`] project explorer over the [`StructurePanel`]
//! outline (structure beside the code tree, JetBrains-style), the bottom rail the
//! [`TerminalPanel`]. Rails are collapsible (calm-by-default reveal-on-demand) and
//! the layout persists across restarts.
//!
//! The center tabs are wrapped in a split (not set directly as a bare tab panel)
//! so the center tab panel has a parent stack — gpui-component locks a parentless
//! tab panel, which would make opened file/session tabs neither closable nor
//! draggable. With the parent stack they can be closed and reordered in the tab bar.
//!
//! Each dockable surface is a "plugin" (`gpui_component::dock::Panel`). They are
//! registered with the dock so a persisted layout can be rehydrated; the live
//! dependencies they need (the engine [`EventBus`] and the shared
//! [`ProjectSpace`]) are stashed in the [`ShellDeps`] global so both the default
//! build and a reload can construct them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use gpui::prelude::*;
use gpui::{
    div, px, App, Context, CursorStyle, Edges, Entity, Focusable, FontWeight, Global, KeyBinding,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions, Pixels,
    PromptLevel, SharedString, WeakEntity, Window,
};
use gpui_component::dock::{
    register_panel, DockArea, DockAreaState, DockItem, DockPlacement, PanelInfo, PanelState,
    PanelView,
};
use gpui_component::notification::Notification;
use gpui_component::{Root, WindowExt};

use moonlight_domain::ids::{SessionId, Timestamp};
use moonlight_domain::phase::Phase;
use moonlight_domain::ports::store::{ManagedSession, ManagedSessionStore};
use moonlight_domain::session::{AttentionKind, SessionStatus};
use moonlight_domain::trust::TrustTier;
use moonlight_engine::{Command, EngineEvent, EventBus};
use tokio::sync::broadcast;
use tokio::sync::mpsc::UnboundedSender;

use super::active_context::{ActiveContext, Eol};
use super::active_editor::ActiveEditor;
use super::center_requests::{CenterRequests, OpenRequest};
use super::chrome_requests::{ChromeRequest, ChromeRequests};
use super::edit_gate::EditGate;
use super::editor_commands::{EditorCommand, EditorCommands};
use super::grid_home::GridHome;
use super::mcp_host::McpHostHandle;
use super::notifications::{NotificationKind, Notifications};
use super::obs_store::ObsStore;
use super::open_tabs::{self, OpenTabsState, SpaceTabs};
use super::panels::activity_rail::{activity_rail, RailSnapshot};
use super::panels::code_editor::{CodeEditorPanel, FormatDocument, SaveFile};
use super::panels::code_review::CodeReviewPanel;
use super::panels::db_console::DbConsolePanel;
use super::panels::db_grid::DbGridPanel;
use super::panels::db_observer::DbObserverPanel;
use super::panels::db_source::DataSource;
use super::panels::file_tree::FileTreePanel;
use super::panels::plan_review::PlanReviewPanel;
use super::panels::session_monitor::{attach_command, SessionMonitor};
use super::panels::spaces::{space_tab_bar, SpaceTab};
use super::panels::status_bar::{
    self, LeftZone, NotifRow, ObsView, QuotaView, StatusSnapshot,
};
use super::panels::structure::StructurePanel;
use super::panels::terminal::TerminalPanel;
use super::panels::toolbar;
use super::project_space::{expand_home, ProjectSpace, SpaceId};
use super::session_io::SessionIo;
use super::session_meta::SessionMetaCache;
use super::space_sessions::SpaceSession;
use super::theme;
use crate::views::run_config::{RunConfig, RunKind};
use gpui_component::input::{Input, InputState};

const DOCK_ID: &str = "moonlight-main";
/// Bump when the default layout shape changes incompatibly; a persisted layout
/// with a different version is discarded and rebuilt. (v2: added the right-rail
/// Structure panel. v3: Structure moved into the left rail under the file tree;
/// center tabs wrapped in a split so they are closable/draggable. v4: added the
/// Spaces rail beside the file tree/structure column in the left dock. v5: the
/// Spaces rail moved out of the dock into a top tab bar — left dock is back to
/// file tree + structure.)
const DOCK_VERSION: usize = 6;

/// Live dependencies the dockable panels need to (re)build themselves. Stored as
/// a GPUI global so the panel registry closures can reach them on layout reload.
#[derive(Clone)]
pub struct ShellDeps {
    pub focus: Entity<ProjectSpace>,
    pub bus: EventBus,
    /// Operator intents (adoption, …) sent to the engine supervisor.
    pub commands: UnboundedSender<Command>,
    /// Cross-panel channel: rail panels emit `OpenRequest`s here; the workspace
    /// opens the matching center tab.
    pub center: Entity<CenterRequests>,
    /// Cross-panel chrome channel: tool-window headers emit `ChromeRequest`s
    /// (hide me) here; the workspace flips the matching dock/tool flag.
    pub chrome: Entity<ChromeRequests>,
    /// Editability gate: whether an open file is read-only (focused session in
    /// `AutoImplement` on that file) or editable + saveable.
    pub edit_gate: Entity<EditGate>,
    /// The frontmost code editor (path/text/handle) — drives the Structure panel.
    pub active_editor: Entity<ActiveEditor>,
    /// The frontmost center tab's context (file caret/metadata or session) — drives
    /// the bottom [`status_bar`](super::panels::status_bar).
    pub active_context: Entity<ActiveContext>,
    /// In-app notification inbox shown behind the status bar's 🔔, folded from the
    /// engine bus by the [`Workspace`].
    pub notifications: Entity<Notifications>,
    /// Status-bar → frontmost-editor command channel (e.g. an EOL click). Every
    /// [`CodeEditorPanel`] subscribes; the active one handles it.
    pub editor_commands: Entity<EditorCommands>,
    /// Claude-observability read-model (per-session model/ctx/time/cost/persona),
    /// polled from `<support>/obs/` and shown in the status bar's right zone.
    pub obs_store: Entity<ObsStore>,
    /// Durable managed-session store. Consulted on layout-restore so a session the
    /// app launched comes back **managed** (re-resumes its embedded terminal), not
    /// as a read-only observed session, and written when a managed session launches.
    pub store: Arc<dyn ManagedSessionStore>,
    /// UI-side session→terminal registry, so a panel that doesn't own the embedded
    /// terminal (the Plan-review tab) can drive the session's CC TUI — e.g. send
    /// Enter to accept a plan's continuation (option 1 / auto). See [`SessionIo`].
    pub session_io: SessionIo,
    /// UI-side registry of outstanding operator approvals (held MCP tool / phase change /
    /// danger-zone command), keyed by session. Folded from the bus by the [`Workspace`]
    /// so a [`SessionMonitor`](super::panels::session_monitor) opened *after* the one-shot
    /// `ApprovalRequested` (e.g. via the bell notification) can rebuild its popup. See
    /// [`Approvals`](super::approvals::Approvals).
    pub approvals: super::approvals::Approvals,
    /// Observable cache of per-session display metadata (custom name + color). The
    /// focus view writes through it; the fleet grid observes it so a rename/recolor
    /// shows on tiles live without a per-render disk read. See [`SessionMetaCache`].
    pub session_meta: Entity<SessionMetaCache>,
    /// Per-session embedded MCP host (Slice 3): `url_for` stands a session's HTTP
    /// MCP endpoint up (cached per id) so the launch arms can append
    /// `--mcp-config`. `None` when the host runtime failed to start — sessions
    /// then launch without MCP rather than not at all.
    pub mcp_host: Option<McpHostHandle>,
    /// The shared run state behind the Run console **and** the MCP run verbs — the
    /// composition root hands the same registry to the `RunVerbExecutor`, so a run
    /// started by either side shows in (and is controllable from) both.
    pub run_registry: crate::run::RunRegistry,
    /// Shared HTTP call history behind the `http_request` verb and the Services view's
    /// HTTP summary (the executor records; the panel polls). Same shared-handle shape
    /// as `run_registry`.
    pub http_history: crate::http::HttpHistory,
    /// Operator's auto-phasing toggle (shared with the MCP actor): when on, the agent's
    /// `request_phase` is auto-approved instead of waiting on the cockpit gate. The
    /// main toolbar flips it; the actor reads it.
    pub auto_phase: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The one language-server pool. Shared so the Structure outline's servers are
    /// the same processes whose `publishDiagnostics` feed the Problems window.
    pub lsp_pool: Arc<crate::lsp::LspPool>,
    /// Shared git command console: IDE-run git ops (checkout, stage, commit)
    /// record here; the Git tool window's Console view replays them.
    pub git_console: super::git_console::GitConsole,
}

impl Global for ShellDeps {}

/// Install the shell dependencies and register the dockable panels. Must run once,
/// before any `Workspace` is constructed.
#[allow(clippy::too_many_arguments)]
pub fn init_shell(
    cx: &mut App,
    focus: Entity<ProjectSpace>,
    bus: EventBus,
    commands: UnboundedSender<Command>,
    center: Entity<CenterRequests>,
    edit_gate: Entity<EditGate>,
    active_editor: Entity<ActiveEditor>,
    active_context: Entity<ActiveContext>,
    notifications: Entity<Notifications>,
    editor_commands: Entity<EditorCommands>,
    obs_store: Entity<ObsStore>,
    store: Arc<dyn ManagedSessionStore>,
    mcp_host: Option<McpHostHandle>,
    run_registry: crate::run::RunRegistry,
    http_history: crate::http::HttpHistory,
    auto_phase: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let session_meta = cx.new(|_| SessionMetaCache::default());
    let chrome = cx.new(|_| ChromeRequests);
    let lsp_pool = Arc::new(crate::lsp::LspPool::new());
    let git_console = super::git_console::GitConsole::default();
    cx.set_global(ShellDeps {
        focus,
        bus,
        commands,
        center,
        chrome,
        edit_gate,
        active_editor,
        active_context,
        notifications,
        editor_commands,
        obs_store,
        store,
        session_io: SessionIo::default(),
        approvals: super::approvals::Approvals::default(),
        session_meta,
        mcp_host,
        run_registry,
        http_history,
        auto_phase,
        lsp_pool,
        git_console,
    });

    // ⌘S saves / ⌘⇧I formats the focused code-editor tab (the panel's `key_context`).
    cx.bind_keys(vec![
        KeyBinding::new("cmd-s", SaveFile, Some("CodeEditor")),
        KeyBinding::new("cmd-shift-i", FormatDocument, Some("CodeEditor")),
    ]);
    // Tab/Shift+Tab inside a focused terminal go to the child (shell completion,
    // Claude Code autofill + mode cycle), overriding Root's focus traversal.
    super::panels::terminal::init_keybindings(cx);
    // Cmd+F / Esc inside the Run window (search open/close).
    super::panels::run_console::init_keybindings(cx);

    register_panel(cx, "GridHome", |_, _, _, _window, cx| {
        let deps = cx.global::<ShellDeps>().clone();
        let view = cx.new(|cx| {
            GridHome::new(
                deps.bus.subscribe(),
                Some(deps.focus.clone()),
                Some(deps.commands.clone()),
                Some(deps.store.clone()),
                cx,
            )
        });
        Box::new(view) as Box<dyn PanelView>
    });
    register_panel(cx, "FileTree", |_, _, _, _window, cx| {
        let deps = cx.global::<ShellDeps>().clone();
        let view = cx.new(|cx| FileTreePanel::new(Some(deps.focus.clone()), cx));
        Box::new(view) as Box<dyn PanelView>
    });
    register_panel(cx, "Terminal", |_, _, _, _window, cx| {
        let deps = cx.global::<ShellDeps>().clone();
        let view = cx.new(|cx| TerminalPanel::new(Some(deps.focus.clone()), cx));
        Box::new(view) as Box<dyn PanelView>
    });
    // Dynamic center tabs — rehydrated from the path/session stashed in `dump()`.
    register_panel(cx, "CodeEditor", |_, _, info, window, cx| {
        let path = panel_info_str(info, "path").map(PathBuf::from);
        let view = cx.new(|cx| CodeEditorPanel::restore(path, window, cx));
        Box::new(view) as Box<dyn PanelView>
    });
    register_panel(cx, "SessionMonitor", |_, _, info, _window, cx| {
        let deps = cx.global::<ShellDeps>().clone();
        let id = SessionId::new(panel_info_str(info, "session").unwrap_or_default());
        let info_root = panel_info_str(info, "root").map(PathBuf::from);
        // A session MoonlightCode launched is recorded in the managed store. On
        // restore it must come back **managed** — re-resume its embedded terminal
        // (`claude --resume <id>`) — not as a read-only observed session (which is
        // all the layout's `root`-only rehydration could otherwise tell us).
        let managed = deps.store.managed(&id).ok().flatten();
        let view = cx.new(|cx| match managed {
            Some(rec) => {
                let root = rec
                    .root
                    .map(PathBuf::from)
                    .or(info_root)
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                let mcp = deps.mcp_host.as_ref().and_then(|h| h.url_for(&id));
                let command = attach_command(&id, rec.agent, rec.phase, mcp.as_deref());
                SessionMonitor::new_managed(
                    id,
                    None,
                    root,
                    command,
                    rec.agent,
                    rec.phase,
                    deps.bus.subscribe(),
                    cx,
                )
            }
            // Not managed: rehydrate as before — with a repo, offer resume (gated to
            // idle/done) once status arrives; without one, the read-only transcript.
            None => match info_root {
                Some(root) => {
                    SessionMonitor::new_observed(id, None, root, deps.bus.subscribe(), cx)
                }
                None => SessionMonitor::new(id, None, deps.bus.subscribe(), cx),
            },
        });
        Box::new(view) as Box<dyn PanelView>
    });
    register_panel(cx, "Structure", |_, _, _, _window, cx| {
        let deps = cx.global::<ShellDeps>().clone();
        let view = cx.new(|cx| StructurePanel::new(Some(deps.active_editor.clone()), cx));
        Box::new(view) as Box<dyn PanelView>
    });
    register_panel(cx, "Commit", |_, _, _, _window, cx| {
        let deps = cx.global::<ShellDeps>().clone();
        let view =
            cx.new(|cx| super::panels::commit::CommitPanel::new(Some(deps.focus.clone()), cx));
        Box::new(view) as Box<dyn PanelView>
    });
    register_panel(cx, "DbObserver", |_, _, _info, _window, cx| {
        // The overview is workspace-owned; a restored instance is replaced by the owned
        // entity right after `load_layout`, so this just needs to be a valid placeholder.
        let view = cx.new(DbObserverPanel::new);
        Box::new(view) as Box<dyn PanelView>
    });
    // Center DB tabs (data editor / SQL console). Rebuilt from their dumped source so
    // `load_layout` doesn't error; the center is then reset to home, so these are
    // transient — they reopen from the overview tree, not across a restart.
    register_panel(cx, "DbGrid", |_, _, info, _window, cx| {
        let view = cx.new(|cx| DbGridPanel::restore(info, cx));
        Box::new(view) as Box<dyn PanelView>
    });
    register_panel(cx, "DbConsole", |_, _, info, _window, cx| {
        let view = cx.new(|cx| DbConsolePanel::restore(info, cx));
        Box::new(view) as Box<dyn PanelView>
    });
    register_panel(cx, "Http", |_, _, info, _window, cx| {
        let view = cx.new(|cx| super::panels::http_panel::HttpPanel::restore(info, cx));
        Box::new(view) as Box<dyn PanelView>
    });
    register_panel(cx, "PlanReview", |_, _, info, _window, cx| {
        let deps = cx.global::<ShellDeps>().clone();
        let id = SessionId::new(panel_info_str(info, "session").unwrap_or_default());
        let view = cx.new(|cx| {
            // Rehydrated tab: no live hold, so it opens non-pending (review-only)
            // until a fresh proposal re-arms the approve/reject buttons.
            PlanReviewPanel::new(
                id,
                "(plan from a previous run — re-trigger to view)".into(),
                false,
                Some(deps.commands.clone()),
                deps.bus.subscribe(),
                cx,
            )
        });
        Box::new(view) as Box<dyn PanelView>
    });
    register_panel(cx, "CodeReview", |_, _, info, _window, cx| {
        let deps = cx.global::<ShellDeps>().clone();
        let id = SessionId::new(panel_info_str(info, "session").unwrap_or_default());
        let root = panel_info_str(info, "root").map(PathBuf::from);
        let summary = panel_info_str(info, "summary");
        let view =
            cx.new(|cx| CodeReviewPanel::new(id, root, summary, Some(deps.commands.clone()), cx));
        Box::new(view) as Box<dyn PanelView>
    });
}

/// Pull a string field out of a panel's stashed `PanelInfo::Panel(json)`.
fn panel_info_str(info: &PanelInfo, key: &str) -> Option<String> {
    match info {
        PanelInfo::Panel(value) => value.get(key).and_then(|v| v.as_str()).map(str::to_owned),
        _ => None,
    }
}

/// Which **tool window** the bottom dock fronts (one body at a time). Each tool
/// window owns its own tab bar — the terminal its shell tabs, the Run window its
/// per-run onglets — and the activity rail's stripe buttons switch/toggle them.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BottomTool {
    Terminal,
    Run,
    Problems,
    Git,
    Services,
}

/// Which **tool window** the left dock fronts — JetBrains' Project ⇄ Commit
/// alternation. The stripe's top-group buttons swap the dock's content (the
/// dock is rebuilt around the workspace-owned panel entities).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LeftTool {
    /// Project files (tree, with the Structure outline stacked when on).
    Project,
    /// The Commit tool (staged/unstaged files + message + commit).
    Commit,
}

pub struct Workspace {
    dock_area: Entity<DockArea>,
    /// Open center tabs **per space** (`None` = Overview), keyed by `OpenRequest::key`.
    /// A file/session opened in one space is scoped to it: switching spaces removes
    /// the outgoing space's tabs and mounts the incoming space's. `GridHome` is NOT
    /// tracked here — it's the constant home tab kept mounted across every space (it
    /// can't be rebuilt cheaply: detection only emits deltas, so a fresh GridHome
    /// would show an empty fleet until activity).
    space_panels: HashMap<Option<SpaceId>, HashMap<String, Arc<dyn PanelView>>>,
    /// The session tab that was last frontmost **per space** (`OpenRequest::key`, i.e.
    /// `session:<id>`). Updated whenever a session tab becomes active (the active-context
    /// observer). On returning to a space, its remembered tab is mounted last so it lands
    /// frontmost — without this, whichever tab happened to mount last (arbitrary `HashMap`
    /// order) would steal focus. Seeded from / persisted to the open-tabs sidecar so the
    /// last-active session also survives a restart.
    space_active_tab: HashMap<Option<SpaceId>, String>,
    /// The space whose dynamic tabs are currently mounted in the center.
    current_space: Option<SpaceId>,
    /// Whether the status bar's notifications popover is open (stub; prepared for
    /// heavy future use).
    notifications_open: bool,
    /// Highest notification id already shown as a toast, so a fresh notification pops
    /// once (JetBrains-style) without re-toasting the whole inbox on every change.
    last_toast_id: Option<u64>,
    /// The bottom dock — composed at the **workspace level** rather than via
    /// gpui-component's `DockArea` (which renders the side docks *outside* the bottom
    /// one), so it spans the **full width** beneath the left dock: the priority surface
    /// for the terminal today, run logs / services to come.
    ///
    /// The terminal is **per space**: each space owns its own pinned terminal (shells
    /// rooted at that space), so switching spaces shows that space's shells instead of
    /// `cd`-ing one shared shell — which used to let spaces stomp on each other.
    /// Keyed by the active space (`None` = Overview); lazily created on first render of
    /// a space and pruned when its space is closed. See [`Self::ensure_space_terminal`].
    space_terminals: HashMap<Option<SpaceId>, Entity<TerminalPanel>>,
    /// Whether the bottom dock is open, and its operator-resizable height (px).
    bottom_open: bool,
    bottom_height: f32,
    /// Active bottom-dock resize drag: `Some((pointer-y at grab, height at grab))`.
    resize_anchor: Option<(f32, f32)>,
    /// The Run console (the bottom dock's second tool) + which tool is frontmost.
    run_console: Entity<super::panels::run_console::RunConsolePanel>,
    /// The Services view (control server + MCP host status; third bottom tool).
    services: Entity<super::panels::services::ServicesPanel>,
    /// The Problems view (LSP diagnostics grouped by file; fourth bottom tool).
    problems: Entity<super::panels::problems::ProblemsPanel>,
    /// The Git view (log + the IDE's git-op console; fifth bottom tool).
    git: Entity<super::panels::git_panel::GitPanel>,
    bottom_tool: BottomTool,
    /// The left dock's tool windows, owned here (like the bottom tools) so the
    /// dock can be **rebuilt** around them — toggling Structure off rebuilds the
    /// left `DockItem` with the tree alone, keeping the tree's entity (and state).
    file_tree: Entity<FileTreePanel>,
    structure: Entity<StructurePanel>,
    /// The Commit tool window (the left dock's alternate content) + which left
    /// tool the dock currently fronts.
    commit: Entity<super::panels::commit::CommitPanel>,
    /// The DB overview tool window (right dock): a data-source tree the operator adds
    /// databases to manually. Owned here so file-tree opens / the stripe toggle act on
    /// the same entity (rebuilt around it after a layout restore, like the left tools).
    db_observer: Entity<DbObserverPanel>,
    left_tool: LeftTool,
    /// Whether the Structure outline is shown under the tree (stripe-toggleable,
    /// JetBrains-style; independent of the left dock's own open state).
    structure_open: bool,
    /// Which of the main toolbar's dropdowns is open (at most one at a time): the
    /// project selector, the branch selector, the run-target picker.
    space_menu_open: bool,
    branch_menu_open: bool,
    run_menu_open: bool,
    /// The operator-selected run target (the run config's command id). `None` →
    /// the active space's first detected target is the default. See [`run_config`].
    run_target: Option<String>,
    /// Whether the Create Run Configuration modal is open.
    show_create_run_config: bool,
    /// Input state for the Run Config modal's custom label.
    run_config_label_input: Option<Entity<InputState>>,
    /// Input state for the Run Config modal's custom command.
    run_config_command_input: Option<Entity<InputState>>,
    /// Selected run kind for the custom run config.
    run_config_kind: RunKind,
    /// Per-session root + status, folded from the engine bus. Drives the space
    /// tabs' attention dots: a space lights up when a session under its root is
    /// Waiting/Errored. (The fleet grid keeps its own richer model; this is just
    /// the thin slice the chrome needs.)
    session_attention: HashMap<SessionId, (Option<PathBuf>, SessionStatus)>,
    /// Live per-session ⚠ attention overlay (Stuck/Incomplete), folded from
    /// [`EngineEvent::SessionAlert`]. Consulted by [`Self::space_attention`] alongside
    /// status so a space-tab dot also lights for a stuck/incomplete session, not just
    /// Waiting/Errored. Transient (mirrors the grid's overlay).
    session_alerts: HashMap<SessionId, AttentionKind>,
    /// Center tabs that were open at the last shutdown for spaces **other** than the
    /// active one — restored lazily (the panels, with their terminals, are rebuilt the
    /// first time that space is switched to, not all at once on launch). Keyed like
    /// `space_panels`; the active space's tabs are reopened eagerly in `new` instead.
    pending_space_tabs: HashMap<Option<SpaceId>, Vec<String>>,
    /// Last open-tabs snapshot serialized to disk, so the crash-safe periodic saver
    /// (see [`Self::persist_open_tabs_if_changed`]) only writes when the set actually
    /// changed. `None` until the first save.
    last_saved_tabs: Option<String>,
    /// Whether the startup restore has finished populating `space_panels`. **Persistence
    /// is gated on this**: saving before restore mounts the previous session's tabs would
    /// capture a near-empty live center and overwrite the good `open_tabs.json` (data
    /// loss). Stays `false` in safe mode (restore skipped) so the saved file is preserved
    /// untouched for the next normal launch.
    restore_done: bool,
}

/// Marker type identifying inbox toasts in gpui-component's notification layer —
/// paired with the inbox entry's id via [`Notification::id1`] so the lifetime
/// timer can close exactly the toast it armed ([`WindowExt::remove_notification1`]).
struct InboxToast;

/// How long a toast stays up before closing itself; a click dismisses it sooner.
/// Short by request — the bell inbox keeps the full history, so the toast is just a
/// brief heads-up, not a thing to read at length.
const TOAST_LIFETIME: std::time::Duration = std::time::Duration::from_secs(6);

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dock_area = cx.new(|cx| DockArea::new(DOCK_ID, Some(DOCK_VERSION), window, cx));
        let weak = dock_area.downgrade();

        // The left dock's tool windows are owned by the workspace (like the bottom
        // tools) so the dock can be rebuilt around the same entities when the
        // Structure toggle / Project⇄Commit swap changes the dock's shape.
        let (file_tree, structure, commit) = {
            let deps = cx.global::<ShellDeps>().clone();
            (
                cx.new(|cx| FileTreePanel::new(Some(deps.focus.clone()), cx)),
                cx.new(|cx| StructurePanel::new(Some(deps.active_editor.clone()), cx)),
                cx.new(|cx| super::panels::commit::CommitPanel::new(Some(deps.focus.clone()), cx)),
            )
        };
        // The DB overview tool window is workspace-owned too, so file-tree opens and the
        // stripe toggle act on the same entity (and it survives a layout restore).
        let db_observer = cx.new(DbObserverPanel::new);
        let structure_open = true;
        let left_tool = LeftTool::Project;

        // Restore a saved layout; on any failure (missing/old/corrupt) rebuild the
        // default — never crash on persisted state (it is effectively untrusted).
        match load_layout(&dock_area, window, cx) {
            Ok(()) => {
                // The restored center may hold dynamic tabs from a previous run, but
                // per-space tabs are in-memory and not attributed to a space across a
                // restart — so start from a clean GridHome-only center (the rails are
                // kept). Open files don't survive restart yet (follow-up).
                reset_center_to_home(&dock_area, weak.clone(), window, cx);
                // Likewise the restored left dock carries its own panel instances;
                // rebuild it around the workspace-owned tree/structure (keeping the
                // restored width + open state) so the Structure toggle always acts
                // on the entities the workspace holds.
                let (size, open) = dock_area
                    .read(cx)
                    .left_dock()
                    .map(|d| {
                        let d = d.read(cx);
                        (Some(d.size()), d.is_open())
                    })
                    .unwrap_or((Some(px(300.)), true));
                set_left_tools(
                    &weak,
                    &file_tree,
                    &structure,
                    &commit,
                    left_tool,
                    structure_open,
                    size,
                    open,
                    window,
                    cx,
                );
                // Likewise rebuild the right dock (DB overview) around the owned observer
                // if the restored layout had one, so toggles/adds act on this entity.
                let right = dock_area.read(cx).right_dock().map(|d| {
                    let d = d.read(cx);
                    (Some(d.size()), d.is_open())
                });
                if let Some((size, open)) = right {
                    let item = DockItem::tab(db_observer.clone(), &weak, window, cx);
                    dock_area.update(cx, |area, cx| {
                        area.set_right_dock(item, size, open, window, cx);
                    });
                }
            }
            Err(err) => {
                tracing::info!(reason = %err, "building default dock layout");
                reset_default_layout(weak.clone(), &file_tree, &structure, &commit, window, cx);
            }
        }

        // Persist the final layout + the open tabs on quit so they survive a restart.
        cx.on_app_quit({
            let dock_area = dock_area.clone();
            move |this, cx| {
                let state = dock_area.read(cx).dump(cx);
                // Only persist tabs if restore finished: a quit before/without restore
                // (safe mode, or quit during the deferred restore tick) would capture a
                // near-empty center and overwrite the saved tabs. When skipped, the
                // existing good `open_tabs.json` is left untouched for the next launch.
                let tabs = this.restore_done.then(|| this.capture_open_tabs(cx));
                cx.background_executor().spawn(async move {
                    if let Err(err) = save_state(&state) {
                        tracing::warn!(error = %err, "failed to save dock layout");
                    }
                    if let Some(tabs) = tabs {
                        open_tabs::save(&tabs);
                    }
                })
            }
        })
        .detach();

        // Open files / session monitors when rail panels request them.
        let center = cx.global::<ShellDeps>().center.clone();
        cx.subscribe_in(
            &center,
            window,
            |this, _center, request: &OpenRequest, window, cx| {
                this.open_in_center(request.clone(), window, cx);
            },
        )
        .detach();

        // Hide a dock/tool when its header's "✕" raises a chrome request.
        let chrome = cx.global::<ShellDeps>().chrome.clone();
        cx.subscribe_in(
            &chrome,
            window,
            |this, _chrome, request: &ChromeRequest, window, cx| {
                this.handle_chrome(*request, window, cx);
            },
        )
        .detach();

        // Auto-open the review gates from engine facts. Re-emits `PlanProposed` /
        // `ReviewReady` as `OpenRequest`s on the center channel (the subscription
        // above has the `Window` `add_panel` needs). Repo roots aren't carried on
        // `ReviewReady`, so we track each session's `attached_path` from the
        // `SessionUpserted` facts that flow by and look it up when review opens.
        {
            let center = center.clone();
            let notifications = cx.global::<ShellDeps>().notifications.clone();
            // Retain outstanding approval holds so a monitor opened after the one-shot
            // `ApprovalRequested` can rebuild its popup (see `approvals` module).
            let approvals = cx.global::<ShellDeps>().approvals.clone();
            let mut rx = cx.global::<ShellDeps>().bus.subscribe();
            cx.spawn(async move |this, cx| {
                let mut roots: HashMap<SessionId, Option<PathBuf>> = HashMap::new();
                // Session labels tracked off `SessionUpserted`, so notifications read
                // the friendly title (falling back to the short id) instead of a uuid.
                let mut labels: HashMap<SessionId, String> = HashMap::new();
                // Last-seen phase + status per session, so we notify only on a real
                // *transition* (and never on first sight — avoids startup spam).
                let mut phases: HashMap<SessionId, Phase> = HashMap::new();
                let mut statuses: HashMap<SessionId, SessionStatus> = HashMap::new();
                // Latest assistant summary per session (T4), so the review tab opens
                // with "what this covers" already populated.
                let mut summaries: HashMap<SessionId, String> = HashMap::new();
                loop {
                    match rx.recv().await {
                        Ok(EngineEvent::SessionUpserted { session }) => {
                            let id = session.id.clone();
                            let label = session.label().to_string();
                            // A full row that no longer shows the session blocked means
                            // any held approval was resolved elsewhere — drop the retained
                            // hold so a reopened monitor doesn't rebuild a stale popup
                            // (mirrors `SessionMonitor::apply_event`).
                            if session.status != SessionStatus::WaitingInput {
                                approvals.clear(&id);
                            }
                            labels.insert(id.clone(), label.clone());
                            roots.insert(
                                id.clone(),
                                session.attached_path.clone().map(PathBuf::from),
                            );
                            // Mirror the thin (root, status) slice onto the workspace
                            // so the space tabs' attention dots track live state.
                            let entry = (
                                session.attached_path.clone().map(PathBuf::from),
                                session.status,
                            );
                            let _ = this.update(cx, |ws, cx| {
                                if ws.session_attention.get(&id) != Some(&entry) {
                                    ws.session_attention.insert(id.clone(), entry);
                                    cx.notify();
                                }
                            });
                            // Phase end: a tracked session moved to a new phase. (Real
                            // transitions publish the full-row SessionUpserted.)
                            if let Some(prev) = phases.insert(id.clone(), session.phase) {
                                if let Some((kind, text)) =
                                    phase_notification(prev, session.phase, &label)
                                {
                                    notifications.update(cx, |n, cx| {
                                        n.push(kind, text, Some(id.clone()));
                                        cx.notify();
                                    });
                                }
                            }
                            // Needs-input / errored: only on a witnessed status change.
                            if let Some(prev) = statuses.insert(id.clone(), session.status) {
                                if prev != session.status {
                                    if let Some((kind, text)) =
                                        status_notification(session.status, &label)
                                    {
                                        notifications.update(cx, |n, cx| {
                                            n.push(kind, text, Some(id.clone()));
                                            cx.notify();
                                        });
                                    }
                                }
                            }
                        }
                        Ok(EngineEvent::PlanProposed { session, plan }) => {
                            let text =
                                format!("Plan proposed · {}", notif_label(&labels, &session));
                            notifications.update(cx, |n, cx| {
                                n.push(NotificationKind::Approval, text, Some(session.clone()));
                                cx.notify();
                            });
                            let root = roots.get(&session).cloned().flatten();
                            center.update(cx, |_center, cx| {
                                cx.emit(OpenRequest::PlanReview {
                                    session,
                                    plan,
                                    root,
                                });
                            });
                        }
                        Ok(EngineEvent::SummaryObserved { session, summary }) => {
                            // Cache only — surfaced when the review tab opens (below).
                            summaries.insert(session, summary);
                        }
                        Ok(EngineEvent::ReviewReady { session }) => {
                            let text = format!("Review ready · {}", notif_label(&labels, &session));
                            notifications.update(cx, |n, cx| {
                                n.push(NotificationKind::Review, text, Some(session.clone()));
                                cx.notify();
                            });
                            let root = roots.get(&session).cloned().flatten();
                            let summary = summaries.get(&session).cloned();
                            center.update(cx, |_center, cx| {
                                cx.emit(OpenRequest::CodeReview {
                                    session,
                                    root,
                                    summary,
                                });
                            });
                        }
                        Ok(EngineEvent::ApprovalRequested {
                            session,
                            what,
                            authorize_tool,
                        }) => {
                            let text = match &authorize_tool {
                                Some(tool) => {
                                    format!("Authorize {tool} · {}", notif_label(&labels, &session))
                                }
                                None => format!("{what} · {}", notif_label(&labels, &session)),
                            };
                            notifications.update(cx, |n, cx| {
                                n.push(NotificationKind::Approval, text, Some(session.clone()));
                                cx.notify();
                            });
                            // Retain the hold so a monitor opened later (via the bell
                            // notification / a space switch) can rebuild the popup — the
                            // one-shot event below reaches only monitors mounted right now.
                            approvals.set(
                                session.clone(),
                                crate::views::approvals::ApprovalHold {
                                    what: what.clone(),
                                    tool: authorize_tool.clone(),
                                },
                            );
                            // The verdict UI (external-MCP tool + sensitive phase change)
                            // is the session's own bottom-right authorization popup,
                            // driven by its `SessionMonitor` bus subscription — no center
                            // tab is opened here. See `SessionMonitor::authorize_popup`.
                        }
                        Ok(EngineEvent::PhaseAdvanceRequested { session, to }) => {
                            let text = format!(
                                "Advance to {} · {}",
                                to.label(),
                                notif_label(&labels, &session)
                            );
                            notifications.update(cx, |n, cx| {
                                n.push(NotificationKind::Advance, text, Some(session));
                                cx.notify();
                            });
                        }
                        Ok(EngineEvent::SessionStateChanged { session, status }) => {
                            // A resume (any status but WaitingInput) means an outstanding
                            // authorization hold was resolved — drop the retained copy.
                            if status != SessionStatus::WaitingInput {
                                approvals.clear(&session);
                            }
                            // Keep the attention slice live on thin status updates
                            // (root from the last full upsert, if one was seen).
                            let root = roots.get(&session).cloned().flatten();
                            let _ = this.update(cx, |ws, cx| {
                                match ws.session_attention.get_mut(&session) {
                                    Some(e) if e.1 == status => {}
                                    Some(e) => {
                                        e.1 = status;
                                        cx.notify();
                                    }
                                    None => {
                                        ws.session_attention
                                            .insert(session.clone(), (root.clone(), status));
                                        cx.notify();
                                    }
                                }
                            });
                            // Notify on a witnessed status change (CC needs input /
                            // errored). `prev.is_some()` skips first-sight, so a fleet
                            // already in those states on startup doesn't spam.
                            let prev = statuses.insert(session.clone(), status);
                            if prev.is_some() && prev != Some(status) {
                                let label = notif_label(&labels, &session);
                                if let Some((kind, text)) = status_notification(status, &label) {
                                    notifications.update(cx, |n, cx| {
                                        n.push(kind, text, Some(session));
                                        cx.notify();
                                    });
                                }
                            }
                        }
                        Ok(EngineEvent::PhaseTransitioned { session, phase }) => {
                            // A phase move resolves a pending phase-change approval.
                            approvals.clear(&session);
                            // Thin phase update (some engine paths use this instead of a
                            // full SessionUpserted). Dedupes against the same map.
                            if let Some(prev) = phases.insert(session.clone(), phase) {
                                let label = notif_label(&labels, &session);
                                if let Some((kind, text)) = phase_notification(prev, phase, &label)
                                {
                                    notifications.update(cx, |n, cx| {
                                        n.push(kind, text, Some(session));
                                        cx.notify();
                                    });
                                }
                            }
                        }
                        Ok(EngineEvent::SessionRemoved { session }) => {
                            // Forgotten session: drop any retained approval hold too.
                            approvals.clear(&session);
                            // Forgotten session: drop its attention entry so a stale
                            // dot doesn't keep a space lit.
                            let _ = this.update(cx, |ws, cx| {
                                let a = ws.session_attention.remove(&session).is_some();
                                let b = ws.session_alerts.remove(&session).is_some();
                                if a || b {
                                    cx.notify();
                                }
                            });
                        }
                        Ok(EngineEvent::SessionAlert { session, alert }) => {
                            // Raise/clear the space-tab dot's ⚠ overlay alongside status.
                            let _ = this.update(cx, |ws, cx| {
                                let changed = match alert {
                                    Some(kind) => {
                                        ws.session_alerts.insert(session.clone(), kind)
                                            != Some(kind)
                                    }
                                    None => ws.session_alerts.remove(&session).is_some(),
                                };
                                if changed {
                                    cx.notify();
                                }
                            });
                        }
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            })
            .detach();
        }

        // When the active space changes, swap the center's dynamic tabs to that
        // space's set (scoping open files to their space); also re-render the top bar
        // and prune tabs for spaces that were closed.
        let focus = cx.global::<ShellDeps>().focus.clone();
        cx.observe_in(&focus, window, |this, focus, window, cx| {
            let active = focus.read(cx).active().cloned();
            if active != this.current_space {
                this.switch_space(active, window, cx);
            }
            this.prune_closed_spaces(&focus, cx);
            cx.notify();
        })
        .detach();

        // Re-render the status bar when the frontmost file/session context changes
        // (a caret move, a tab switch, a live session-status update all flow here).
        // Also remember which **session** tab is frontmost in the current space, so
        // returning to that space later re-focuses it (see `incoming_ordered`). Only a
        // session tab can be active in the mounted space, so it's keyed to `current_space`.
        let active_context = cx.global::<ShellDeps>().active_context.clone();
        cx.observe(&active_context, |this, ac, cx| {
            if let crate::views::active_context::ActiveContext::Session { id, .. } = ac.read(cx) {
                let key = format!("session:{}", id.as_str());
                this.space_active_tab
                    .insert(this.current_space.clone(), key);
            }
            cx.notify();
        })
        .detach();

        // Re-render the bar (bell badge + popover) when the notification inbox changes,
        // and pop each *new* notification as a toast (JetBrains-style). Toasts are
        // deliberately hard to miss: a large box that stays up for [`TOAST_LIFETIME`]
        // and closes on a click anywhere on it. `observe_in` hands us the `Window`
        // that `push_notification` needs.
        let notifications = cx.global::<ShellDeps>().notifications.clone();
        cx.observe_in(&notifications, window, |this, notifs, window, cx| {
            // Items are newest-first; toast those with an id above the last toasted.
            let last = this.last_toast_id;
            let fresh: Vec<(u64, NotificationKind, String)> = {
                let n = notifs.read(cx);
                if let Some(top) = n.items().first() {
                    this.last_toast_id = Some(top.id);
                }
                n.items()
                    .iter()
                    .take_while(|it| last.map_or(true, |l| it.id > l))
                    .map(|it| (it.id, it.kind, it.text.clone()))
                    .collect()
            };
            // Push oldest-first so the newest ends up on top of the toast stack.
            for (id, kind, text) in fresh.into_iter().rev() {
                // Style each toast like a bell-popover row — the kind glyph in its
                // color + the message — inside gpui-component's themed frame, sized
                // up so it can't be missed. gpui-component's own autohide (hardcoded
                // 5s) is off; we close it ourselves after [`TOAST_LIFETIME`], and a
                // click anywhere on the toast dismisses it immediately (the inbox
                // entry stays unread — the bell still tracks it).
                let (glyph, color) = status_bar::kind_style(kind);
                let note = Notification::new()
                    .id1::<InboxToast>(id as usize)
                    .autohide(false)
                    // The library dismisses on click before running the callback;
                    // dismiss is all we want.
                    .on_click(|_, _, _| {})
                    // Narrower + more vertical padding: a taller, slimmer box. The
                    // narrow width wraps longer messages onto more lines, adding height.
                    .w(px(300.))
                    .py_5()
                    .content(move |_, _, _| {
                        div()
                            .flex()
                            .flex_row()
                            .items_start()
                            .gap_3()
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(color)
                                    .text_size(theme::text_lg())
                                    .child(glyph),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_color(theme::text_primary())
                                    .text_size(theme::text_base())
                                    .child(text.clone()),
                            )
                            .into_any_element()
                    });
                window.push_notification(note, cx);
                // Self-managed lifetime: close this toast (by its inbox id) once
                // TOAST_LIFETIME elapses. A no-op if the user already clicked it away.
                cx.spawn_in(window, async move |_, cx| {
                    cx.background_executor().timer(TOAST_LIFETIME).await;
                    let _ = cx.update(|window, cx| {
                        window.remove_notification1::<InboxToast>(id as usize, cx);
                    });
                })
                .detach();
            }
            cx.notify();
        })
        .detach();

        // Poll the statusline-fed Claude-obs directory into the read-model on a slow
        // timer (off the engine path); re-render the bar when it changes.
        let obs_store = cx.global::<ShellDeps>().obs_store.clone();
        // Managed-session store, for resolving each AGY session's model from its own
        // transcript (keyed by our managed id via a root→conversationId bridge).
        let managed_store = cx.global::<ShellDeps>().store.clone();
        cx.observe(&obs_store, |_this, _o, cx| cx.notify()).detach();
        let mut tick: u64 = 0;
        cx.spawn(async move |this, cx| loop {
            // The obs store lives in the global `ShellDeps` (never drops), so gate the
            // loop on the workspace view instead — stop once the window is gone.
            if this.upgrade().is_none() {
                break;
            }
            // Sessions refresh every tick (~1.2s); the account quota is network-bound
            // (self-fetched from Anthropic), so refresh it on the first tick and every
            // ~2 min thereafter.
            let do_quota = tick % 100 == 0;
            // Managed AGY sessions `(managed_id, root)` — the bridge input for resolving
            // each one's model from AGY's own transcript (see `obs::agy_models`).
            let managed_agy: Vec<(String, String)> = managed_store
                .all_managed()
                .unwrap_or_default()
                .into_iter()
                .filter(|m| m.agent == moonlight_domain::AgentKind::Antigravity)
                .filter_map(|m| m.root.map(|r| (m.id.as_str().to_string(), r)))
                .collect();
            let (loaded, quota) = cx
                .background_executor()
                .spawn(async move {
                    let q = do_quota.then(crate::obs::quota).flatten();
                    // Claude obs (statusline JSON) + AGY models (from AGY's own transcript,
                    // keyed by our managed id) into the one read-model, so the bar shows
                    // each session's backend-correct model. No third-party cache.
                    let mut loaded = crate::obs::load_all();
                    loaded.extend(crate::obs::agy_models(&managed_agy));
                    (loaded, q)
                })
                .await;
            obs_store.update(cx, |s, cx| {
                let mut changed = s.apply(loaded);
                if do_quota {
                    changed |= s.set_quota(quota);
                }
                if changed {
                    cx.notify();
                }
            });
            tick = tick.wrapping_add(1);
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1200))
                .await;
        })
        .detach();

        let current_space = cx.global::<ShellDeps>().focus.read(cx).active().cloned();
        // The bottom dock's tools are owned here (not by the DockArea) so the dock can
        // span the full width beneath the left dock: the terminal + the Run console
        // (the latter polling the shared run registry).
        let registry = cx.global::<ShellDeps>().run_registry.clone();
        let run_console =
            cx.new(|cx| super::panels::run_console::RunConsolePanel::new(registry, window, cx));
        // The run state mutates off-thread (reader/waiter threads); the console's
        // dirty-poll notifies on every change, so observing it keeps the *chrome* —
        // the toolbar's play⇄stop swap, the Run chip's lamp — live too (e.g. the ⏹
        // flipping back to ▶ when the run exits on its own).
        cx.observe(&run_console, |_, _, cx| cx.notify()).detach();
        // The Services view (third bottom tool): control-server + MCP-host status.
        let services = cx.new(|cx| {
            super::panels::services::ServicesPanel::new(crate::control_socket_path(), cx)
        });
        // The Problems view (fourth bottom tool): diagnostics off the shared LSP
        // pool. Observed so its stripe lamp (chrome) updates when counts change.
        let lsp_pool = cx.global::<ShellDeps>().lsp_pool.clone();
        let problems = cx.new(|cx| super::panels::problems::ProblemsPanel::new(lsp_pool, cx));
        cx.observe(&problems, |_, _, cx| cx.notify()).detach();
        // The Git view (fifth bottom tool): repo log + the IDE's git-op console.
        let git_focus = cx.global::<ShellDeps>().focus.clone();
        let git = cx.new(|cx| super::panels::git_panel::GitPanel::new(Some(git_focus), cx));
        let this = Self {
            dock_area,
            space_panels: HashMap::new(),
            space_active_tab: HashMap::new(),
            current_space,
            notifications_open: false,
            last_toast_id: None,
            session_attention: HashMap::new(),
            session_alerts: HashMap::new(),
            pending_space_tabs: HashMap::new(),
            space_terminals: HashMap::new(),
            bottom_open: true,
            bottom_height: 240.,
            resize_anchor: None,
            run_console,
            services,
            problems,
            git,
            bottom_tool: BottomTool::Terminal,
            file_tree,
            structure,
            commit,
            db_observer,
            left_tool,
            structure_open,
            space_menu_open: false,
            branch_menu_open: false,
            run_menu_open: false,
            run_target: None,
            show_create_run_config: false,
            run_config_label_input: None,
            run_config_command_input: None,
            run_config_kind: RunKind::Run,
            last_saved_tabs: None,
            restore_done: false,
        };
        // Reopen the center tabs that were open at the last shutdown — **deferred to the
        // next tick**. The restore adds panels via `DockArea::add_panel(Center, …)`,
        // which resolves the dock's *active* center tab panel; during `Workspace::new`
        // the workspace isn't wrapped in its `Root` yet and the dock hasn't rendered, so
        // there is no active panel to add onto and the mounts silently go nowhere. A
        // one-shot spawn runs `restore_or_safe_mode` after construction unwinds, when the
        // window + dock are live (this is why earlier synchronous restore never showed
        // tabs at runtime). The center already shows GridHome from the layout-restore.
        cx.spawn_in(window, async move |this, cx| {
            let _ = this.update_in(cx, |this, window, cx| {
                this.restore_or_safe_mode(window, cx);
            });
        })
        .detach();

        // Crash-safe persistence: snapshot the open tabs on a slow timer and save on
        // change, so a crash / SIGKILL (which skips `on_app_quit`) loses at most a
        // couple of seconds of tab changes instead of the whole set. This also catches
        // tab *closes* — they go through the dock "×" with no workspace hook, and
        // `capture_open_tabs` reconciles the current space against the live center keys.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(2000))
                .await;
            let alive = this
                .update(cx, |this, cx| this.persist_open_tabs_if_changed(cx))
                .is_ok();
            if !alive {
                break; // window gone
            }
        })
        .detach();
        this
    }

    /// Run the eager open-tab restore, guarded by the safe-mode breadcrumb.
    ///
    /// Safe mode: the eager restore builds panels + spawns the sessions' PTYs. A panic
    /// here used to be fatal at launch and, since `open_tabs.json` reloads every launch,
    /// became a boot loop. If the previous launch died mid-restore (breadcrumb still
    /// set), skip it once so the operator gets back in; the fleet grid still rehydrates
    /// the sessions from the managed store, so nothing is lost. The next clean launch
    /// restores normally. Called deferred from [`Self::new`] (the dock must be live).
    fn restore_or_safe_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if super::restore_guard::crashed_last_time() {
            super::restore_guard::finish(); // clear so the next launch restores normally
            tracing::warn!(
                "previous restore did not finish — skipping eager tab restore (safe mode)"
            );
            if let Some(notifications) = cx
                .try_global::<ShellDeps>()
                .map(|d| d.notifications.clone())
            {
                notifications.update(cx, |n, cx| {
                    n.push(
                        NotificationKind::Error,
                        "Previous restore didn't finish — tabs not reopened. Your sessions are \
                         still in the grid."
                            .to_string(),
                        None,
                    );
                    cx.notify();
                });
            }
            return;
        }
        super::restore_guard::begin();
        self.restore_open_tabs(window, cx);
        super::restore_guard::finish();
        // Restore mounted the previous tabs into `space_panels` — persistence may now run
        // (capturing the live center reflects the restored set, not an empty one). Seed
        // `last_saved_tabs` with the just-restored snapshot so the first periodic tick is
        // a no-op rather than an immediate identical rewrite.
        self.restore_done = true;
        if let Ok(json) = serde_json::to_string(&self.capture_open_tabs(cx)) {
            self.last_saved_tabs = Some(json);
        }
    }

    /// Capture the open tabs and persist them on the background executor **only when
    /// they changed** since the last save (dirty-compared via the serialized form). The
    /// crash-safe twin of the `on_app_quit` flush — see the periodic saver in [`Self::new`].
    fn persist_open_tabs_if_changed(&mut self, cx: &mut Context<Self>) {
        // Never persist before restore has repopulated the center — a pre-restore capture
        // is near-empty and would clobber the saved tabs (the bug that ate them).
        if !self.restore_done {
            return;
        }
        let state = self.capture_open_tabs(cx);
        let Ok(json) = serde_json::to_string(&state) else {
            return;
        };
        if self.last_saved_tabs.as_deref() == Some(json.as_str()) {
            return;
        }
        self.last_saved_tabs = Some(json);
        cx.background_executor()
            .spawn(async move { open_tabs::save(&state) })
            .detach();
    }

    /// The space tab's two attention signals among the sessions under `root`:
    /// `(dot, needs_input)`. The **dot** is the worst "broke / stuck" kind
    /// (Stuck/Incomplete/Errored) by [`AttentionKind::severity`], or `None` when calm.
    /// **`needs_input`** is true when any session is awaiting the operator
    /// ([`AttentionKind::NeedsInput`]) — carried by the blinking caret, *not* the steady
    /// dot, so a "needs you now" pause is never hidden behind a higher-severity error
    /// (NeedsInput is the lowest severity and used to be outranked off the tab entirely).
    fn space_attention(&self, root: &Path) -> (Option<gpui::Hsla>, bool) {
        let mut worst: Option<AttentionKind> = None;
        let mut needs_input = false;
        for (id, (path, status)) in self.session_attention.iter() {
            let Some(path) = path else { continue };
            if !path.starts_with(root) {
                continue;
            }
            let attn = self
                .session_alerts
                .get(id)
                .copied()
                .or_else(|| AttentionKind::from_status(*status));
            match attn {
                // Carried by the caret, not the dot.
                Some(AttentionKind::NeedsInput) => needs_input = true,
                Some(a) if worst.map_or(true, |w| a.severity() > w.severity()) => worst = Some(a),
                _ => {}
            }
        }
        (worst.map(theme::attention_color), needs_input)
    }

    /// Toggle the left dock (Explorer: file tree + structure). Rail button.
    pub(crate) fn toggle_left_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(dock) = self.dock_area.read(cx).left_dock().cloned() {
            dock.update(cx, |d, cx| d.toggle_open(window, cx));
        }
    }

    /// Flip the operator's **auto-phasing** opt-in (the shared flag the MCP actor
    /// reads): when on, the agent's `request_phase` is auto-approved instead of raising
    /// a cockpit approval. Main-toolbar toggle.
    pub(crate) fn toggle_auto_phase(&mut self, cx: &mut Context<Self>) {
        if let Some(deps) = cx.try_global::<ShellDeps>() {
            let f = &deps.auto_phase;
            f.store(
                !f.load(std::sync::atomic::Ordering::Relaxed),
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        cx.notify();
    }

    /// The left dock's current (size, open) so a rebuild keeps the operator's
    /// width and visibility.
    fn left_dock_geometry(&self, cx: &Context<Self>) -> (Option<Pixels>, bool) {
        self.dock_area
            .read(cx)
            .left_dock()
            .map(|d| {
                let d = d.read(cx);
                (Some(d.size()), d.is_open())
            })
            .unwrap_or((Some(px(300.)), true))
    }

    /// Rebuild the left dock around the owned entities with the current
    /// `left_tool`/`structure_open`, keeping geometry. `force_open` reveals a
    /// hidden dock (a stripe click's intent is "show me this tool").
    fn rebuild_left(&mut self, force_open: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (size, open) = self.left_dock_geometry(cx);
        set_left_tools(
            &self.dock_area.downgrade(),
            &self.file_tree,
            &self.structure,
            &self.commit,
            self.left_tool,
            self.structure_open,
            size,
            open || force_open,
            window,
            cx,
        );
        cx.notify();
    }

    /// Stripe click on a left tool (Project / Commit) — JetBrains behavior:
    /// front it (opening the dock), swap the dock's content when the other tool
    /// is fronted, or collapse the dock when it's already the open frontmost.
    pub(crate) fn toggle_left_tool(
        &mut self,
        tool: LeftTool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (_, open) = self.left_dock_geometry(cx);
        if open && self.left_tool == tool {
            self.toggle_left_dock(window, cx); // collapse to the stripe
            cx.notify();
            return;
        }
        self.left_tool = tool;
        self.rebuild_left(true, window, cx);
    }

    /// Toggle the Structure outline under the tree (stripe button / header ✕).
    /// The dock's *shape* changes (tree alone ⇄ tree ÷ structure), so the left
    /// `DockItem` is rebuilt around the workspace-owned entities — geometry kept.
    /// Turning the outline **on** also reveals a hidden dock and fronts the
    /// Project tool (the outline lives under the tree).
    pub(crate) fn toggle_structure(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.structure_open = !self.structure_open;
        if self.structure_open {
            self.left_tool = LeftTool::Project;
        }
        self.rebuild_left(self.structure_open, window, cx);
    }

    /// Toggle the right dock (DB overview; right-stripe button / header ✕). The overview
    /// is the workspace-owned [`DbObserverPanel`] — a data-source tree the operator adds
    /// databases to manually (no database is opened automatically).
    pub(crate) fn toggle_db_observer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dock_area.read(cx).right_dock().is_some() {
            if let Some(dock) = self.dock_area.read(cx).right_dock().cloned() {
                dock.update(cx, |d, cx| d.toggle_open(window, cx));
            }
            cx.notify();
            return;
        }
        self.mount_db_dock(true, window, cx);
    }

    /// Mount the DB overview into the right dock around the owned observer, `open`.
    fn mount_db_dock(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        let weak = self.dock_area.downgrade();
        let item = DockItem::tab(self.db_observer.clone(), &weak, window, cx);
        self.dock_area.update(cx, |area, cx| {
            area.set_right_dock(item, Some(px(300.)), open, window, cx);
        });
        cx.notify();
    }

    /// Add a data source to the overview and front the dock (file-tree open / `+ Add`).
    fn add_db_source(&mut self, source: DataSource, window: &mut Window, cx: &mut Context<Self>) {
        self.db_observer
            .update(cx, |obs, cx| obs.add_source(source, cx));
        if let Some(dock) = self.dock_area.read(cx).right_dock().cloned() {
            dock.update(cx, |d, cx| d.set_open(true, window, cx));
            cx.notify();
        } else {
            self.mount_db_dock(true, window, cx);
        }
    }

    /// Apply a tool-window header's hide request (the uniform "✕" — see
    /// [`ChromeRequest`]). Hides are idempotent: a stale ✕ on an already-hidden
    /// tool is a no-op, never a re-open.
    fn handle_chrome(&mut self, req: ChromeRequest, window: &mut Window, cx: &mut Context<Self>) {
        match req {
            ChromeRequest::HideLeftDock => {
                if let Some(dock) = self.dock_area.read(cx).left_dock().cloned() {
                    dock.update(cx, |d, cx| d.set_open(false, window, cx));
                }
            }
            ChromeRequest::HideStructure => {
                if self.structure_open {
                    self.toggle_structure(window, cx);
                }
            }
            ChromeRequest::HideBottomDock => self.bottom_open = false,
            ChromeRequest::HideRightDock => {
                if let Some(dock) = self.dock_area.read(cx).right_dock().cloned() {
                    dock.update(cx, |d, cx| d.set_open(false, window, cx));
                }
            }
        }
        cx.notify();
    }

    /// Rail click on a bottom tool: front it (opening the dock), or hide the dock
    /// when it's already the frontmost open tool — JetBrains stripe-button behavior.
    pub(crate) fn toggle_bottom_tool(&mut self, tool: BottomTool, cx: &mut Context<Self>) {
        if self.bottom_open && self.bottom_tool == tool {
            self.bottom_open = false;
        } else {
            self.bottom_tool = tool;
            self.bottom_open = true;
        }
        cx.notify();
    }

    /// Front a bottom tool window, opening the dock if it was hidden (▶ does this
    /// with the Run window).
    pub(crate) fn select_bottom_tool(&mut self, tool: BottomTool, cx: &mut Context<Self>) {
        self.bottom_tool = tool;
        self.bottom_open = true;
        cx.notify();
    }

    /// The full-width bottom dock: a top resize grip over the frontmost **tool
    /// window** (Terminal or Run today; git / services later). Each tool window owns
    /// its *own* tab bar — the terminal its shell tabs, the Run window its per-run
    /// onglets — so the dock adds no header of its own; switching tools happens on
    /// the activity rail (stripe buttons), JetBrains-style.
    /// The dock terminal for the **current space**, creating it (pinned to that space's
    /// root, `None` = Overview → cwd) on first use. Each space keeps its own terminal so
    /// switching spaces reveals that space's own shells instead of re-`cd`-ing a single
    /// shared shell. The entity is retained in `space_terminals`, so a space's shells
    /// keep running while another space is shown, until the space is closed (pruned in
    /// [`Self::prune_closed_spaces`]).
    fn ensure_space_terminal(&mut self, cx: &mut Context<Self>) -> Entity<TerminalPanel> {
        let key = self.current_space.clone();
        if let Some(term) = self.space_terminals.get(&key) {
            return term.clone();
        }
        let focus = cx.global::<ShellDeps>().focus.clone();
        let cwd = || std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let root = match &key {
            Some(id) => focus
                .read(cx)
                .spaces()
                .iter()
                .find(|s| &s.id == id)
                .map(|s| s.root.clone())
                .unwrap_or_else(cwd),
            None => cwd(),
        };
        let term = cx.new(|cx| TerminalPanel::new_pinned(root, cx));
        self.space_terminals.insert(key, term.clone());
        term
    }

    fn bottom_dock(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_none()
            .flex()
            .flex_col()
            .h(px(self.bottom_height))
            .bg(theme::surface_base())
            // Resize grip on the top edge (drag to grow/shrink).
            .child(
                div()
                    .id("bottom-resize")
                    .flex_none()
                    .h(px(5.))
                    .w_full()
                    .cursor(CursorStyle::ResizeUpDown)
                    .border_t_1()
                    .border_color(theme::border_subtle())
                    .hover(|d| d.bg(theme::tint(theme::accent(), 0.20)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, ev: &MouseDownEvent, _w, cx| {
                            this.resize_anchor =
                                Some((f32::from(ev.position.y), this.bottom_height));
                            cx.notify();
                        }),
                    ),
            )
            // Frontmost tool window's body. Both entities stay alive; only the
            // frontmost renders (the terminal's PTY keeps running regardless).
            .child(div().flex_1().min_h_0().child(match self.bottom_tool {
                BottomTool::Terminal => self.ensure_space_terminal(cx).into_any_element(),
                BottomTool::Run => self.run_console.clone().into_any_element(),
                BottomTool::Problems => self.problems.clone().into_any_element(),
                BottomTool::Git => self.git.clone().into_any_element(),
                BottomTool::Services => self.services.clone().into_any_element(),
            }))
    }

    /// Toggle the status bar's notifications popover (called from the bell button).
    pub(crate) fn toggle_notifications(&mut self, cx: &mut Context<Self>) {
        self.notifications_open = !self.notifications_open;
        cx.notify();
    }

    /// Mark every notification read (popover "Mark all read").
    pub(crate) fn mark_all_notifications_read(&mut self, cx: &mut Context<Self>) {
        let n = cx.global::<ShellDeps>().notifications.clone();
        n.update(cx, |n, cx| {
            n.mark_all_read();
            cx.notify();
        });
    }

    /// Clear the notification inbox (popover "Clear").
    pub(crate) fn clear_notifications(&mut self, cx: &mut Context<Self>) {
        let n = cx.global::<ShellDeps>().notifications.clone();
        n.update(cx, |n, cx| {
            n.clear();
            cx.notify();
        });
    }

    /// Click-to-navigate from the notifications popover: mark the entry read, close the
    /// popover, and — if the notification names a session — bring that session's focus
    /// tab to front (reconstructed from the managed store; dedups onto an open tab).
    pub(crate) fn open_notification(
        &mut self,
        id: u64,
        session: Option<SessionId>,
        cx: &mut Context<Self>,
    ) {
        let deps = cx.global::<ShellDeps>().clone();
        deps.notifications.update(cx, |n, cx| {
            n.mark_read(id);
            cx.notify();
        });
        self.notifications_open = false;
        if let Some(session) = session {
            let root = deps
                .store
                .managed(&session)
                .ok()
                .flatten()
                .and_then(|rec| rec.root)
                .map(PathBuf::from);
            deps.center.update(cx, |_c, cx| {
                cx.emit(OpenRequest::SessionById { id: session, root });
            });
        }
        cx.notify();
    }

    /// Set the frontmost editor's line-ending (status-bar EOL click). Broadcasts on
    /// the editor-command channel; the active [`CodeEditorPanel`] applies it.
    pub(crate) fn set_editor_eol(&mut self, eol: Eol, cx: &mut Context<Self>) {
        let ec = cx.global::<ShellDeps>().editor_commands.clone();
        ec.update(cx, |_ec, cx| cx.emit(EditorCommand::SetEol(eol)));
    }

    /// Drop tracked tab sets for spaces that no longer exist (e.g. closed via the
    /// tab "×"), so a re-created space (same root ⇒ same id) starts fresh.
    fn prune_closed_spaces(&mut self, focus: &Entity<ProjectSpace>, cx: &App) {
        let live: HashSet<Option<SpaceId>> = std::iter::once(None)
            .chain(focus.read(cx).spaces().iter().map(|s| Some(s.id.clone())))
            .collect();
        self.space_panels.retain(|k, _| live.contains(k));
        self.pending_space_tabs.retain(|k, _| live.contains(k));
        self.space_active_tab.retain(|k, _| live.contains(k));
        // Drop a closed space's terminal too, shutting its shell(s) down.
        self.space_terminals.retain(|k, _| live.contains(k));
    }

    /// Snapshot the open center tabs (per space) for the shutdown sidecar. For the
    /// **current** space we reconcile its tracked keys against what is actually
    /// mounted (the operator may have closed a tab via "×"); other spaces' tracked
    /// sets were already reconciled when last switched away, plus any not-yet-realized
    /// pending tabs from a prior restore that were never visited.
    fn capture_open_tabs(&self, cx: &App) -> OpenTabsState {
        let live = self.live_center_keys(cx);
        let mut spaces: Vec<SpaceTabs> = Vec::new();
        let mut seen: HashSet<Option<SpaceId>> = HashSet::new();

        for (space, panels) in &self.space_panels {
            seen.insert(space.clone());
            let tabs: Vec<String> = if space == &self.current_space {
                panels
                    .keys()
                    .filter(|k| live.contains(*k))
                    .cloned()
                    .collect()
            } else {
                panels.keys().cloned().collect()
            };
            if !tabs.is_empty() {
                // Persist the last-active session tab only while it's still one of the
                // space's open tabs (it may have been closed since).
                let active_tab = self
                    .space_active_tab
                    .get(space)
                    .filter(|k| tabs.contains(k))
                    .cloned();
                spaces.push(SpaceTabs {
                    space: space.as_ref().map(|s| s.as_str().to_string()),
                    tabs,
                    active_tab,
                });
            }
        }
        // Carry forward pending (lazily-restored, never-visited) spaces verbatim.
        for (space, tabs) in &self.pending_space_tabs {
            if seen.contains(space) || tabs.is_empty() {
                continue;
            }
            let active_tab = self
                .space_active_tab
                .get(space)
                .filter(|k| tabs.contains(k))
                .cloned();
            spaces.push(SpaceTabs {
                space: space.as_ref().map(|s| s.as_str().to_string()),
                tabs: tabs.clone(),
                active_tab,
            });
        }

        OpenTabsState {
            version: open_tabs::OPEN_TABS_VERSION,
            active: self.current_space.as_ref().map(|s| s.as_str().to_string()),
            spaces,
        }
    }

    /// Rebuild per-space tab tracking from the shutdown sidecar: the active space's
    /// tabs are reopened (+ resumed) and mounted now; every other (still-open) space's
    /// tabs are stashed in `pending_space_tabs` to be realized on first switch. Tabs
    /// for spaces that no longer exist in the project list are dropped.
    fn restore_open_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = open_tabs::load() else {
            return;
        };
        // The spaces still known to the project list, by their id string.
        let known: HashMap<String, SpaceId> = self
            .project_space(cx)
            .read(cx)
            .spaces()
            .iter()
            .map(|s| (s.id.as_str().to_string(), s.id.clone()))
            .collect();

        let mut mounted = 0usize;
        let mut pending = 0usize;
        for entry in state.spaces {
            // Resolve the persisted id back to a live space (None = overview, always
            // valid). Skip a space that has since been forgotten.
            let space: Option<SpaceId> = match entry.space {
                None => None,
                Some(id) => match known.get(&id) {
                    Some(found) => Some(found.clone()),
                    None => continue,
                },
            };
            // Restore the last-active session tab for this space (eager or pending), so
            // the first mount / first switch lands it frontmost (see `incoming_ordered`).
            if let Some(active_tab) = entry.active_tab {
                self.space_active_tab.insert(space.clone(), active_tab);
            }
            if space == self.current_space {
                let deps = cx.global::<ShellDeps>().clone();
                let dock = self.dock_area.clone();
                for key in entry.tabs {
                    if let Some(panel) =
                        build_center_panel(&deps, &key, space.as_ref(), true, window, cx)
                    {
                        self.space_panels
                            .entry(space.clone())
                            .or_default()
                            .insert(key, panel);
                        mounted += 1;
                    }
                }
                // Mount with the remembered-active tab last so it lands frontmost (rather
                // than whichever tab happened to mount last). See `incoming_ordered`.
                for panel in self.incoming_ordered(&space) {
                    dock.update(cx, |area, cx| {
                        area.add_panel(panel, DockPlacement::Center, None, window, cx);
                    });
                }
            } else if !entry.tabs.is_empty() {
                pending += entry.tabs.len();
                self.pending_space_tabs.insert(space, entry.tabs);
            }
        }
        tracing::info!(mounted, pending, "restore_open_tabs");
    }

    /// Realize a space's lazily-restored tabs the first time it is switched to: rebuild
    /// each panel (resuming its terminal) into `space_panels` so the caller's mount step
    /// picks it up. A no-op once the space has no pending tabs.
    fn realize_pending(
        &mut self,
        space: &Option<SpaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(keys) = self.pending_space_tabs.remove(space) else {
            return;
        };
        let deps = cx.global::<ShellDeps>().clone();
        for key in keys {
            if self
                .space_panels
                .get(space)
                .is_some_and(|m| m.contains_key(&key))
            {
                continue; // Already live (e.g. reopened before the first switch).
            }
            if let Some(panel) = build_center_panel(&deps, &key, space.as_ref(), true, window, cx) {
                self.space_panels
                    .entry(space.clone())
                    .or_default()
                    .insert(key, panel);
            }
        }
    }

    /// Swap the center's dynamic tabs from the current space to `new`. Reconciles the
    /// outgoing set against what is actually mounted (the operator may have closed a
    /// tab via "×") so closed tabs are not resurrected, then removes the outgoing
    /// tabs and mounts the incoming ones. `GridHome` is untouched (constant home).
    fn switch_space(&mut self, new: Option<SpaceId>, window: &mut Window, cx: &mut Context<Self>) {
        let old = self.current_space.clone();
        if old == new {
            return;
        }
        // First switch to a space restored from the last shutdown: rebuild its tabs
        // (and resume their terminals) now, so the incoming collection below mounts
        // them. A no-op once realized.
        self.realize_pending(&new, window, cx);
        let dock = self.dock_area.clone();

        // Reconcile: keep only outgoing tabs still mounted in the center.
        let live = self.live_center_keys(cx);
        if let Some(map) = self.space_panels.get_mut(&old) {
            map.retain(|k, _| live.contains(k));
        }
        let outgoing: Vec<Arc<dyn PanelView>> = self
            .space_panels
            .get(&old)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default();
        let incoming = self.incoming_ordered(&new);

        dock.update(cx, |area, cx| {
            for panel in &outgoing {
                area.remove_panel(panel.clone(), DockPlacement::Center, window, cx);
            }
            for panel in incoming {
                area.add_panel(panel, DockPlacement::Center, None, window, cx);
            }
        });
        self.current_space = new;
        // The mounted set changed (and the outgoing space was reconciled against the
        // live keys) — persist so a crash keeps the just-switched layout.
        self.persist_open_tabs_if_changed(cx);
    }

    /// A space's center panels ordered for mounting: the remembered last-active tab
    /// (`space_active_tab`) goes **last** so `add_panel` — which activates whatever it
    /// adds last — lands it frontmost. Everything else keeps arbitrary `HashMap` order
    /// (it always did). With no remembered tab, this is the previous behavior verbatim.
    fn incoming_ordered(&self, space: &Option<SpaceId>) -> Vec<Arc<dyn PanelView>> {
        let Some(map) = self.space_panels.get(space) else {
            return Vec::new();
        };
        let active = self.space_active_tab.get(space);
        let mut entries: Vec<(&String, &Arc<dyn PanelView>)> = map.iter().collect();
        // `false` (0) sorts before `true` (1), so the active key ends up last. `sort_by_key`
        // is stable, so the non-active tabs keep their iteration order.
        entries.sort_by_key(|(k, _)| active == Some(*k));
        entries.into_iter().map(|(_, p)| p.clone()).collect()
    }

    /// Make the space rooted at `root` active (if such a space is open), mounting its
    /// tab set and re-rooting the rails. Used to land a session's auto-opened gate in
    /// its own project. No-op if no space matches `root` or it's already active.
    fn activate_space_for_root(
        &mut self,
        root: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = self.project_space(cx);
        let Some(id) = focus.read(cx).space_id_for_root(root) else {
            return;
        };
        if self.current_space.as_ref() == Some(&id) {
            return;
        }
        // Swap the mounted tab set now (deterministic, before the caller scopes its
        // tab), then mark the space active so the rails re-root + the tab bar updates.
        // `switch_space` is idempotent, so the focus observer's own call is a no-op.
        self.switch_space(Some(id.clone()), window, cx);
        focus.update(cx, |s, cx| {
            s.select_space(id);
            cx.notify();
        });
    }

    /// The keys of center tabs currently mounted (from a live `dump()`), so a switch
    /// can tell which tracked tabs were closed in the meantime.
    fn live_center_keys(&self, cx: &App) -> HashSet<String> {
        let state = self.dock_area.read(cx).dump(cx);
        let mut keys = HashSet::new();
        collect_panel_keys(&state.center, &mut keys);
        keys
    }

    /// The shared project space (held in `ShellDeps`).
    fn project_space(&self, cx: &App) -> Entity<ProjectSpace> {
        cx.global::<ShellDeps>().focus.clone()
    }

    /// Switch the active space (top-tab click). The rails + fleet observe the space
    /// and re-root / re-scope.
    pub(crate) fn select_space(&mut self, id: SpaceId, cx: &mut Context<Self>) {
        self.project_space(cx).update(cx, |s, cx| {
            s.select_space(id);
            cx.notify();
        });
    }

    /// Switch to the global Overview (no active space; whole fleet, rails at cwd).
    pub(crate) fn select_overview(&mut self, cx: &mut Context<Self>) {
        self.project_space(cx).update(cx, |s, cx| {
            s.select_overview();
            cx.notify();
        });
    }

    /// **Toggle** the overview from the rail's grid tool: overview ⇄ the space you were
    /// on (or the first open space). See [`ProjectSpace::toggle_overview`].
    pub(crate) fn toggle_overview(&mut self, cx: &mut Context<Self>) {
        self.project_space(cx).update(cx, |s, cx| {
            s.toggle_overview();
            cx.notify();
        });
    }

    // ── Main-toolbar dropdowns ────────────────────────────────────────────────
    // At most one of the three selector menus is open; opening one closes the others.

    /// Close every open toolbar dropdown (after an item is chosen, or to swap menus).
    pub(crate) fn close_toolbar_menus(&mut self, cx: &mut Context<Self>) {
        if self.space_menu_open || self.branch_menu_open || self.run_menu_open {
            self.space_menu_open = false;
            self.branch_menu_open = false;
            self.run_menu_open = false;
            cx.notify();
        }
    }

    pub(crate) fn toggle_space_menu(&mut self, cx: &mut Context<Self>) {
        let open = !self.space_menu_open;
        self.close_toolbar_menus(cx);
        self.space_menu_open = open;
        cx.notify();
    }

    pub(crate) fn toggle_branch_menu(&mut self, cx: &mut Context<Self>) {
        let open = !self.branch_menu_open;
        self.close_toolbar_menus(cx);
        self.branch_menu_open = open;
        cx.notify();
    }

    pub(crate) fn toggle_run_menu(&mut self, cx: &mut Context<Self>) {
        let open = !self.run_menu_open;
        self.close_toolbar_menus(cx);
        self.run_menu_open = open;
        cx.notify();
    }

    /// Select the active run target (the run config's command id). Persisted only in
    /// memory — the default is re-derived per space from [`run_config::detect`].
    pub(crate) fn set_run_target(&mut self, id: String, cx: &mut Context<Self>) {
        self.run_target = Some(id);
        cx.notify();
    }

    /// Launch the active run target: open the bottom dock (its terminal is the run
    /// surface) and send the command. The command id *is* the shell command.
    pub(crate) fn run_active_target(&mut self, cx: &mut Context<Self>) {
        let root = self.project_space(cx).read(cx).root();
        let configs = crate::views::run_config::detect(&root);
        if configs.is_empty() {
            return;
        }
        // Resolve the selected target, falling back to the first detected one.
        let config = self
            .run_target
            .as_ref()
            .and_then(|id| configs.iter().find(|c| c.id() == id))
            .or_else(|| configs.first());
        let Some(config) = config else { return };
        // Launch as a captured run in the Run console (the JetBrains Run window) —
        // not typed into the operator's interactive Terminal tab.
        let registry = cx.global::<ShellDeps>().run_registry.clone();
        if let Err(err) = registry.start(&config.label, &config.command, root) {
            cx.global::<ShellDeps>()
                .notifications
                .clone()
                .update(cx, |n, cx| {
                    n.push(NotificationKind::Error, format!("Run failed: {err}"), None);
                    cx.notify();
                });
        }
        self.select_bottom_tool(BottomTool::Run, cx);
    }

    /// Stop the toolbar's **selected target's** run (the ⏹ — the play⇄stop swap's
    /// other half). Other targets' runs keep going; their onglets stay live.
    pub(crate) fn stop_active_target(&mut self, cx: &mut Context<Self>) {
        let root = self.project_space(cx).read(cx).root();
        let configs = crate::views::run_config::detect(&root);
        let command = self
            .run_target
            .as_ref()
            .and_then(|id| configs.iter().find(|c| c.id() == id))
            .or_else(|| configs.first())
            .map(|c| c.command.clone());
        let Some(command) = command else { return };
        let registry = cx.global::<ShellDeps>().run_registry.clone();
        if let Some(id) = registry.find_by_command(&command) {
            registry.stop(id);
        }
        cx.notify();
    }

    pub(crate) fn open_create_run_config_modal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_create_run_config = true;
        let label_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. Run Production"));
        let command_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. cargo run --release"));

        // Focus the name/label input immediately
        label_input.focus_handle(cx).focus(window, cx);

        self.run_config_label_input = Some(label_input);
        self.run_config_command_input = Some(command_input);
        self.run_config_kind = RunKind::Run;
        cx.notify();
    }

    pub(crate) fn close_create_run_config_modal(&mut self, cx: &mut Context<Self>) {
        self.show_create_run_config = false;
        self.run_config_label_input = None;
        self.run_config_command_input = None;
        cx.notify();
    }

    pub(crate) fn save_create_run_config(&mut self, cx: &mut Context<Self>) {
        let label = self
            .run_config_label_input
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_string())
            .unwrap_or_default();
        let command = self
            .run_config_command_input
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_string())
            .unwrap_or_default();
        let kind = self.run_config_kind;

        if label.is_empty() || command.is_empty() {
            cx.global::<ShellDeps>()
                .notifications
                .clone()
                .update(cx, |n, cx| {
                    n.push(
                        NotificationKind::Error,
                        "Name and Command cannot be empty".to_string(),
                        None,
                    );
                    cx.notify();
                });
            return;
        }

        let root = self.project_space(cx).read(cx).root();
        let mut custom_configs = crate::views::run_config::load_custom(&root);

        // Add or replace the custom configuration by label
        let new_config = RunConfig::new(&label, kind, &command);
        custom_configs.retain(|c| c.label != label);
        custom_configs.push(new_config);

        if let Err(err) = crate::views::run_config::save_custom(&root, &custom_configs) {
            cx.global::<ShellDeps>()
                .notifications
                .clone()
                .update(cx, |n, cx| {
                    n.push(
                        NotificationKind::Error,
                        format!("Failed to save run configs: {err}"),
                        None,
                    );
                    cx.notify();
                });
            return;
        }

        // Set the saved config as the active target
        self.run_target = Some(command);

        cx.global::<ShellDeps>()
            .notifications
            .clone()
            .update(cx, |n, cx| {
                n.push(
                    NotificationKind::Phase,
                    format!("Saved configuration '{label}'"),
                    None,
                );
                cx.notify();
            });

        self.close_create_run_config_modal(cx);
    }

    pub(crate) fn set_run_config_kind(&mut self, kind: RunKind, cx: &mut Context<Self>) {
        self.run_config_kind = kind;
        cx.notify();
    }

    /// Check out a git branch from the toolbar's branch dropdown. Runs `git checkout`
    /// off the UI thread; on failure, surfaces the git error as a notification.
    pub(crate) fn checkout_branch(&mut self, branch: String, cx: &mut Context<Self>) {
        let root = self.project_space(cx).read(cx).root();
        let notifications = cx.global::<ShellDeps>().notifications.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn({
                    let root = root.clone();
                    let branch = branch.clone();
                    async move { crate::views::git_info::checkout(&root, &branch) }
                })
                .await;
            // Record the op on the shared git console (the Git tool window's
            // Console view) — success and failure both.
            let _ = this.update(cx, |_, cx| {
                let as_string = match &result {
                    Ok(()) => Ok(String::new()),
                    Err(e) => Err(e.clone()),
                };
                cx.global::<ShellDeps>()
                    .git_console
                    .record(format!("checkout {branch}"), &as_string);
            });
            match result {
                Ok(()) => {
                    // WeakEntity update — Err just means the workspace is gone.
                    let _ = this.update(cx, |_, cx| cx.notify());
                }
                Err(err) => {
                    notifications.update(cx, |n, cx| {
                        n.push(
                            NotificationKind::Error,
                            format!("Checkout {branch} failed: {err}"),
                            None,
                        );
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    /// Close (forget) a space (top-tab "×"). Leaves files + `.moonlight/` intact.
    pub(crate) fn remove_space(&mut self, id: SpaceId, cx: &mut Context<Self>) {
        self.project_space(cx).update(cx, |s, cx| {
            s.remove_space(&id);
            cx.notify();
        });
    }

    /// "＋ Session": quick-launch a managed CC session from the tab bar (no trip to
    /// the Sessions grid). Mints a fresh id and opens it in the **Discovery** mode (the
    /// safe default; switch live from the session card). Mirrors GridHome's ＋New.
    pub(crate) fn new_session(&mut self, cx: &mut Context<Self>) {
        let center = cx.global::<ShellDeps>().center.clone();
        let id = SessionId::new(uuid::Uuid::new_v4().to_string());
        center.update(cx, |_center, cx| {
            cx.emit(OpenRequest::NewManagedSession {
                id,
                phase: Phase::Discovery,
                agent: moonlight_domain::AgentKind::ClaudeCode,
            });
        });
    }

    /// "＋ New Space": open the native folder picker; the chosen folder becomes a new
    /// active space (the only way a space is created — IntelliJ "Open Project").
    pub(crate) fn new_space(&mut self, cx: &mut Context<Self>) {
        let space = self.project_space(cx);
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(SharedString::from("New Space")),
        });
        cx.spawn(async move |_this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(path) = paths.into_iter().next() {
                    space.update(cx, |s, cx| {
                        s.open_root(path);
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    /// Open (or bring to front) a center tab for `request`. Dedups by key:
    /// re-opening the same file/session brings its existing tab to front **without
    /// rebuilding it** — remove + re-add the *same* panel `Arc` (gpui-component has
    /// no public "activate existing tab"). Reusing the entity preserves live state
    /// such as a managed session's embedded terminal (its PTY keeps running).
    fn open_in_center(
        &mut self,
        request: OpenRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A SQLite file open isn't a center tab: it adds the file as a data source in the
        // DB overview tool window (right dock) and fronts it.
        if let OpenRequest::Db(path) = &request {
            self.add_db_source(DataSource::Sqlite(path.clone()), window, cx);
            return;
        }

        // Auto-opened session gates (plan / code review) belong to the *emitting
        // session's* project — switch to that space first so the tab lands there, not
        // in whatever space happens to be live. No-op if there's no open space for the
        // root or it is already active.
        if let Some(root) = request.target_root() {
            self.activate_space_for_root(&root, window, cx);
        }

        let key = request.key();
        let dock = self.dock_area.clone();
        // Scope this tab to the space it is opened in (now the session's, if switched).
        let space = self.current_space.clone();

        if let Some(existing) = self
            .space_panels
            .get(&space)
            .and_then(|m| m.get(&key))
            .cloned()
        {
            dock.update(cx, |area, cx| {
                area.remove_panel(existing.clone(), DockPlacement::Center, window, cx);
                area.add_panel(existing, DockPlacement::Center, None, window, cx);
            });
            return;
        }

        let deps = cx.global::<ShellDeps>().clone();
        let panel: Arc<dyn PanelView> = match request {
            OpenRequest::File(path) => {
                Arc::new(cx.new(|cx| CodeEditorPanel::open(path, window, cx)))
            }
            OpenRequest::Session(session) => {
                let id = session.id.clone();
                // Importing a session into the cockpit governs it: auto-adopt so the
                // PDP gates it from now on (idempotent — a no-op if already adopted).
                let _ = deps.commands.send(Command::SetAdopted {
                    session: id.clone(),
                    adopted: true,
                });
                // Take over an observed session: resume it in a terminal (gated to
                // idle/done inside `new_observed`). Needs the session's repo to root
                // the terminal; without one, the read-only transcript is shown.
                match session.attached_path.as_deref().map(expand_home) {
                    Some(root) => Arc::new(cx.new(|cx| {
                        SessionMonitor::new_observed(
                            id,
                            Some(session),
                            root,
                            deps.bus.subscribe(),
                            cx,
                        )
                    })),
                    None => Arc::new(cx.new(|cx| {
                        SessionMonitor::new(id, Some(session), deps.bus.subscribe(), cx)
                    })),
                }
            }
            OpenRequest::PlanReview { session, plan, .. } => Arc::new(cx.new(|cx| {
                // Live proposal: the session is paused on its plan, so open with the
                // approve/reject buttons armed.
                PlanReviewPanel::new(
                    session,
                    plan,
                    true,
                    Some(deps.commands.clone()),
                    deps.bus.subscribe(),
                    cx,
                )
            })),
            OpenRequest::CodeReview {
                session,
                root,
                summary,
            } => Arc::new(cx.new(|cx| {
                CodeReviewPanel::new(session, root, summary, Some(deps.commands.clone()), cx)
            })),
            OpenRequest::NewManagedSession { id, phase, agent } => {
                // Launch the agent in the phase's permission mode: Plan ⇒ `--permission-mode
                // plan`, everything else ⇒ `auto` (Discovery's no-edit posture is our
                // PDP's job, not the CLI's). `mode` is the derived native binary, stored
                // on the record/roster. The backend (`claude` / `agy`) owns the command shape.
                let mode = phase.operator_mode();
                // Stand the session's embedded MCP endpoint up so the agent gets the
                // `moonlight` actor verbs from its very first turn.
                let mcp = deps.mcp_host.as_ref().and_then(|h| h.url_for(&id));
                let backend = crate::agent_backend::backend_for(agent);
                // Backend-specific pre-launch side effects (AGY writes its mcp_config so
                // `/mcp` exposes the moonlight verbs; Claude injects via the launch flag).
                backend.prepare_launch(mcp.as_deref());
                let command = crate::agent_backend::wrap_statusline(
                    agent,
                    backend.launch_command(&crate::agent_backend::LaunchSpec {
                        selector: crate::agent_backend::SessionSelector::Fresh(&id),
                        permission_mode: Some(phase.cc_permission_mode()),
                        mcp_url: mcp.as_deref(),
                    }),
                );
                let root = deps.focus.read(cx).root();
                // Record managed identity so a restart re-resumes this as a MANAGED
                // session (embedded terminal), not a read-only observed one.
                let now = now_ms();
                let record = ManagedSession {
                    id: id.clone(),
                    root: Some(root.to_string_lossy().into_owned()),
                    // No title yet — detection persists CC's name once observed.
                    title: None,
                    mode,
                    phase,
                    agent,
                    // App-created sessions are auto-adopted: the cockpit launched them,
                    // so they're governed from the first tool call (the engine seeds
                    // `adopted` from this record when detection discovers the session).
                    adopted: true,
                    paused: false,
                    phase_pinned: false,
                    hidden: false,
                    created_at: now,
                    last_seen: now,
                };
                if let Err(err) = deps.store.upsert_managed(&record) {
                    tracing::warn!(error = %err, "failed to record managed session");
                }
                // Also record it into the space's repo-local roster
                // (`<root>/.moonlight/sessions.json`) so the spaces rail lists +
                // resumes it after a restart. Updates the in-memory space too, so
                // the rail reflects the new session immediately.
                deps.focus.update(cx, |ps, cx| {
                    ps.record_managed_session(
                        root.clone(),
                        SpaceSession {
                            id: id.clone(),
                            mode,
                            phase,
                            label: None,
                        },
                    );
                    cx.notify();
                });
                // Trust-on-open: the first session launched under a project asks the
                // operator whether to trust it (then remembers the answer); the choice
                // is applied to this session and re-applied to future ones.
                maybe_prompt_project_trust(&deps, id.clone(), root.clone(), window, cx);
                Arc::new(cx.new(|cx| {
                    SessionMonitor::new_managed(
                        id,
                        None,
                        root,
                        command,
                        agent,
                        phase,
                        deps.bus.subscribe(),
                        cx,
                    )
                }))
            }
            OpenRequest::SessionById { id, root } => {
                // Click-to-navigate from a status-bar notification: reconstruct the
                // monitor from the managed store (mirrors the layout-restore path) —
                // resume it if managed, else show its read-only transcript. Pure view:
                // no adoption change. (Dedups by `session:{id}`, so an open tab fronts.)
                let managed = deps.store.managed(&id).ok().flatten();
                match managed {
                    Some(rec) => {
                        let root = rec
                            .root
                            .map(PathBuf::from)
                            .or(root)
                            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                        let mcp = deps.mcp_host.as_ref().and_then(|h| h.url_for(&id));
                        let command = attach_command(&id, rec.agent, rec.phase, mcp.as_deref());
                        Arc::new(cx.new(|cx| {
                            SessionMonitor::new_managed(
                                id,
                                None,
                                root,
                                command,
                                rec.agent,
                                rec.phase,
                                deps.bus.subscribe(),
                                cx,
                            )
                        }))
                    }
                    None => match root {
                        Some(root) => Arc::new(cx.new(|cx| {
                            SessionMonitor::new_observed(id, None, root, deps.bus.subscribe(), cx)
                        })),
                        None => Arc::new(
                            cx.new(|cx| SessionMonitor::new(id, None, deps.bus.subscribe(), cx)),
                        ),
                    },
                }
            }
            // Routed to the DB overview tool window above, never reaches the center match.
            OpenRequest::Db(_) => unreachable!("Db is handled before the center match"),
            OpenRequest::DbTable { source, table } => {
                Arc::new(cx.new(|cx| DbGridPanel::new(source, table, cx)))
            }
            OpenRequest::DbConsole { source } => {
                Arc::new(cx.new(|cx| DbConsolePanel::new(source, cx)))
            }
            OpenRequest::Http => Arc::new(cx.new(super::panels::http_panel::HttpPanel::new)),
        };

        dock.update(cx, |area, cx| {
            area.add_panel(panel.clone(), DockPlacement::Center, None, window, cx);
        });
        self.space_panels
            .entry(space)
            .or_default()
            .insert(key, panel);
        // Persist immediately so a crash right after opening a tab still restores it
        // (the periodic saver would otherwise be the only writer until on-quit).
        self.persist_open_tabs_if_changed(cx);
    }
}

/// Recursively collect the `OpenRequest::key` of every center tab in a dumped
/// [`PanelState`] tree (`GridHome` and unknown panels are skipped). Used to detect
/// which tracked tabs are still mounted vs. closed by the operator.
fn collect_panel_keys(panel: &PanelState, out: &mut HashSet<String>) {
    if let Some(key) = panel_state_key(panel) {
        out.insert(key);
    }
    for child in &panel.children {
        collect_panel_keys(child, out);
    }
}

/// Reconstruct a center panel's `OpenRequest::key` from its dumped state (matching
/// [`OpenRequest::key`]); `None` for `GridHome` and any non-dynamic panel.
fn panel_state_key(panel: &PanelState) -> Option<String> {
    match panel.panel_name.as_str() {
        "CodeEditor" => panel_info_str(&panel.info, "path").map(|p| format!("file:{p}")),
        "SessionMonitor" => panel_info_str(&panel.info, "session").map(|s| format!("session:{s}")),
        "PlanReview" => panel_info_str(&panel.info, "session").map(|s| format!("plan:{s}")),
        "CodeReview" => panel_info_str(&panel.info, "session").map(|s| format!("review:{s}")),
        // The precomputed `key` matches `OpenRequest::Db(path).key()` for SQLite, so a
        // file-tree open and a restored tab collapse onto one tab. Legacy layouts
        // (path only) fall back to the same `db:{path}` form.
        "DbObserver" => panel_info_str(&panel.info, "key")
            .filter(|k| !k.is_empty())
            .or_else(|| panel_info_str(&panel.info, "path").map(|p| format!("db:{p}"))),
        _ => None,
    }
}

/// Rebuild a center panel from its persisted dedup `key` (the inverse of
/// [`OpenRequest::key`] / [`panel_state_key`]), for the open-tabs restore. Mirrors the
/// per-kind construction in [`Workspace::open_in_center`] but as a pure view — a
/// `session:` tab resumes a managed session in its embedded terminal (else shows the
/// read-only transcript) **without** changing adoption. `space` roots the session/review
/// when the managed record carries no root. `None` for an unknown key.
/// Rebuild a center tab from its dedup `key`. `restoring` is `true` only on the
/// IDE-restart restore paths ([`Workspace::restore_open_tabs`] / [`Workspace::realize_pending`]):
/// a managed session rebuilt then, under an auto-resume project, is armed to nudge itself
/// to continue once it comes back up stalled (see [`SessionMonitor::arm_restore_resume`]).
fn build_center_panel(
    deps: &ShellDeps,
    key: &str,
    space: Option<&SpaceId>,
    restoring: bool,
    window: &mut Window,
    cx: &mut App,
) -> Option<Arc<dyn PanelView>> {
    let (kind, rest) = key.split_once(':')?;
    let space_root: Option<PathBuf> = space.map(|s| PathBuf::from(s.as_str()));
    let panel: Arc<dyn PanelView> = match kind {
        "session" => {
            let id = SessionId::new(rest.to_string());
            match deps.store.managed(&id).ok().flatten() {
                Some(rec) => {
                    let root = rec
                        .root
                        .map(PathBuf::from)
                        .or_else(|| space_root.clone())
                        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                    let mcp = deps.mcp_host.as_ref().and_then(|h| h.url_for(&id));
                    let command = attach_command(&id, rec.agent, rec.phase, mcp.as_deref());
                    // Arm the restart auto-resume nudge for a restored managed session whose
                    // project opted in — it fires once the session comes back up stalled.
                    let arm = restoring && deps.focus.read(cx).project_auto_resume(&root);
                    let mon = cx.new(|cx| {
                        SessionMonitor::new_managed(
                            id,
                            None,
                            root,
                            command,
                            rec.agent,
                            rec.phase,
                            deps.bus.subscribe(),
                            cx,
                        )
                    });
                    if arm {
                        mon.update(cx, |m, _| m.arm_restore_resume());
                    }
                    Arc::new(mon)
                }
                None => match space_root {
                    Some(root) => Arc::new(cx.new(|cx| {
                        SessionMonitor::new_observed(id, None, root, deps.bus.subscribe(), cx)
                    })),
                    None => Arc::new(
                        cx.new(|cx| SessionMonitor::new(id, None, deps.bus.subscribe(), cx)),
                    ),
                },
            }
        }
        "file" => Arc::new(cx.new(|cx| CodeEditorPanel::open(PathBuf::from(rest), window, cx))),
        // `db:` is no longer a center tab — databases live in the DB overview tool window,
        // and the data-editor / console tabs (`dbtable:` / `dbconsole:`) are transient, so
        // they fall through to `None` (re-opened from the tree, not restored across runs).
        "plan" => {
            let id = SessionId::new(rest.to_string());
            Arc::new(cx.new(|cx| {
                PlanReviewPanel::new(
                    id,
                    "(plan from a previous run — re-trigger to view)".into(),
                    false,
                    Some(deps.commands.clone()),
                    deps.bus.subscribe(),
                    cx,
                )
            }))
        }
        "review" => {
            let id = SessionId::new(rest.to_string());
            Arc::new(cx.new(|cx| {
                CodeReviewPanel::new(id, space_root, None, Some(deps.commands.clone()), cx)
            }))
        }
        _ => return None,
    };
    Some(panel)
}

/// An empty string rendered as an em-dash (status-bar obs placeholder).
fn blank_dash(s: &str) -> String {
    if s.is_empty() {
        "—".to_string()
    } else {
        s.to_string()
    }
}

/// A phase-change notification (kind + text) when the phase actually moved — the
/// "a phase ended / the next began" signal. `None` when `prev == new`.
fn phase_notification(prev: Phase, new: Phase, label: &str) -> Option<(NotificationKind, String)> {
    (prev != new).then(|| {
        (
            NotificationKind::Phase,
            format!("{} → {} · {label}", prev.label(), new.label()),
        )
    })
}

/// A status-change notification for the states worth surfacing: CC needs input, or a
/// session errored. Other statuses (running/idle/done/paused) don't notify here.
fn status_notification(status: SessionStatus, label: &str) -> Option<(NotificationKind, String)> {
    match status {
        SessionStatus::WaitingInput => Some((
            NotificationKind::Input,
            format!("Needs your input · {label}"),
        )),
        SessionStatus::Errored => Some((
            NotificationKind::Error,
            format!("Session errored · {label}"),
        )),
        _ => None,
    }
}

/// Friendly label for a session in a notification: its tracked title, else a short id.
fn notif_label(labels: &HashMap<SessionId, String>, id: &SessionId) -> String {
    labels
        .get(id)
        .cloned()
        .unwrap_or_else(|| id.as_str().chars().take(8).collect())
}

/// Wall-clock now as epoch millis, for stamping a managed-session record's
/// `created_at`/`last_seen` (the domain itself has no clock).
/// Trust-on-open prompt: the first managed session launched under a project asks the
/// operator whether to trust it, then remembers the answer in the project space. The
/// choice is applied to this session (via `SetTrust`, which lands once the engine tracks
/// it) and persisted so later sessions and restarts come up at the chosen tier.
///
/// No-op when (a) there's no open space for `root` to remember against, or (b) the
/// project's trust is already known — restored sessions then pick the tier up from the
/// space on their first sighting (see [`SessionMonitor`]'s upsert fold).
fn maybe_prompt_project_trust(
    deps: &ShellDeps,
    id: SessionId,
    root: PathBuf,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let focus = deps.focus.clone();
    {
        let ps = focus.read(cx);
        if ps.space_id_for_root(&root).is_none() || ps.project_trust(&root).is_some() {
            return;
        }
    }
    let label = root
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| root.display().to_string());
    // Index ↔ tier must match the order below.
    let answers = ["Trust", "Read-only", "Don't trust"];
    let receiver = window.prompt(
        PromptLevel::Info,
        &format!("Trust the project “{label}”?"),
        Some(
            "Trust lets this project's sessions act autonomously at their tier. Read-only \
             permits reads but gates writes. Don't trust gates every action. You can change \
             this anytime from a session's Trust selector.",
        ),
        &answers,
        cx,
    );
    let commands = deps.commands.clone();
    cx.spawn(async move |_this, cx| {
        let Ok(choice) = receiver.await else {
            return;
        };
        let tier = match choice {
            0 => TrustTier::Trusted,
            1 => TrustTier::ReadOnly,
            _ => TrustTier::Observed,
        };
        let _ = focus.update(cx, |ps, _cx| ps.set_project_trust(&root, tier));
        // Apply to the just-launched session; by answer time the engine has usually
        // tracked it. (If not, the monitor re-applies the persisted tier on first sight.)
        let _ = commands.send(Command::SetTrust { session: id, tier });
    })
    .detach();
}

fn now_ms() -> Timestamp {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Timestamp::from_millis(ms)
}

/// Build the center home: the fleet [`GridHome`] wrapped in a split so the center
/// tab panel has a parent stack (gpui-component locks a parentless tab panel, which
/// would make opened file/session tabs neither closable nor draggable).
fn home_center(weak: &WeakEntity<DockArea>, window: &mut Window, cx: &mut App) -> DockItem {
    let deps = cx.global::<ShellDeps>().clone();
    let grid = cx.new(|cx| {
        GridHome::new(
            deps.bus.subscribe(),
            Some(deps.focus.clone()),
            Some(deps.commands.clone()),
            Some(deps.store.clone()),
            cx,
        )
    });
    DockItem::split(
        gpui::Axis::Horizontal,
        vec![DockItem::tabs(
            vec![Arc::new(grid) as Arc<dyn PanelView>],
            weak,
            window,
            cx,
        )],
        weak,
        window,
        cx,
    )
}

/// Replace the center with a fresh GridHome-only home (dropping any dynamic tabs a
/// restored layout carried). The rails are left as-is. Used after a successful
/// layout restore so per-space tab tracking starts from a known-empty center.
fn reset_center_to_home(
    dock_area: &Entity<DockArea>,
    weak: WeakEntity<DockArea>,
    window: &mut Window,
    cx: &mut App,
) {
    let center = home_center(&weak, window, cx);
    dock_area.update(cx, |area, cx| area.set_center(center, window, cx));
}

/// (Re)build the left dock around the workspace-owned tool windows, fronting
/// `left_tool`: **Project** = the file tree with the Structure outline stacked
/// below it when `structure_open`; **Commit** = the commit tool. Used for the
/// default layout, after a layout restore (so the dock always wraps the owned
/// entities), and on every Structure toggle / Project⇄Commit swap (the dock's
/// shape changes).
#[allow(clippy::too_many_arguments)]
fn set_left_tools(
    weak: &WeakEntity<DockArea>,
    tree: &Entity<FileTreePanel>,
    structure: &Entity<StructurePanel>,
    commit: &Entity<super::panels::commit::CommitPanel>,
    left_tool: LeftTool,
    structure_open: bool,
    size: Option<Pixels>,
    open: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let items = match left_tool {
        LeftTool::Project => {
            let mut items = vec![DockItem::tab(tree.clone(), weak, window, cx)];
            if structure_open {
                items.push(DockItem::tab(structure.clone(), weak, window, cx).size(px(220.)));
            }
            items
        }
        LeftTool::Commit => vec![DockItem::tab(commit.clone(), weak, window, cx)],
    };
    let left = DockItem::split(gpui::Axis::Vertical, items, weak, window, cx);
    let _ = weak.update(cx, |area, cx| {
        area.set_left_dock(left, size, open, window, cx);
    });
}

/// Build the default layout: center fleet + left project rail (tree over structure).
/// The bottom dock (terminal) is **not** part of the `DockArea` — it's composed at the
/// workspace level so it spans the full width beneath the left dock (see [`Workspace`]).
fn reset_default_layout(
    weak: WeakEntity<DockArea>,
    tree: &Entity<FileTreePanel>,
    structure: &Entity<StructurePanel>,
    commit: &Entity<super::panels::commit::CommitPanel>,
    window: &mut Window,
    cx: &mut App,
) {
    let center = home_center(&weak, window, cx);
    let _ = weak.update(cx, |area, cx| {
        area.set_version(DOCK_VERSION, window, cx);
        area.set_center(center, window, cx);
        // Left/right rails collapse to their edge; the bottom dock is workspace-owned.
        area.set_dock_collapsible(
            Edges {
                left: true,
                right: true,
                bottom: false,
                top: false,
            },
            window,
            cx,
        );
    });
    // Left rail: file tree on top, structure outline below (the space switcher now
    // lives in the top tab bar, not the dock).
    set_left_tools(
        &weak,
        tree,
        structure,
        commit,
        LeftTool::Project,
        true,
        Some(px(300.)),
        true,
        window,
        cx,
    );
    let _ = weak.update(cx, |area, cx| {
        if let Err(err) = save_state(&area.dump(cx)) {
            tracing::warn!(error = %err, "failed to save initial dock layout");
        }
    });
}

/// Load and apply a persisted layout. Errors if there is no saved layout, it is
/// unreadable, or its version no longer matches (caller then rebuilds default).
fn load_layout(
    dock_area: &Entity<DockArea>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> anyhow::Result<()> {
    let path = layout_path().context("no layout path available")?;
    let json = std::fs::read_to_string(&path).context("read saved layout")?;
    let state: DockAreaState = serde_json::from_str(&json).context("parse saved layout")?;
    anyhow::ensure!(
        state.version == Some(DOCK_VERSION),
        "saved layout version mismatch"
    );

    dock_area.update(cx, |area, cx| {
        area.load(state, window, cx).context("apply saved layout")?;
        area.set_dock_collapsible(
            Edges {
                left: true,
                right: true,
                bottom: true,
                top: false,
            },
            window,
            cx,
        );
        anyhow::Ok(())
    })
}

/// Serialize the dock layout to the on-disk location (creating its directory).
fn save_state(state: &DockAreaState) -> anyhow::Result<()> {
    let Some(path) = layout_path() else {
        return Ok(()); // No home dir → run with a fresh layout each time.
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create layout dir")?;
    }
    let json = serde_json::to_string_pretty(state).context("serialize layout")?;
    std::fs::write(&path, json).context("write layout")?;
    Ok(())
}

/// macOS-first layout file location under Application Support.
fn layout_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/MoonlightCode/layout.json"))
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Overlay layers host popovers/menus/notifications raised by panels.
        let notification_layer = Root::render_notification_layer(window, cx);
        let sheet_layer = Root::render_sheet_layer(window, cx);
        let dialog_layer = Root::render_dialog_layer(window, cx);

        // Snapshot the spaces into 'static tab data for the top switcher bar.
        let (tabs, overview_active) = {
            let ps = self.project_space(cx);
            let ps = ps.read(cx);
            let active = ps.active().cloned();
            let tabs: Vec<SpaceTab> = ps
                .spaces()
                .iter()
                .map(|s| {
                    let (attention, needs_input) = self.space_attention(&s.root);
                    SpaceTab {
                        id: s.id.clone(),
                        label: s.label.clone(),
                        active: active.as_ref() == Some(&s.id),
                        attention,
                        needs_input,
                    }
                })
                .collect();
            (tabs, active.is_none())
        };

        // Snapshot the activity rail's lit states (dock open flags + frontmost
        // center surface).
        let bottom_open = self.bottom_open;
        let left_open = self
            .dock_area
            .read(cx)
            .left_dock()
            .is_some_and(|d| d.read(cx).is_open());
        let db_open = self
            .dock_area
            .read(cx)
            .right_dock()
            .is_some_and(|d| d.read(cx).is_open());
        let rail = RailSnapshot {
            overview_active,
            left_open,
            left_tool: self.left_tool,
            // Structure is "on" when it's actually visible: dock open, Project
            // fronted, outline shown.
            structure_open: left_open && self.left_tool == LeftTool::Project && self.structure_open,
            bottom_open,
            bottom_tool: self.bottom_tool,
            // The Run stripe button's lamp: live blue while anything runs, then the
            // last run's verdict — glanceable even with the dock closed.
            run_lamp: cx
                .global::<ShellDeps>()
                .run_registry
                .overall_status()
                .map(|s| super::panels::run_console::RunConsolePanel::status_color(&s)),
            // The Problems lamp: red on errors / amber on warnings in open files.
            problems_lamp: self.problems.read(cx).counts().lamp(),
        };

        // Snapshot the main toolbar (titlebar): the active space + branch selectors,
        // the run target, and which dropdown is open — all owned so handlers stay
        // `'static`. Branch list / run-config rows are filled only while their menu is
        // open (each costs a `git`/fs probe). The active run target falls back to the
        // first detected config when the operator hasn't picked one.
        let toolbar = {
            let ps = self.project_space(cx);
            let ps = ps.read(cx);
            let root = ps.root();
            let active = ps.active().cloned();
            let project = active
                .as_ref()
                .and_then(|id| ps.spaces().iter().find(|s| &s.id == id))
                .map(|s| s.label.clone())
                .unwrap_or_else(|| "Overview".to_string());
            let branch = crate::views::git_info::current_branch(&root);
            let spaces: Vec<toolbar::SpaceRow> = ps
                .spaces()
                .iter()
                .map(|s| toolbar::SpaceRow {
                    id: s.id.clone(),
                    label: s.label.clone(),
                    active: active.as_ref() == Some(&s.id),
                })
                .collect();
            let branches = if self.branch_menu_open {
                crate::views::git_info::local_branches(&root)
            } else {
                Vec::new()
            };
            let configs = crate::views::run_config::detect(&root);
            let active_cfg = self
                .run_target
                .as_ref()
                .and_then(|id| configs.iter().find(|c| c.id() == id))
                .or_else(|| configs.first());
            let run_label = active_cfg.map(|c| c.label.clone());
            let run_glyph = active_cfg.map(|c| c.kind.glyph()).unwrap_or("▶");
            let active_id = active_cfg.map(|c| c.id().to_string());
            let run_configs: Vec<toolbar::RunRow> = if self.run_menu_open {
                configs
                    .iter()
                    .map(|c| toolbar::RunRow {
                        id: c.id().to_string(),
                        label: c.label.clone(),
                        glyph: c.kind.glyph(),
                        active: active_id.as_deref() == Some(c.id()),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            toolbar::ToolbarSnapshot {
                project,
                branch,
                space_menu_open: self.space_menu_open,
                branch_menu_open: self.branch_menu_open,
                run_menu_open: self.run_menu_open,
                left_open,
                spaces,
                branches,
                run_label,
                run_glyph,
                run_running: active_cfg
                    .map(|c| {
                        cx.global::<ShellDeps>()
                            .run_registry
                            .command_running(c.id())
                    })
                    .unwrap_or(false),
                run_configs,
                auto_phase: cx
                    .global::<ShellDeps>()
                    .auto_phase
                    .load(std::sync::atomic::Ordering::Relaxed),
            }
        };

        // Snapshot the status-bar data into owned values (frontmost file/session
        // context + active project + theme), so the bar render fn stays `'static`.
        let status = {
            let root = self.project_space(cx).read(cx).root();
            let project = root
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
                .unwrap_or_else(|| "—".to_string());
            let ac = cx.global::<ShellDeps>().active_context.read(cx).clone();
            // The selected session drives the right-zone obs (per the "on selected
            // session" rule); `None` when a file/grid is frontmost.
            let active_session = match &ac {
                ActiveContext::Session { id, .. } => Some(id.clone()),
                _ => None,
            };
            // The Claude-observability cluster (model/quota/ctx) only applies to a Claude
            // session. An AGY session (looked up from the managed store) doesn't feed it,
            // so the bar shows an "AGY" marker instead of misleading Claude stats. No
            // session / unknown ⇒ keep the cluster (account quota still relevant).
            let obs_native = active_session
                .as_ref()
                .and_then(|id| cx.global::<ShellDeps>().store.managed(id).ok().flatten())
                .map(|m| m.agent != moonlight_domain::AgentKind::Antigravity)
                .unwrap_or(true);
            let left = match ac {
                ActiveContext::None => LeftZone::Empty,
                ActiveContext::File {
                    path,
                    line,
                    column,
                    eol,
                    indent,
                } => LeftZone::File {
                    rel_path: path
                        .strip_prefix(&root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .into_owned(),
                    caret: format!("Ln {line}:{column}"),
                    eol,
                    encoding: "UTF-8",
                    indent: indent.label(),
                },
                ActiveContext::Session {
                    label,
                    status,
                    phase,
                    ..
                } => LeftZone::Session {
                    label,
                    status,
                    phase,
                },
            };
            let notif = cx.global::<ShellDeps>().notifications.read(cx);
            let unread = notif.unread();
            let notifications = notif
                .items()
                .iter()
                .map(|n| {
                    let (icon, color) = status_bar::kind_style(n.kind);
                    NotifRow {
                        id: n.id,
                        icon,
                        color,
                        text: n.text.clone(),
                        read: n.read,
                        session: n.session.clone(),
                    }
                })
                .collect();
            // Account quota (5h / weekly / Sonnet-weekly) + per-session obs, both from
            // the obs read-model.
            let (quota, obs) = {
                let store = cx.global::<ShellDeps>().obs_store.read(cx);
                let quota = store.quota().cloned().unwrap_or_default();
                let quota = QuotaView {
                    five_h: quota.five_hour_pct,
                    five_h_reset: quota.five_hour_resets_at.and_then(crate::obs::fmt_reset_in),
                    weekly: quota.weekly_pct,
                    sonnet: quota.sonnet_pct,
                };
                let obs = active_session.as_ref().and_then(|id| {
                    store.get(id).map(|o| ObsView {
                        model: blank_dash(&o.model),
                        time: crate::obs::fmt_dur(o.session_ms),
                        ctx_pct: o.ctx_pct(),
                        persona: blank_dash(&o.persona),
                    })
                });
                (quota, obs)
            };
            StatusSnapshot {
                left,
                project,
                theme_name: "Moonlight",
                notifications_open: self.notifications_open,
                notifications,
                unread,
                quota,
                obs,
                obs_native,
            }
        };

        div()
            .id("moonlight-workspace")
            .relative()
            .flex()
            .flex_col()
            .size_full()
            // While the bottom dock is being resized, track the pointer window-wide.
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, cx| {
                if let Some((grab_y, grab_h)) = this.resize_anchor {
                    // Dragging the grip upward (smaller y) grows the bottom dock.
                    let dy = grab_y - f32::from(ev.position.y);
                    this.bottom_height = (grab_h + dy).clamp(120., 640.);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _ev: &MouseUpEvent, _w, cx| {
                    if this.resize_anchor.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            // Row 1: the main toolbar, hosted in the custom titlebar at the macOS
            // traffic-light level (project + branch selectors, run widget, actions).
            .child(toolbar::main_toolbar(toolbar, cx))
            // Row 2: the project-space tab bar (Overview + open spaces).
            .child(space_tab_bar(tabs, overview_active, cx))
            // Below: the activity rail (full height) beside the working area. The
            // working area is a column of [dock = left rail + center] over the
            // **full-width** bottom dock, so the bottom dock (terminal / run logs /
            // services) gets the whole width beneath the left rail.
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(activity_rail(rail, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().flex_1().min_h_0().child(self.dock_area.clone()))
                            .when(self.bottom_open, |d| d.child(self.bottom_dock(cx))),
                    )
                    // The right stripe (DB observer today; more right tools later).
                    .child(super::panels::activity_rail::right_stripe(db_open, cx)),
            )
            // Bottom: the JetBrains-style status bar.
            .child(status_bar::status_bar(status, cx))
            .when(self.show_create_run_config, |d| {
                if let Some(modal) = self.render_create_run_config_modal(window, cx) {
                    d.child(modal)
                } else {
                    d
                }
            })
            .children(sheet_layer)
            .children(dialog_layer)
            .children(notification_layer)
    }
}

impl Workspace {
    fn render_create_run_config_modal(
        &self,
        _window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Option<impl IntoElement> {
        if !self.show_create_run_config {
            return None;
        }

        let label_input = self.run_config_label_input.as_ref()?;
        let command_input = self.run_config_command_input.as_ref()?;
        let active_kind = self.run_config_kind;

        // Render the backdrop spanning the entire window, catching clicks to close
        let backdrop = div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(theme::tint(theme::surface_void(), 0.65))
            .flex()
            .items_center()
            .justify_center()
            .child(
                // The centered modal box
                div()
                    .w(px(460.))
                    .p_5()
                    .rounded(theme::radius_md())
                    .bg(theme::surface_overlay())
                    .border_1()
                    .border_color(theme::border_strong())
                    .shadow(theme::overlay_shadow())
                    .flex()
                    .flex_col()
                    .gap_4()
                    // Modal Title
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(theme::text_lg())
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme::text_primary())
                                    .child("Create Run Configuration"),
                            )
                            .child(
                                // Close "X" button
                                div()
                                    .id("close-run-config-modal")
                                    .cursor_pointer()
                                    .text_size(theme::text_sm())
                                    .text_color(theme::text_muted())
                                    .hover(|d| d.text_color(theme::text_primary()))
                                    .child("✕")
                                    .on_click(cx.listener(|this, _ev, _w, cx| {
                                        this.close_create_run_config_modal(cx);
                                    })),
                            ),
                    )
                    // Config Name Input
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(theme::text_xs())
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::text_secondary())
                                    .child("Configuration Name"),
                            )
                            .child(Input::new(label_input)),
                    )
                    // Config Command Input
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(theme::text_xs())
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::text_secondary())
                                    .child("Shell Command"),
                            )
                            .child(Input::new(command_input)),
                    )
                    // Run Kind Selector
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1_5()
                            .child(
                                div()
                                    .text_size(theme::text_xs())
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::text_secondary())
                                    .child("Configuration Type"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap_2()
                                    .child(self.render_kind_btn(
                                        RunKind::Run,
                                        "▶ Run",
                                        active_kind == RunKind::Run,
                                        cx,
                                    ))
                                    .child(self.render_kind_btn(
                                        RunKind::Build,
                                        "⚒ Build",
                                        active_kind == RunKind::Build,
                                        cx,
                                    ))
                                    .child(self.render_kind_btn(
                                        RunKind::Test,
                                        "✓ Test",
                                        active_kind == RunKind::Test,
                                        cx,
                                    )),
                            ),
                    )
                    // Dialog Footer / Actions (Save, Cancel)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_end()
                            .gap_2()
                            .child(
                                // Cancel Button
                                div()
                                    .id("btn-cancel-run-config")
                                    .cursor_pointer()
                                    .px_3()
                                    .py_1_5()
                                    .rounded(theme::radius_sm())
                                    .text_size(theme::text_sm())
                                    .text_color(theme::text_secondary())
                                    .hover(|d| d.bg(theme::row_hover()))
                                    .child("Cancel")
                                    .on_click(cx.listener(|this, _ev, _w, cx| {
                                        this.close_create_run_config_modal(cx);
                                    })),
                            )
                            .child(
                                // Save Button (Accent Highlight)
                                div()
                                    .id("btn-save-run-config")
                                    .cursor_pointer()
                                    .px_4()
                                    .py_1_5()
                                    .rounded(theme::radius_sm())
                                    .bg(theme::accent())
                                    .hover(|d| d.bg(theme::accent_hover()))
                                    .text_size(theme::text_sm())
                                    .text_color(theme::on_accent())
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Save Configuration")
                                    .on_click(cx.listener(|this, _ev, _w, cx| {
                                        this.save_create_run_config(cx);
                                    })),
                            ),
                    ),
            );

        Some(backdrop)
    }

    fn render_kind_btn(
        &self,
        kind: RunKind,
        label: &'static str,
        active: bool,
        cx: &mut Context<Workspace>,
    ) -> impl IntoElement {
        let bg = if active {
            theme::tint(theme::accent(), 0.14)
        } else {
            theme::surface_raised()
        };
        let border_color = if active {
            theme::accent()
        } else {
            theme::border_subtle()
        };
        let text_color = if active {
            theme::accent()
        } else {
            theme::text_secondary()
        };

        div()
            .id(SharedString::from(format!("kind-btn-{}", label)))
            .flex()
            .items_center()
            .justify_center()
            .flex_1()
            .py_2()
            .rounded(theme::radius_sm())
            .border_1()
            .border_color(border_color)
            .bg(bg)
            .text_color(text_color)
            .text_size(theme::text_sm())
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .hover(|d| if !active { d.bg(theme::row_hover()) } else { d })
            .on_click(cx.listener(move |this, _ev, _w, cx| {
                this.set_run_config_kind(kind, cx);
            }))
            .child(label)
    }
}
