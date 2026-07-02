//! Per-space managed-session roster — the **repo-local** record of which
//! app-managed CC sessions belong to a project space.
//!
//! Stored at `<root>/.moonlight/sessions.json` (JetBrains `.idea`-style: state
//! that lives *with* the project, not in a global app dir). This is the spaces
//! feature's view of a space's managed sessions — id + mode + phase, enough to
//! list them in the [`super::panels::spaces`] rail and `claude --resume` them
//! after a restart.
//!
//! This is intentionally **not** the engine's source of truth: `moonlight.db`
//! (`ManagedSessionStore`) stays canonical for live supervision state + the
//! append-only audit log. This file is the space's roster, keyed by the same
//! session ids; resume mechanics (`claude --resume <id>`) are identical whichever
//! store the id comes from. Both are written when the app launches a managed
//! session — the db for the supervisor, this file for the space.
//!
//! The `.moonlight/` dir self-ignores (a `.gitignore` of `*`) so a space's local
//! session state never shows up in the user's `git status`.

use std::path::{Path, PathBuf};

use moonlight_domain::ids::SessionId;
use moonlight_domain::phase::Phase;
use moonlight_domain::session::Mode;
use serde::{Deserialize, Serialize};

/// One app-managed CC session belonging to a space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceSession {
    pub id: SessionId,
    pub mode: Mode,
    pub phase: Phase,
    /// A human label (session title), if known at record time.
    #[serde(default)]
    pub label: Option<String>,
}

/// On-disk shape: a `{ sessions: [...] }` object (room to grow without a breaking
/// format change).
#[derive(Default, Serialize, Deserialize)]
struct Roster {
    #[serde(default)]
    sessions: Vec<SpaceSession>,
}

fn roster_dir(root: &Path) -> PathBuf {
    root.join(".moonlight")
}

fn roster_path(root: &Path) -> PathBuf {
    roster_dir(root).join("sessions.json")
}

/// Load the managed-session roster for the space at `root`. Empty when the file is
/// missing or unreadable (defensive — the project dir is user-owned and may race).
pub fn load(root: &Path) -> Vec<SpaceSession> {
    let Ok(json) = std::fs::read_to_string(roster_path(root)) else {
        return Vec::new();
    };
    serde_json::from_str::<Roster>(&json)
        .map(|r| r.sessions)
        .unwrap_or_default()
}

/// Insert or update a managed session in the space's roster (dedup by id), then
/// persist. Creates `<root>/.moonlight/` (self-ignoring) on first write.
pub fn upsert(root: &Path, session: SpaceSession) {
    let mut sessions = load(root);
    match sessions.iter_mut().find(|s| s.id == session.id) {
        Some(existing) => *existing = session,
        None => sessions.push(session),
    }
    save(root, &sessions);
}

/// Forget a managed session (e.g. it was permanently closed). No-op if absent.
#[allow(dead_code)]
pub fn remove(root: &Path, id: &SessionId) {
    let mut sessions = load(root);
    let before = sessions.len();
    sessions.retain(|s| &s.id != id);
    if sessions.len() != before {
        save(root, &sessions);
    }
}

fn save(root: &Path, sessions: &[SpaceSession]) {
    let dir = roster_dir(root);
    if let Err(err) = std::fs::create_dir_all(&dir) {
        tracing::warn!(error = %err, "failed to create .moonlight dir");
        return;
    }
    ensure_gitignore(&dir);
    let roster = Roster {
        sessions: sessions.to_vec(),
    };
    match serde_json::to_string_pretty(&roster) {
        Ok(json) => {
            if let Err(err) = std::fs::write(roster_path(root), json) {
                tracing::warn!(error = %err, "failed to write space session roster");
            }
        }
        Err(err) => tracing::warn!(error = %err, "failed to serialize space session roster"),
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
            "mlc-spacesess-{}-{tag}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sess(id: &str, phase: Phase) -> SpaceSession {
        SpaceSession {
            id: SessionId::new(id),
            mode: Mode::Plan,
            phase,
            label: None,
        }
    }

    #[test]
    fn load_is_empty_for_missing_roster() {
        let root = temp_root("missing");
        assert!(load(&root).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn upsert_persists_dedups_and_round_trips() {
        let root = temp_root("roundtrip");

        upsert(&root, sess("s1", Phase::Plan));
        upsert(&root, sess("s2", Phase::AutoImplement));
        // Re-upsert s1 with a new phase → updates in place, no duplicate.
        upsert(&root, sess("s1", Phase::Review));

        let loaded = load(&root);
        assert_eq!(loaded.len(), 2);
        let s1 = loaded
            .iter()
            .find(|s| s.id == SessionId::new("s1"))
            .unwrap();
        assert_eq!(s1.phase, Phase::Review);

        // The dir self-ignores so it never shows in git status.
        let gitignore = root.join(".moonlight/.gitignore");
        assert_eq!(std::fs::read_to_string(gitignore).unwrap(), "*\n");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_drops_a_session() {
        let root = temp_root("remove");
        upsert(&root, sess("s1", Phase::Plan));
        upsert(&root, sess("s2", Phase::Plan));
        remove(&root, &SessionId::new("s1"));

        let loaded = load(&root);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, SessionId::new("s2"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
