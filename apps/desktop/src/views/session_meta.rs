//! Per-space **session display metadata** — the operator's custom *name* and
//! *color* for a session, shown in the focus view and the (future) catalog.
//!
//! Stored repo-local at `<root>/.moonlight/session-meta.json` (same posture as the
//! managed-session roster in [`super::space_sessions`]: state that lives *with* the
//! project, self-ignored from git). Kept **separate** from that roster because it
//! covers **all** sessions a space can show — managed *and* merely-observed CC
//! sessions — whereas the roster only lists app-managed ones.
//!
//! Names integrate with Claude Code's native rename where possible: CC supports
//! `/rename` / `claude -n <name>` but its name store is undocumented and not
//! reliably readable, and CC has **no** color concept. So for a live/managed session
//! we *write through* to CC (`/rename`) and mirror the name here for display
//! ([`NameSource::Cc`]); color is always ours. Observed sessions are named locally.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::{hsla, Hsla};
use moonlight_domain::ids::SessionId;
use serde::{Deserialize, Serialize};

/// A small fixed palette of session colors. A closed enum (not raw hex) so colors
/// stay on-theme and serialize stably across versions.
/// A session color. The palette mirrors Claude Code's `/color` accepted values
/// exactly (red, orange, yellow, green, cyan, blue, purple, pink) so the app color
/// and CC's own per-session color stay identical, and [`token`](Self::token) can be
/// sent straight to CC's `/color`. Persisted as that token (see [`SessionMeta::color`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionColor {
    Red,
    Orange,
    Yellow,
    Green,
    Cyan,
    Blue,
    Purple,
    Pink,
}

impl SessionColor {
    /// The full palette, in swatch order (a warm→cool spectrum).
    pub const ALL: [SessionColor; 8] = [
        SessionColor::Red,
        SessionColor::Orange,
        SessionColor::Yellow,
        SessionColor::Green,
        SessionColor::Cyan,
        SessionColor::Blue,
        SessionColor::Purple,
        SessionColor::Pink,
    ];

    /// A short human label (swatch caption).
    pub fn label(self) -> &'static str {
        match self {
            SessionColor::Red => "Red",
            SessionColor::Orange => "Orange",
            SessionColor::Yellow => "Yellow",
            SessionColor::Green => "Green",
            SessionColor::Cyan => "Cyan",
            SessionColor::Blue => "Blue",
            SessionColor::Purple => "Purple",
            SessionColor::Pink => "Pink",
        }
    }

    /// The Claude Code `/color` token — also the persisted form (lowercase). Round-trips
    /// via [`from_token`](Self::from_token).
    pub fn token(self) -> &'static str {
        match self {
            SessionColor::Red => "red",
            SessionColor::Orange => "orange",
            SessionColor::Yellow => "yellow",
            SessionColor::Green => "green",
            SessionColor::Cyan => "cyan",
            SessionColor::Blue => "blue",
            SessionColor::Purple => "purple",
            SessionColor::Pink => "pink",
        }
    }

    /// Parse a stored/CC token back to a palette color (case-insensitive). Unknown or
    /// retired tokens (e.g. "teal", "default") yield `None`.
    pub fn from_token(token: &str) -> Option<SessionColor> {
        match token.trim().to_ascii_lowercase().as_str() {
            "red" => Some(SessionColor::Red),
            "orange" => Some(SessionColor::Orange),
            "yellow" => Some(SessionColor::Yellow),
            "green" => Some(SessionColor::Green),
            "cyan" => Some(SessionColor::Cyan),
            "blue" => Some(SessionColor::Blue),
            "purple" => Some(SessionColor::Purple),
            "pink" => Some(SessionColor::Pink),
            _ => None,
        }
    }

    /// The rendered color. Mid-lightness, moderate saturation so it reads on both the
    /// raised card and dark base surfaces.
    pub fn hsla(self) -> Hsla {
        match self {
            SessionColor::Red => hsla(0.00, 0.62, 0.58, 1.0),
            SessionColor::Orange => hsla(0.07, 0.74, 0.58, 1.0),
            SessionColor::Yellow => hsla(0.13, 0.72, 0.55, 1.0),
            SessionColor::Green => hsla(0.38, 0.48, 0.50, 1.0),
            SessionColor::Cyan => hsla(0.50, 0.55, 0.52, 1.0),
            SessionColor::Blue => hsla(0.60, 0.60, 0.60, 1.0),
            SessionColor::Purple => hsla(0.75, 0.48, 0.64, 1.0),
            SessionColor::Pink => hsla(0.90, 0.58, 0.66, 1.0),
        }
    }
}

/// Where a session's custom name came from — drives whether we wrote through to CC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum NameSource {
    /// Set locally only (e.g. an observed session with no live PTY to drive).
    #[default]
    Local,
    /// Written through to Claude Code's native rename (`/rename`) for a managed session.
    Cc,
}

/// Operator-set display metadata for one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SessionMeta {
    /// Custom name overriding CC's auto `aiTitle`. `None`/empty ⇒ fall back to the title.
    #[serde(default)]
    pub name: Option<String>,
    /// Operator-chosen color as a Claude Code `/color` **token** (e.g. `"blue"`); `None`
    /// = default/no color. Stored as the token (not the enum) so it round-trips straight
    /// to CC's `/color`, and so a retired/unknown token degrades to "no color" via
    /// [`palette_color`](Self::palette_color) instead of failing the whole record (which
    /// would also drop the name).
    #[serde(default)]
    pub color: Option<String>,
    /// Provenance of `name` (whether it was mirrored to CC).
    #[serde(default)]
    pub name_source: NameSource,
}

impl SessionMeta {
    /// The custom name if set and non-empty.
    pub fn display_name(&self) -> Option<&str> {
        self.name.as_deref().filter(|n| !n.trim().is_empty())
    }

    /// The palette color, if the stored token maps to one (`None` for unset/unknown).
    pub fn palette_color(&self) -> Option<SessionColor> {
        self.color.as_deref().and_then(SessionColor::from_token)
    }
}

/// On-disk shape: `{ "meta": { "<session-id>": { … } } }` (room to grow).
#[derive(Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    meta: HashMap<String, SessionMeta>,
}

fn meta_dir(root: &Path) -> PathBuf {
    root.join(".moonlight")
}

fn meta_path(root: &Path) -> PathBuf {
    meta_dir(root).join("session-meta.json")
}

/// Load all session metadata for the space at `root`. Empty on a missing/unreadable
/// file (defensive — the project dir is user-owned and may race).
pub fn load(root: &Path) -> HashMap<String, SessionMeta> {
    let Ok(json) = std::fs::read_to_string(meta_path(root)) else {
        return HashMap::new();
    };
    serde_json::from_str::<Store>(&json)
        .map(|s| s.meta)
        .unwrap_or_default()
}

/// The metadata for one session (default when absent).
pub fn get(root: &Path, id: &SessionId) -> SessionMeta {
    load(root).remove(id.as_str()).unwrap_or_default()
}

/// Replace the metadata for `id` and persist (dedup by id).
pub fn upsert(root: &Path, id: &SessionId, meta: SessionMeta) {
    let mut all = load(root);
    all.insert(id.as_str().to_string(), meta);
    save(root, &all);
}

/// Set (or clear) a session's name, preserving its color. Clearing = empty/`None`.
pub fn set_name(root: &Path, id: &SessionId, name: Option<String>, source: NameSource) {
    let mut meta = get(root, id);
    meta.name = name.filter(|n| !n.trim().is_empty());
    meta.name_source = source;
    upsert(root, id, meta);
}

/// Set (or clear) a session's color, preserving its name. Stored as the CC `/color`
/// token so it round-trips to Claude Code.
pub fn set_color(root: &Path, id: &SessionId, color: Option<SessionColor>) {
    let mut meta = get(root, id);
    meta.color = color.map(|c| c.token().to_string());
    upsert(root, id, meta);
}

fn save(root: &Path, all: &HashMap<String, SessionMeta>) {
    let dir = meta_dir(root);
    if let Err(err) = std::fs::create_dir_all(&dir) {
        tracing::warn!(error = %err, "failed to create .moonlight dir");
        return;
    }
    ensure_gitignore(&dir);
    let store = Store { meta: all.clone() };
    match serde_json::to_string_pretty(&store) {
        Ok(json) => {
            if let Err(err) = std::fs::write(meta_path(root), json) {
                tracing::warn!(error = %err, "failed to write session metadata");
            }
        }
        Err(err) => tracing::warn!(error = %err, "failed to serialize session metadata"),
    }
}

/// An in-memory, observable cache of session metadata across the spaces the UI has
/// touched, so the fleet grid can render a tile's custom name/color with a cheap map
/// lookup instead of a disk read per tile per frame. Held as a shared `Entity` in
/// [`crate::views::workspace::ShellDeps`]: the focus view writes through it (which
/// persists *and* updates the map, then notifies), and the grid observes it so a
/// rename/recolor reflects live without a reload.
#[derive(Default)]
pub struct SessionMetaCache {
    loaded: HashSet<PathBuf>,
    by_id: HashMap<String, SessionMeta>,
}

impl SessionMetaCache {
    /// Load a space's metadata into the cache once (no-op if already loaded). Cheap
    /// to call repeatedly — only the first call for a root touches disk.
    pub fn ensure_space(&mut self, root: &Path) {
        if self.loaded.insert(root.to_path_buf()) {
            self.by_id.extend(load(root));
        }
    }

    /// The cached metadata for `id` (default when unknown). Pure map lookup.
    pub fn get(&self, id: &SessionId) -> SessionMeta {
        self.by_id.get(id.as_str()).cloned().unwrap_or_default()
    }

    /// Persist + cache a name change for `id` in the space at `root`.
    pub fn set_name(&mut self, root: &Path, id: &SessionId, name: Option<String>, source: NameSource) {
        set_name(root, id, name, source);
        self.loaded.insert(root.to_path_buf());
        self.by_id.insert(id.as_str().to_string(), get(root, id));
    }

    /// Persist + cache a color change for `id` in the space at `root`.
    pub fn set_color(&mut self, root: &Path, id: &SessionId, color: Option<SessionColor>) {
        set_color(root, id, color);
        self.loaded.insert(root.to_path_buf());
        self.by_id.insert(id.as_str().to_string(), get(root, id));
    }

    /// Persist + cache the full metadata for `id` — used to carry a session's
    /// name/color over to its successor when ↻ Reset replaces it with a fresh id.
    pub fn put(&mut self, root: &Path, id: &SessionId, meta: SessionMeta) {
        upsert(root, id, meta.clone());
        self.loaded.insert(root.to_path_buf());
        self.by_id.insert(id.as_str().to_string(), meta);
    }
}

/// Make `.moonlight/` self-ignoring so a space's local state never pollutes git.
fn ensure_gitignore(dir: &Path) {
    let path = dir.join(".gitignore");
    if !path.exists() {
        let _ = std::fs::write(path, "*\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mlc-sessmeta-{}-{tag}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_is_empty_for_missing_file() {
        let root = temp_root("missing");
        assert!(load(&root).is_empty());
        assert_eq!(get(&root, &SessionId::new("x")), SessionMeta::default());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn set_name_and_color_round_trip_and_merge() {
        let root = temp_root("roundtrip");
        let id = SessionId::new("s1");

        set_name(&root, &id, Some("Auth refactor".into()), NameSource::Cc);
        // Setting color must preserve the name (merge, not overwrite).
        set_color(&root, &id, Some(SessionColor::Yellow));

        let m = get(&root, &id);
        assert_eq!(m.display_name(), Some("Auth refactor"));
        assert_eq!(m.palette_color(), Some(SessionColor::Yellow));
        // Persisted as the CC `/color` token, so it round-trips to Claude Code.
        assert_eq!(m.color.as_deref(), Some("yellow"));
        assert_eq!(m.name_source, NameSource::Cc);

        // The dir self-ignores so it never shows in git status.
        let gitignore = root.join(".moonlight/.gitignore");
        assert_eq!(std::fs::read_to_string(gitignore).unwrap(), "*\n");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clearing_name_falls_back_to_none_and_keeps_color() {
        let root = temp_root("clear");
        let id = SessionId::new("s2");
        set_color(&root, &id, Some(SessionColor::Blue));
        set_name(&root, &id, Some("temp".into()), NameSource::Local);
        // Empty name clears it (display falls back to the title elsewhere).
        set_name(&root, &id, Some("   ".into()), NameSource::Local);

        let m = get(&root, &id);
        assert_eq!(m.display_name(), None);
        assert_eq!(
            m.palette_color(),
            Some(SessionColor::Blue),
            "color survives a name clear"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cache_loads_once_and_reflects_writes() {
        let root = temp_root("cache");
        set_color(&root, &SessionId::new("a"), Some(SessionColor::Cyan));

        let mut cache = SessionMetaCache::default();
        cache.ensure_space(&root);
        assert_eq!(
            cache.get(&SessionId::new("a")).palette_color(),
            Some(SessionColor::Cyan)
        );
        // Unknown id → default (no panic).
        assert_eq!(cache.get(&SessionId::new("zzz")), SessionMeta::default());

        // A write through the cache updates the map immediately and persists.
        cache.set_name(&root, &SessionId::new("a"), Some("Pipeline".into()), NameSource::Local);
        assert_eq!(cache.get(&SessionId::new("a")).display_name(), Some("Pipeline"));
        assert_eq!(get(&root, &SessionId::new("a")).display_name(), Some("Pipeline"));
        // The earlier color survived the name write.
        assert_eq!(
            cache.get(&SessionId::new("a")).palette_color(),
            Some(SessionColor::Cyan)
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn entries_are_keyed_per_session() {
        let root = temp_root("multi");
        set_color(&root, &SessionId::new("a"), Some(SessionColor::Green));
        set_color(&root, &SessionId::new("b"), Some(SessionColor::Red));
        let all = load(&root);
        assert_eq!(all.len(), 2);
        assert_eq!(all["a"].palette_color(), Some(SessionColor::Green));
        assert_eq!(all["b"].palette_color(), Some(SessionColor::Red));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn token_round_trips_and_retired_colors_degrade() {
        // Every palette color round-trips through its CC token.
        for c in SessionColor::ALL {
            assert_eq!(SessionColor::from_token(c.token()), Some(c));
        }
        // Case-insensitive (old capitalized values like "Blue" still resolve).
        assert_eq!(SessionColor::from_token("BLUE"), Some(SessionColor::Blue));
        // Retired/unknown tokens degrade to no color (names are unaffected).
        assert_eq!(SessionColor::from_token("teal"), None);
        assert_eq!(SessionColor::from_token("default"), None);
    }
}
