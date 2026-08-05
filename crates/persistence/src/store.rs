//! The SQLite-backed [`Store`]: a synchronous implementation of the domain
//! [`ManagedSessionStore`] port. Enum fields are persisted as their serde-JSON
//! text so the schema is stable across enum-variant changes.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension, Row};

use moonlight_domain::audit::AuditEntry;
use moonlight_domain::errors::StoreError;
use moonlight_domain::ids::{SessionId, Timestamp};
use moonlight_domain::ports::store::{ManagedSession, ManagedSessionStore, ManagedStateUpdate};

/// A SQLite connection guarded for shared, thread-safe use. `rusqlite::Connection`
/// is `Send` but not `Sync`; the `Mutex` makes the store `Send + Sync` so it can
/// be shared as `Arc<dyn ManagedSessionStore>` across the engine and UI.
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Open (creating if absent) the database file at `path` and run migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let conn = Connection::open(path).map_err(backend)?;
        Self::init(conn)
    }

    /// Open an ephemeral in-memory database (tests / a no-home fallback).
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory().map_err(backend)?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        crate::migrations::apply(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Lock the connection, recovering from a poisoned mutex rather than panicking
    /// (a prior panic mid-query must not take the whole store down).
    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl ManagedSessionStore for Store {
    fn upsert_managed(&self, s: &ManagedSession) -> Result<(), StoreError> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO managed_session
                (id, root, title, mode, phase, adopted, paused, phase_pinned, hidden, created_at, last_seen, agent, conversation_id, trust_tier)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(id) DO UPDATE SET
                root         = excluded.root,
                title        = excluded.title,
                mode         = excluded.mode,
                phase        = excluded.phase,
                adopted      = excluded.adopted,
                paused       = excluded.paused,
                phase_pinned = excluded.phase_pinned,
                hidden       = excluded.hidden,
                last_seen    = excluded.last_seen,
                trust_tier   = excluded.trust_tier,
                conversation_id = COALESCE(excluded.conversation_id, managed_session.conversation_id)",
            params![
                s.id.as_str(),
                s.root,
                s.title,
                enc(&s.mode)?,
                enc(&s.phase)?,
                s.adopted as i64,
                s.paused as i64,
                s.phase_pinned as i64,
                s.hidden as i64,
                s.created_at.as_millis(),
                s.last_seen.as_millis(),
                enc(&s.agent)?,
                s.conversation_id,
                enc(&s.trust_tier)?,
            ],
        )
        .map_err(backend)?;
        Ok(())
    }

    fn update_managed_state(&self, u: &ManagedStateUpdate) -> Result<bool, StoreError> {
        let conn = self.lock();
        let rows = conn
            .execute(
                "UPDATE managed_session
                 SET title = ?2, phase = ?3, mode = ?4, adopted = ?5, paused = ?6,
                     phase_pinned = ?7, hidden = ?8, last_seen = ?9, trust_tier = ?10
                 WHERE id = ?1",
                params![
                    u.id.as_str(),
                    u.title,
                    enc(&u.phase)?,
                    enc(&u.mode)?,
                    u.adopted as i64,
                    u.paused as i64,
                    u.phase_pinned as i64,
                    u.hidden as i64,
                    u.last_seen.as_millis(),
                    enc(&u.trust_tier)?,
                ],
            )
            .map_err(backend)?;
        Ok(rows > 0)
    }

    fn set_conversation_id(
        &self,
        id: &SessionId,
        conversation_id: &str,
    ) -> Result<bool, StoreError> {
        let conn = self.lock();
        let rows = conn
            .execute(
                "UPDATE managed_session SET conversation_id = ?2 WHERE id = ?1",
                params![id.as_str(), conversation_id],
            )
            .map_err(backend)?;
        Ok(rows > 0)
    }

    fn managed(&self, id: &SessionId) -> Result<Option<ManagedSession>, StoreError> {
        let conn = self.lock();
        let raw = conn
            .query_row(
                "SELECT id, root, title, mode, phase, adopted, paused, phase_pinned, hidden, created_at, last_seen, agent, conversation_id, trust_tier
                 FROM managed_session WHERE id = ?1 OR conversation_id = ?1",
                [id.as_str()],
                raw_managed,
            )
            .optional()
            .map_err(backend)?;
        raw.map(managed_from_raw).transpose()
    }

    fn all_managed(&self) -> Result<Vec<ManagedSession>, StoreError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, root, title, mode, phase, adopted, paused, phase_pinned, hidden, created_at, last_seen, agent, conversation_id, trust_tier
                 FROM managed_session ORDER BY created_at ASC, id ASC",
            )
            .map_err(backend)?;
        let rows = stmt.query_map([], raw_managed).map_err(backend)?;
        let mut out = Vec::new();
        for raw in rows {
            out.push(managed_from_raw(raw.map_err(backend)?)?);
        }
        Ok(out)
    }

    fn remove_managed(&self, id: &SessionId) -> Result<(), StoreError> {
        let conn = self.lock();
        conn.execute("DELETE FROM managed_session WHERE id = ?1", [id.as_str()])
            .map_err(backend)?;
        Ok(())
    }

    fn append_audit(&self, e: &AuditEntry) -> Result<(), StoreError> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO audit_entry (id, session_id, at, action, revertible)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                e.id,
                e.session_id.as_str(),
                e.at.as_millis(),
                enc(&e.action)?,
                e.revertible as i64,
            ],
        )
        .map_err(backend)?;
        Ok(())
    }

    fn recent_audit(
        &self,
        session: &SessionId,
        limit: usize,
    ) -> Result<Vec<AuditEntry>, StoreError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, at, action, revertible
                 FROM audit_entry WHERE session_id = ?1
                 ORDER BY at DESC, id DESC LIMIT ?2",
            )
            .map_err(backend)?;
        let rows = stmt
            .query_map(params![session.as_str(), limit as i64], raw_audit)
            .map_err(backend)?;
        let mut out = Vec::new();
        for raw in rows {
            out.push(audit_from_raw(raw.map_err(backend)?)?);
        }
        Ok(out)
    }
}

// ---- row mapping ---------------------------------------------------------
//
// Row callbacks must return `rusqlite::Result`, so they pull the raw columns
// only; serde decoding (which yields a `StoreError`) happens afterwards.

struct RawManaged {
    id: String,
    root: Option<String>,
    title: Option<String>,
    mode: String,
    phase: String,
    adopted: i64,
    paused: i64,
    phase_pinned: i64,
    hidden: i64,
    created_at: i64,
    last_seen: i64,
    agent: String,
    conversation_id: Option<String>,
    trust_tier: String,
}

fn raw_managed(row: &Row) -> rusqlite::Result<RawManaged> {
    Ok(RawManaged {
        id: row.get(0)?,
        root: row.get(1)?,
        title: row.get(2)?,
        mode: row.get(3)?,
        phase: row.get(4)?,
        adopted: row.get(5)?,
        paused: row.get(6)?,
        phase_pinned: row.get(7)?,
        hidden: row.get(8)?,
        created_at: row.get(9)?,
        last_seen: row.get(10)?,
        agent: row.get(11)?,
        conversation_id: row.get(12)?,
        trust_tier: row.get(13)?,
    })
}

fn managed_from_raw(r: RawManaged) -> Result<ManagedSession, StoreError> {
    Ok(ManagedSession {
        id: SessionId::new(r.id),
        root: r.root,
        title: r.title,
        mode: dec(&r.mode)?,
        phase: dec(&r.phase)?,
        adopted: r.adopted != 0,
        paused: r.paused != 0,
        phase_pinned: r.phase_pinned != 0,
        hidden: r.hidden != 0,
        created_at: Timestamp::from_millis(r.created_at),
        last_seen: Timestamp::from_millis(r.last_seen),
        agent: dec(&r.agent)?,
        conversation_id: r.conversation_id,
        trust_tier: dec(&r.trust_tier)?,
    })
}

struct RawAudit {
    id: String,
    session_id: String,
    at: i64,
    action: String,
    revertible: i64,
}

fn raw_audit(row: &Row) -> rusqlite::Result<RawAudit> {
    Ok(RawAudit {
        id: row.get(0)?,
        session_id: row.get(1)?,
        at: row.get(2)?,
        action: row.get(3)?,
        revertible: row.get(4)?,
    })
}

fn audit_from_raw(r: RawAudit) -> Result<AuditEntry, StoreError> {
    Ok(AuditEntry {
        id: r.id,
        session_id: SessionId::new(r.session_id),
        at: Timestamp::from_millis(r.at),
        action: dec(&r.action)?,
        revertible: r.revertible != 0,
    })
}

// ---- helpers -------------------------------------------------------------

fn backend(e: rusqlite::Error) -> StoreError {
    StoreError::Backend(e.to_string())
}

/// Encode an enum field as its serde-JSON text (stable across schema changes).
fn enc<T: serde::Serialize>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|e| StoreError::Corrupt(e.to_string()))
}

/// Decode an enum field stored by [`enc`].
fn dec<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, StoreError> {
    serde_json::from_str(text).map_err(|e| StoreError::Corrupt(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonlight_domain::agent::AgentKind;
    use moonlight_domain::audit::AuditAction;
    use moonlight_domain::phase::Phase;
    use moonlight_domain::session::Mode;
    use moonlight_domain::trust::TrustTier;

    fn managed(id: &str, created: i64) -> ManagedSession {
        ManagedSession {
            id: SessionId::new(id),
            root: Some("/repo/api".into()),
            title: Some("Refactor auth".into()),
            mode: Mode::Auto,
            phase: Phase::Plan,
            agent: AgentKind::ClaudeCode,
            conversation_id: None,
            // Non-default so the round-trip test proves trust persists.
            trust_tier: TrustTier::Trusted,
            adopted: false,
            paused: false,
            phase_pinned: false,
            hidden: false,
            created_at: Timestamp::from_millis(created),
            last_seen: Timestamp::from_millis(created),
        }
    }

    #[test]
    fn migrations_are_idempotent() {
        // A second open of the same in-memory connection path is not possible, but
        // re-running `apply` on an already-migrated connection must be a no-op.
        let store = Store::open_in_memory().unwrap();
        crate::migrations::apply(&store.lock()).unwrap();
        // Still usable.
        assert!(store.all_managed().unwrap().is_empty());
    }

    #[test]
    fn managed_round_trips() {
        let store = Store::open_in_memory().unwrap();
        let rec = managed("sess-1", 1000);
        store.upsert_managed(&rec).unwrap();

        let got = store.managed(&SessionId::new("sess-1")).unwrap();
        assert_eq!(got.as_ref(), Some(&rec));

        assert!(store.managed(&SessionId::new("nope")).unwrap().is_none());
    }

    #[test]
    fn agent_backend_round_trips() {
        // A non-default backend (Antigravity) survives a persist→restore so a restart
        // relaunches `agy`, not `claude`.
        let store = Store::open_in_memory().unwrap();
        let mut rec = managed("agy-1", 1000);
        rec.agent = AgentKind::Antigravity;
        store.upsert_managed(&rec).unwrap();

        let got = store.managed(&SessionId::new("agy-1")).unwrap().unwrap();
        assert_eq!(got.agent, AgentKind::Antigravity);
    }

    #[test]
    fn conversation_id_persists_and_setter_updates_it() {
        // AGY correlation: a managed row starts with no conversation id; the discovery
        // setter stamps it, and it survives a subsequent upsert that carries None
        // (the COALESCE keeps the discovered value instead of clobbering it back).
        let store = Store::open_in_memory().unwrap();
        let mut rec = managed("agy-cid", 1000);
        rec.agent = AgentKind::Antigravity;
        store.upsert_managed(&rec).unwrap();
        assert_eq!(
            store
                .managed(&SessionId::new("agy-cid"))
                .unwrap()
                .unwrap()
                .conversation_id,
            None
        );

        // Setter stamps the discovered conversation id.
        assert!(store
            .set_conversation_id(&SessionId::new("agy-cid"), "conv-xyz")
            .unwrap());
        assert_eq!(
            store
                .managed(&SessionId::new("agy-cid"))
                .unwrap()
                .unwrap()
                .conversation_id,
            Some("conv-xyz".to_string())
        );

        // A refresh-upsert with conversation_id: None must NOT wipe the discovered value.
        let mut refreshed = managed("agy-cid", 1000);
        refreshed.agent = AgentKind::Antigravity;
        refreshed.title = Some("moved on".into());
        assert_eq!(refreshed.conversation_id, None);
        store.upsert_managed(&refreshed).unwrap();
        let got = store.managed(&SessionId::new("agy-cid")).unwrap().unwrap();
        assert_eq!(
            got.conversation_id,
            Some("conv-xyz".to_string()),
            "kept via COALESCE"
        );
        assert_eq!(
            got.title.as_deref(),
            Some("moved on"),
            "other fields still refresh"
        );

        // The setter is UPDATE-only — a ghost id touches nothing.
        assert!(!store
            .set_conversation_id(&SessionId::new("ghost"), "nope")
            .unwrap());
    }

    #[test]
    fn legacy_rows_backfill_to_claude_code_and_the_merged_plan_phase() {
        // A row inserted without the `agent` column (the pre-migration record shape)
        // gets the schema DEFAULT, which decodes to ClaudeCode — old sessions stay
        // Claude on restart rather than failing to decode. Its `phase` is the literal
        // "Discovery" from before Discovery and Plan merged: it must decode as Plan
        // (the serde alias), or every pre-merge session would come back Corrupt.
        let store = Store::open_in_memory().unwrap();
        store
            .lock()
            .execute(
                "INSERT INTO managed_session
                    (id, root, title, mode, phase, adopted, paused, phase_pinned, hidden, created_at, last_seen)
                 VALUES ('legacy', NULL, NULL, '\"Auto\"', '\"Discovery\"', 0, 0, 0, 0, 0, 0)",
                [],
            )
            .unwrap();

        let got = store.managed(&SessionId::new("legacy")).unwrap().unwrap();
        assert_eq!(got.agent, AgentKind::ClaudeCode);
        assert_eq!(
            got.phase,
            Phase::Plan,
            "a persisted Discovery phase rehydrates as its successor, Plan"
        );
    }

    #[test]
    fn upsert_preserves_created_at_and_updates_mutable_fields() {
        let store = Store::open_in_memory().unwrap();
        store.upsert_managed(&managed("sess-1", 1000)).unwrap();

        // Re-upsert with a different created_at + mutated fields.
        let mut second = managed("sess-1", 9999);
        second.phase = Phase::AutoImplement;
        second.mode = Mode::Auto;
        second.adopted = true;
        second.last_seen = Timestamp::from_millis(2000);
        store.upsert_managed(&second).unwrap();

        let got = store.managed(&SessionId::new("sess-1")).unwrap().unwrap();
        assert_eq!(got.created_at.as_millis(), 1000, "created_at preserved");
        assert_eq!(got.last_seen.as_millis(), 2000);
        assert_eq!(got.phase, Phase::AutoImplement);
        assert_eq!(got.mode, Mode::Auto);
        assert!(got.adopted);
    }

    #[test]
    fn update_managed_state_is_update_only() {
        let store = Store::open_in_memory().unwrap();

        // No row yet → update touches nothing (the table stays managed-only).
        let update = ManagedStateUpdate {
            id: SessionId::new("ghost"),
            title: Some("Renamed in CC".into()),
            phase: Phase::Review,
            mode: Mode::Auto,
            trust_tier: TrustTier::Standard,
            adopted: true,
            paused: true,
            phase_pinned: true,
            hidden: true,
            last_seen: Timestamp::from_millis(50),
        };
        assert!(!store.update_managed_state(&update).unwrap());
        assert!(store.managed(&SessionId::new("ghost")).unwrap().is_none());

        // Insert, then a state update lands.
        store.upsert_managed(&managed("ghost", 10)).unwrap();
        assert!(store.update_managed_state(&update).unwrap());
        let got = store.managed(&SessionId::new("ghost")).unwrap().unwrap();
        assert_eq!(got.phase, Phase::Review);
        assert!(got.paused);
        assert!(got.phase_pinned, "pin persisted through update");
        assert!(got.hidden, "hidden persisted through update");
        assert_eq!(
            got.trust_tier,
            TrustTier::Standard,
            "trust tier persisted through update"
        );
        assert_eq!(
            got.title.as_deref(),
            Some("Renamed in CC"),
            "title persisted"
        );
        assert_eq!(got.created_at.as_millis(), 10, "created_at untouched");
    }

    #[test]
    fn all_managed_is_ordered_by_created_at() {
        let store = Store::open_in_memory().unwrap();
        store.upsert_managed(&managed("b", 200)).unwrap();
        store.upsert_managed(&managed("a", 100)).unwrap();
        store.upsert_managed(&managed("c", 300)).unwrap();

        let ids: Vec<_> = store
            .all_managed()
            .unwrap()
            .into_iter()
            .map(|s| s.id.0)
            .collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn remove_managed_forgets_the_session() {
        let store = Store::open_in_memory().unwrap();
        store.upsert_managed(&managed("sess-1", 1)).unwrap();
        store.remove_managed(&SessionId::new("sess-1")).unwrap();
        assert!(store.managed(&SessionId::new("sess-1")).unwrap().is_none());
        // Removing an absent row is not an error.
        store.remove_managed(&SessionId::new("sess-1")).unwrap();
    }

    #[test]
    fn managed_lookup_by_id_or_conversation_id() {
        let store = Store::open_in_memory().unwrap();
        let mut sess = managed("sess-1", 1);
        sess.conversation_id = Some("conv-123".to_string());
        store.upsert_managed(&sess).unwrap();

        // Should find by launch UUID
        let res1 = store.managed(&SessionId::new("sess-1")).unwrap().unwrap();
        assert_eq!(res1.id.as_str(), "sess-1");

        // Should also find by conversation ID
        let res2 = store.managed(&SessionId::new("conv-123")).unwrap().unwrap();
        assert_eq!(res2.id.as_str(), "sess-1");
    }

    #[test]
    fn audit_appends_and_reads_newest_first() {
        let store = Store::open_in_memory().unwrap();
        let sid = SessionId::new("sess-1");

        for (i, at) in [10_i64, 30, 20].into_iter().enumerate() {
            store
                .append_audit(&AuditEntry {
                    id: format!("e{i}"),
                    session_id: sid.clone(),
                    at: Timestamp::from_millis(at),
                    action: AuditAction::PhaseChanged { to: Phase::Plan },
                    revertible: false,
                })
                .unwrap();
        }
        // A different session must not bleed in.
        store
            .append_audit(&AuditEntry {
                id: "other".into(),
                session_id: SessionId::new("sess-2"),
                at: Timestamp::from_millis(999),
                action: AuditAction::FeedbackInjected {
                    message: "x".into(),
                },
                revertible: false,
            })
            .unwrap();

        let recent = store.recent_audit(&sid, 2).unwrap();
        let ats: Vec<_> = recent.iter().map(|e| e.at.as_millis()).collect();
        assert_eq!(ats, vec![30, 20], "newest first, limited");

        let all = store.recent_audit(&sid, 10).unwrap();
        assert_eq!(all.len(), 3);
        // The complex action variant round-trips through JSON.
        assert!(matches!(
            all.last().unwrap().action,
            AuditAction::PhaseChanged { to: Phase::Plan }
        ));
    }
}
