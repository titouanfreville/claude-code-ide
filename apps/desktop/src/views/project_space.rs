//! The **project space** — "what project am I looking at, and how was it chosen".
//!
//! A *space* binds one project **root** to the sessions that live under it. The
//! IDE-classic rails (file tree, structure, terminal) follow a single **active
//! space**; the [`super::panels::spaces`] rail lets the operator switch among the
//! known spaces (or an "overview" with no specific root). A space's root has three
//! sources, in this order of intent:
//!   1. an explicitly **opened folder** (`open_root`) → its space becomes active, or
//!   2. the **focused session**'s `attached_path` — its space becomes active, but
//!      only while `follow_focus` is on (the default; UX spec "follow focused
//!      session", FR8), or
//!   3. the launch working directory (fallback, so the rails are always usable when
//!      no space is active / "overview").
//!
//! Generalized from a single root to `Vec<Space>` + an `active` id so the operator
//! can keep several projects open at once and flip between them; focusing a session
//! switches to *its* space (extending follow-focus from one root to N spaces).
//!
//! Spaces, the active selection, recently-used roots, and the follow-focus toggle
//! persist across restarts (`projects.json`, mirroring the dock-layout persistence
//! in [`super::workspace`]). This is UI-local view state — a tiny GPUI [`Entity`]
//! the workspace creates once and hands to each rail; panels `cx.observe` it and
//! re-root when it changes. It never crosses the engine bus.

use std::path::{Path, PathBuf};

use moonlight_domain::ids::SessionId;
use moonlight_domain::trust::TrustTier;
use serde::{Deserialize, Serialize};

use super::space_sessions::{self, SpaceSession};

/// How many recent project roots to remember.
const RECENT_CAP: usize = 10;

/// Stable identity of a [`Space`], derived from its absolute root path (one space
/// per root). Used as the rail's selection key and element id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpaceId(String);

impl SpaceId {
    /// The space's path string. Read by tests today; a stable public accessor for
    /// any future caller that needs the id without matching on the newtype.
    #[allow(dead_code)]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A project space: a `{root + label + its managed sessions}`. The IDE rails bind
/// to one **active** space at a time. `sessions` is the space's roster of
/// app-managed CC sessions, hydrated from the repo-local
/// `<root>/.moonlight/sessions.json` (see [`super::space_sessions`]) — it is **not**
/// persisted into the global `projects.json` (`#[serde(skip)]`); the durable part
/// there is just the root + label.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Space {
    pub id: SpaceId,
    pub root: PathBuf,
    pub label: String,
    /// App-managed sessions belonging to this space, loaded from the per-space
    /// roster file. Skipped in `projects.json` — the roster file owns it.
    #[serde(skip)]
    pub sessions: Vec<SpaceSession>,
    /// The operator's persisted trust decision for this project (asked once on first
    /// session launch, then remembered). `None` until answered — sessions launch at
    /// the engine default (`Observed`) and the operator is prompted. Once set, every
    /// session under this root is brought to this tier (and the Trust selector persists
    /// changes back here). See [`ProjectSpace::project_trust`].
    #[serde(default)]
    pub trust: Option<TrustTier>,
    /// When on, a session under this project that stalls/stops mid-work is automatically
    /// resumed with a continue prompt (opt-in per project, default off). See
    /// [`ProjectSpace::project_auto_resume`].
    #[serde(default)]
    pub auto_resume: bool,
}

impl Space {
    /// Build a space for `root`, deriving its id (the path) and label (dir name),
    /// and hydrating its managed-session roster from `<root>/.moonlight/sessions.json`.
    fn from_root(root: PathBuf) -> Self {
        let id = SpaceId(root.to_string_lossy().into_owned());
        let label = root
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| root.display().to_string());
        let sessions = space_sessions::load(&root);
        Self {
            id,
            root,
            label,
            sessions,
            trust: None,
            auto_resume: false,
        }
    }
}

pub struct ProjectSpace {
    /// The known spaces, in creation order (the rail renders them in this order).
    spaces: Vec<Space>,
    /// The active space; `None` → the **overview** ("All") view, where
    /// [`ProjectSpace::root`] falls back to the cwd.
    active: Option<SpaceId>,
    /// The space to restore when toggling **out of** the overview (the rail's grid
    /// tool: click → overview, click again → back to where you were). In-memory only
    /// (a session-scoped convenience), not persisted — `active` already survives
    /// restart. See [`Self::toggle_overview`].
    last_space: Option<SpaceId>,
    /// The focused session and the root derived from it (kept so toggling
    /// `follow_focus` back on can re-root onto its space).
    session: Option<SessionId>,
    session_root: Option<PathBuf>,
    /// When on, focusing a session re-roots onto its space.
    follow_focus: bool,
    /// Most-recently-used roots, front = newest, deduped, capped at [`RECENT_CAP`].
    recent: Vec<PathBuf>,
    /// Where to persist (the `projects.json` path). `None` → **in-memory only**, so a
    /// `ProjectSpace` built via [`Default`] (tests) never writes the real user config.
    /// [`Self::load`] sets it to [`state_path`] so the live app persists normally.
    persist: Option<PathBuf>,
}

impl Default for ProjectSpace {
    fn default() -> Self {
        Self {
            spaces: Vec::new(),
            active: None,
            last_space: None,
            session: None,
            session_root: None,
            follow_focus: true,
            recent: Vec::new(),
            persist: None,
        }
    }
}

/// The persisted slice of the project space (spaces + active + recent + follow).
#[derive(Serialize, Deserialize)]
struct PersistState {
    #[serde(default)]
    spaces: Vec<Space>,
    #[serde(default)]
    active: Option<SpaceId>,
    #[serde(default)]
    recent: Vec<PathBuf>,
    #[serde(default = "default_true")]
    follow_focus: bool,
}

fn default_true() -> bool {
    true
}

impl ProjectSpace {
    /// Build with persisted state if a state file exists, else defaults
    /// (follow-focus on, no spaces, overview active). Each space's managed-session
    /// roster is (re)hydrated from its repo-local `<root>/.moonlight/sessions.json`
    /// (it is skipped in `projects.json`), so a space comes back knowing the
    /// sessions the app launched under it.
    pub fn load() -> Self {
        let base = Self {
            persist: state_path(),
            ..Self::default()
        };
        match state_path().and_then(|p| load_from(&p)) {
            Some(state) => {
                let mut spaces = state.spaces;
                for s in &mut spaces {
                    s.sessions = space_sessions::load(&s.root);
                }
                // Drop a dangling `active` that no longer names a known space.
                let active = state.active.filter(|id| spaces.iter().any(|s| &s.id == id));
                Self {
                    spaces,
                    active,
                    follow_focus: state.follow_focus,
                    recent: state.recent,
                    ..base
                }
            }
            None => base,
        }
    }

    /// The active project root: the active space's root, else the launch cwd, else `/`.
    pub fn root(&self) -> PathBuf {
        self.active_space()
            .map(|s| s.root.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    /// The known spaces (rail list).
    pub fn spaces(&self) -> &[Space] {
        &self.spaces
    }

    /// The active space's id, or `None` for the overview.
    pub fn active(&self) -> Option<&SpaceId> {
        self.active.as_ref()
    }

    /// The active space, if one is selected (not overview).
    fn active_space(&self) -> Option<&Space> {
        let id = self.active.as_ref()?;
        self.spaces.iter().find(|s| &s.id == id)
    }

    /// The session the operator last focused, if any. Read by tests today; the UI
    /// will read it for a "focused tile" affordance (keeps the `session` field
    /// live, so this stays honest state rather than dead weight).
    #[allow(dead_code)]
    pub fn session(&self) -> Option<&SessionId> {
        self.session.as_ref()
    }

    pub fn follow_focus(&self) -> bool {
        self.follow_focus
    }

    pub fn recent(&self) -> &[PathBuf] {
        &self.recent
    }

    /// **Open (create) a space** for a folder the operator explicitly chose (the
    /// "＋ New Space" action / file-tree "Open…", IntelliJ "Open Project"): ensure a
    /// space for `path` exists, make it active, record it in `recent`, persist.
    /// Spaces are **only** born here — never auto-materialized from session activity.
    /// Returns whether the active root changed.
    pub fn open_root(&mut self, path: PathBuf) -> bool {
        let changed = self.root() != path;
        let id = self.ensure_space(path.clone());
        self.push_recent(path);
        self.active = Some(id);
        self.save();
        changed
    }

    /// Select an already-known space as active (rail click). Re-roots the rails.
    /// Returns whether the active selection changed; no-op for an unknown id.
    pub fn select_space(&mut self, id: SpaceId) -> bool {
        if !self.spaces.iter().any(|s| s.id == id) {
            return false;
        }
        let changed = self.active.as_ref() != Some(&id);
        self.active = Some(id);
        self.save();
        changed
    }

    /// Select the overview ("All") — no active space, rails fall back to cwd.
    /// Returns whether the active selection changed.
    pub fn select_overview(&mut self) -> bool {
        let changed = self.active.is_some();
        self.active = None;
        self.save();
        changed
    }

    /// **Toggle** the overview from the rail's grid tool: if a space is active, go to
    /// the overview (remembering the space so a second click returns here); if already
    /// on the overview, restore the remembered space — falling back to the first known
    /// space when it's gone (or staying put when there are none). Returns whether the
    /// active selection changed.
    pub fn toggle_overview(&mut self) -> bool {
        if let Some(active) = self.active.clone() {
            // Leaving a space for the overview — remember where to come back to.
            self.last_space = Some(active);
            self.active = None;
            self.save();
            return true;
        }
        // On the overview — restore the remembered space if it still exists, else the
        // first known space; no-op when no spaces are open.
        let restore = self
            .last_space
            .clone()
            .filter(|id| self.spaces.iter().any(|s| &s.id == id))
            .or_else(|| self.spaces.first().map(|s| s.id.clone()));
        match restore {
            Some(id) => {
                self.active = Some(id);
                self.save();
                true
            }
            None => false,
        }
    }

    /// Close (forget) a space — the operator's "Close Project". Removes it from the
    /// list; if it was active, falls back to the overview. Does **not** delete any
    /// files on disk (the repo and its `.moonlight/` roster are left intact).
    /// Returns whether a space was removed.
    pub fn remove_space(&mut self, id: &SpaceId) -> bool {
        let before = self.spaces.len();
        self.spaces.retain(|s| &s.id != id);
        let removed = self.spaces.len() != before;
        if removed {
            if self.active.as_ref() == Some(id) {
                self.active = None;
            }
            self.save();
        }
        removed
    }

    /// Focus a session and, while `follow_focus` is on, switch to **its space — but
    /// only if that space already exists**. Spaces are user-defined, so focusing a
    /// session whose root isn't an open space does *not* create one (the rails stay
    /// on the current space). Returns whether the active root changed.
    pub fn focus(&mut self, session: SessionId, attached_path: Option<&str>) -> bool {
        self.session = Some(session);
        self.session_root = attached_path.map(expand_home);

        if !self.follow_focus {
            return false;
        }
        let Some(root) = self.session_root.clone() else {
            return false;
        };
        let Some(id) = self.space_id_for_root(&root) else {
            return false; // no open space for this root → don't auto-create / switch
        };
        let changed = self.active.as_ref() != Some(&id);
        self.active = Some(id);
        self.save();
        changed
    }

    /// Toggle follow-focus. Turning it on re-roots onto the focused session's space
    /// **if that space exists** (never auto-creates one). Returns whether the active
    /// root changed. Persists either way.
    pub fn set_follow_focus(&mut self, on: bool) -> bool {
        self.follow_focus = on;
        let mut changed = false;
        if on {
            if let Some(root) = self.session_root.clone() {
                if let Some(id) = self.space_id_for_root(&root) {
                    changed = self.active.as_ref() != Some(&id);
                    self.active = Some(id);
                }
            }
        }
        self.save();
        changed
    }

    /// Record an **app-managed** session into the **existing** space for `root`:
    /// write-through to the repo-local roster (`<root>/.moonlight/sessions.json`) and
    /// update the in-memory roster so the rail reflects it live. Dedups by session id.
    /// No-op when no space is open for `root` (spaces aren't auto-created — a session
    /// launched outside any open space is still tracked by the engine, just not here).
    pub fn record_managed_session(&mut self, root: PathBuf, session: SpaceSession) {
        let Some(id) = self.space_id_for_root(&root) else {
            return;
        };
        space_sessions::upsert(&root, session.clone());
        if let Some(space) = self.spaces.iter_mut().find(|s| s.id == id) {
            match space.sessions.iter_mut().find(|s| s.id == session.id) {
                Some(existing) => *existing = session,
                None => space.sessions.push(session),
            }
        }
    }

    /// The operator's persisted trust decision for the open space at `root`, if any.
    /// `None` means either no open space for `root` or the operator hasn't been asked
    /// yet (the launch path prompts, then [`set_project_trust`](Self::set_project_trust)
    /// records the answer).
    pub fn project_trust(&self, root: &Path) -> Option<TrustTier> {
        self.spaces.iter().find(|s| s.root == *root)?.trust
    }

    /// Persist the project's trust tier (the operator's answer to the trust prompt, or a
    /// later change via the session's Trust selector). No-op when no space is open for
    /// `root` (trust is remembered per opened project). Persists to `projects.json`.
    pub fn set_project_trust(&mut self, root: &Path, tier: TrustTier) {
        if let Some(space) = self.spaces.iter_mut().find(|s| s.root == *root) {
            if space.trust != Some(tier) {
                space.trust = Some(tier);
                self.save();
            }
        }
    }

    /// Whether the project at `root` opts into auto-resuming a stalled/stopped session.
    /// `false` when no space is open for `root` or the operator left it off (default).
    pub fn project_auto_resume(&self, root: &Path) -> bool {
        self.spaces
            .iter()
            .find(|s| s.root == *root)
            .map(|s| s.auto_resume)
            .unwrap_or(false)
    }

    /// Toggle the project's auto-resume opt-in. No-op when no space is open for `root`.
    /// Persists to `projects.json`.
    pub fn set_project_auto_resume(&mut self, root: &Path, on: bool) {
        if let Some(space) = self.spaces.iter_mut().find(|s| s.root == *root) {
            if space.auto_resume != on {
                space.auto_resume = on;
                self.save();
            }
        }
    }

    /// The id of the open space rooted at `root`, if any (non-creating lookup).
    pub fn space_id_for_root(&self, root: &Path) -> Option<SpaceId> {
        self.spaces
            .iter()
            .find(|s| s.root == *root)
            .map(|s| s.id.clone())
    }

    /// Ensure a space exists for `root` (creating it if new). Returns its id. Only
    /// called from the explicit open/create path ([`Self::open_root`]).
    fn ensure_space(&mut self, root: PathBuf) -> SpaceId {
        if let Some(s) = self.spaces.iter().find(|s| s.root == root) {
            return s.id.clone();
        }
        let space = Space::from_root(root);
        let id = space.id.clone();
        self.spaces.push(space);
        id
    }

    /// Move `path` to the front of `recent`, deduped, capped.
    fn push_recent(&mut self, path: PathBuf) {
        self.recent.retain(|p| p != &path);
        self.recent.insert(0, path);
        self.recent.truncate(RECENT_CAP);
    }

    fn save(&self) {
        // In-memory only (e.g. tests) → never touch the real user config.
        let Some(path) = self.persist.as_ref() else {
            return;
        };
        let state = PersistState {
            spaces: self.spaces.clone(),
            active: self.active.clone(),
            recent: self.recent.clone(),
            follow_focus: self.follow_focus,
        };
        if let Err(err) = save_to(path, &state) {
            tracing::warn!(error = %err, "failed to save project space");
        }
    }
}

/// Expand a leading `~` to the user's home directory; otherwise verbatim. Session
/// paths are often stored home-abbreviated (see `session_tile`). Shared with
/// [`super::edit_gate`] so the editability gate roots on the same absolute path.
pub(crate) fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    if path == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(path)
}

/// macOS-first state file location under Application Support.
fn state_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/MoonlightCode/projects.json"))
}

fn load_from(path: &Path) -> Option<PersistState> {
    let json = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&json).ok()
}

fn save_to(path: &Path, state: &PersistState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    std::fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_root_creates_space_activates_and_fronts_recent() {
        let mut space = ProjectSpace::default();
        space.open_root(PathBuf::from("/tmp/a"));
        space.open_root(PathBuf::from("/tmp/b"));
        space.open_root(PathBuf::from("/tmp/a")); // re-open dedups + reactivates

        assert_eq!(space.root(), PathBuf::from("/tmp/a"));
        assert_eq!(
            space.recent(),
            &[PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]
        );
        // Two distinct spaces (re-opening "a" doesn't add a third).
        assert_eq!(space.spaces().len(), 2);
        assert_eq!(space.active().map(|i| i.as_str()), Some("/tmp/a"));
    }

    #[test]
    fn recent_is_capped() {
        let mut space = ProjectSpace::default();
        for i in 0..(RECENT_CAP + 5) {
            space.open_root(PathBuf::from(format!("/tmp/p{i}")));
        }
        assert_eq!(space.recent().len(), RECENT_CAP);
        // Newest first.
        assert_eq!(
            space.recent()[0],
            PathBuf::from(format!("/tmp/p{}", RECENT_CAP + 4))
        );
    }

    #[test]
    fn focus_does_not_create_a_space_only_switches_to_existing() {
        let mut space = ProjectSpace::default();

        // Focusing a session whose root has no open space must NOT create one
        // (spaces are user-defined) and must not move the active root off overview.
        space.focus(SessionId::new("s1"), Some("/tmp/proj"));
        assert!(space.spaces().is_empty());
        assert!(space.active().is_none());

        // Once the operator opens that folder as a space, focusing the session
        // switches to it.
        space.open_root(PathBuf::from("/tmp/other"));
        space.open_root(PathBuf::from("/tmp/proj"));
        space.select_overview();
        assert!(space.active().is_none());
        space.focus(SessionId::new("s1"), Some("/tmp/proj"));
        assert_eq!(space.root(), PathBuf::from("/tmp/proj"));
    }

    #[test]
    fn toggle_overview_round_trips_through_the_remembered_space() {
        let mut space = ProjectSpace::default();
        space.open_root(PathBuf::from("/tmp/a"));
        space.open_root(PathBuf::from("/tmp/b")); // b is active
        let b = space.active().cloned();

        // On a space → go to overview, remembering b.
        assert!(space.toggle_overview());
        assert!(space.active().is_none());
        // On overview → restore the remembered space (b), not the first one.
        assert!(space.toggle_overview());
        assert_eq!(space.active(), b.as_ref());
    }

    #[test]
    fn toggle_overview_falls_back_to_first_space_when_remembered_is_gone() {
        let mut space = ProjectSpace::default();
        space.open_root(PathBuf::from("/tmp/a"));
        space.open_root(PathBuf::from("/tmp/b")); // b active, will be remembered
        let b = space.active().cloned().unwrap();

        space.toggle_overview(); // remembers b, now on overview
        space.remove_space(&b); // b is gone (a remains)
                                // Restoring can't find b → falls back to the first known space (a).
        assert!(space.toggle_overview());
        assert_eq!(
            space.active().map(|id| id.as_str()),
            space.spaces().first().map(|s| s.id.as_str())
        );
    }

    #[test]
    fn toggle_overview_is_a_noop_with_no_spaces() {
        let mut space = ProjectSpace::default();
        // Already on overview, nothing to restore.
        assert!(!space.toggle_overview());
        assert!(space.active().is_none());
    }

    #[test]
    fn remove_space_drops_it_and_falls_back_to_overview() {
        let mut space = ProjectSpace::default();
        space.open_root(PathBuf::from("/tmp/a"));
        space.open_root(PathBuf::from("/tmp/b")); // b is active
        let b = space
            .spaces()
            .iter()
            .find(|s| s.root == PathBuf::from("/tmp/b"))
            .unwrap()
            .id
            .clone();

        assert!(space.remove_space(&b));
        assert_eq!(space.spaces().len(), 1);
        // Removing the active space falls back to overview (cwd), not /tmp/b.
        assert!(space.active().is_none());
        // Removing an unknown id is a no-op.
        assert!(!space.remove_space(&b));
    }

    #[test]
    fn record_managed_session_writes_roster_for_an_open_space() {
        use crate::views::space_sessions::{self, SpaceSession};
        use moonlight_domain::phase::Phase;
        use moonlight_domain::session::Mode;

        let root = std::env::temp_dir().join(format!("mlc-ps-managed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let rec = SpaceSession {
            id: SessionId::new("m1"),
            mode: Mode::Auto,
            phase: Phase::Plan,
            label: Some("Build the thing".into()),
        };

        let mut space = ProjectSpace::default();
        // No open space for this root yet → recording is a no-op (no auto-create).
        space.record_managed_session(root.clone(), rec.clone());
        assert!(space.spaces().is_empty());
        assert!(space_sessions::load(&root).is_empty());

        // Open the space, then record → roster updated in memory + on disk.
        space.open_root(root.clone());
        space.record_managed_session(root.clone(), rec);
        let sp = space.spaces().iter().find(|s| s.root == root).unwrap();
        assert_eq!(sp.sessions.len(), 1);
        assert_eq!(sp.sessions[0].id, SessionId::new("m1"));
        assert_eq!(space_sessions::load(&root).len(), 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn follow_toggle_gates_session_rerooting() {
        let mut space = ProjectSpace::default();
        assert!(space.follow_focus());

        // Spaces are user-defined: open the two projects first.
        space.open_root(PathBuf::from("/tmp/proj"));
        space.open_root(PathBuf::from("/tmp/other")); // active now

        // Following: focusing a session re-roots onto ITS (existing) space.
        space.focus(SessionId::new("s1"), Some("/tmp/proj"));
        assert_eq!(space.root(), PathBuf::from("/tmp/proj"));

        // Stop following, manually switch back to /tmp/other.
        space.set_follow_focus(false);
        let other = space
            .spaces()
            .iter()
            .find(|s| s.root == PathBuf::from("/tmp/other"))
            .unwrap()
            .id
            .clone();
        space.select_space(other);
        assert_eq!(space.root(), PathBuf::from("/tmp/other"));

        // Focusing a different session must NOT move the active root now (follow off).
        space.focus(SessionId::new("s2"), Some("/tmp/proj"));
        assert_eq!(space.root(), PathBuf::from("/tmp/other"));
        assert_eq!(space.session(), Some(&SessionId::new("s2")));

        // Turning follow back on re-roots onto the focused session's space.
        space.set_follow_focus(true);
        assert_eq!(space.root(), PathBuf::from("/tmp/proj"));
    }

    #[test]
    fn select_space_and_overview_switch_active_root() {
        let mut space = ProjectSpace::default();
        space.open_root(PathBuf::from("/tmp/a"));
        space.open_root(PathBuf::from("/tmp/b"));
        let a = space.spaces()[0].id.clone();

        // Selecting a known space re-roots and reports the change.
        assert!(space.select_space(a.clone()));
        assert_eq!(space.root(), PathBuf::from("/tmp/a"));
        // Re-selecting the same space is a no-op (no change).
        assert!(!space.select_space(a));

        // Overview drops the active root → falls back to cwd, not /tmp/a.
        assert!(space.select_overview());
        assert!(space.active().is_none());
        assert_ne!(space.root(), PathBuf::from("/tmp/a"));

        // An unknown id is rejected (stays on overview).
        assert!(!space.select_space(SpaceId("/tmp/nope".into())));
        assert!(space.active().is_none());
    }

    #[test]
    fn persistence_round_trips() {
        let dir = std::env::temp_dir().join(format!("mlc-projspace-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("projects.json");

        let state = PersistState {
            spaces: vec![
                Space::from_root(PathBuf::from("/tmp/x")),
                Space::from_root(PathBuf::from("/tmp/y")),
            ],
            active: Some(SpaceId("/tmp/y".into())),
            recent: vec![PathBuf::from("/tmp/x"), PathBuf::from("/tmp/y")],
            follow_focus: false,
        };
        save_to(&path, &state).unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.spaces.len(), 2);
        assert_eq!(loaded.active, Some(SpaceId("/tmp/y".into())));
        assert_eq!(loaded.recent, state.recent);
        assert!(!loaded.follow_focus);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_trust_and_auto_resume_persist_per_space() {
        let mut space = ProjectSpace::default();
        let a = PathBuf::from("/tmp/a");

        // Unknown until the space is opened and the operator decides.
        assert_eq!(space.project_trust(&a), None);
        assert!(!space.project_auto_resume(&a));
        // No space yet → setters are no-ops (nothing to remember against).
        space.set_project_trust(&a, TrustTier::Trusted);
        assert_eq!(space.project_trust(&a), None);

        space.open_root(a.clone());
        space.set_project_trust(&a, TrustTier::Trusted);
        space.set_project_auto_resume(&a, true);
        assert_eq!(space.project_trust(&a), Some(TrustTier::Trusted));
        assert!(space.project_auto_resume(&a));

        // A different project is independent.
        let b = PathBuf::from("/tmp/b");
        space.open_root(b.clone());
        assert_eq!(space.project_trust(&b), None);
        assert!(!space.project_auto_resume(&b));
    }

    #[test]
    fn trust_and_auto_resume_round_trip_through_persistence() {
        let dir = std::env::temp_dir().join(format!("mlc-trust-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("projects.json");

        let mut x = Space::from_root(PathBuf::from("/tmp/x"));
        x.trust = Some(TrustTier::ReadOnly);
        x.auto_resume = true;
        let state = PersistState {
            spaces: vec![x],
            active: None,
            recent: vec![],
            follow_focus: true,
        };
        save_to(&path, &state).unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.spaces[0].trust, Some(TrustTier::ReadOnly));
        assert!(loaded.spaces[0].auto_resume);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expand_home_resolves_tilde() {
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(
                expand_home("~/code/api"),
                PathBuf::from(home).join("code/api")
            );
        }
        assert_eq!(expand_home("/abs/path"), PathBuf::from("/abs/path"));
    }
}
