//! The SQLite-backed [`Store`]: a synchronous implementation of the domain
//! [`ManagedSessionStore`] port. Enum fields are persisted as their serde-JSON
//! text so the schema is stable across enum-variant changes.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension, Row};

use moonlight_domain::audit::AuditEntry;
use moonlight_domain::changes::{Baseline, FileTouch, ReviewComment, TouchedPath};
use moonlight_domain::errors::StoreError;
use moonlight_domain::ids::{SessionId, Timestamp};
use moonlight_domain::ports::store::{
    ManagedSession, ManagedSessionStore, ManagedStateUpdate, SessionChangeStore,
};

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

impl SessionChangeStore for Store {
    fn record_touch(&self, touch: &FileTouch) -> Result<bool, StoreError> {
        let conn = self.lock();
        // The conflict clause is what makes first-touch-wins atomic: the baseline
        // and `first_touch_at` are only ever written by the INSERT, so a concurrent
        // second write can't overwrite the pre-image with post-edit content.
        let inserted = conn
            .execute(
                "INSERT INTO session_file_change
                    (session_id, path, first_touch_at, last_touch_at, touches, tool, baseline)
                 VALUES (?1, ?2, ?3, ?3, 1, ?4, ?5)
                 ON CONFLICT(session_id, path) DO UPDATE SET
                    last_touch_at = excluded.last_touch_at,
                    touches       = touches + 1,
                    tool          = excluded.tool",
                params![
                    touch.session_id.as_str(),
                    touch.path,
                    touch.at.as_millis(),
                    enc(&touch.tool)?,
                    enc(&touch.baseline)?,
                ],
            )
            .map_err(backend)?;
        // An upsert reports 1 row changed either way, so re-read the counter to
        // tell "created the baseline" from "bumped an existing row".
        let touches: i64 = conn
            .query_row(
                "SELECT touches FROM session_file_change WHERE session_id = ?1 AND path = ?2",
                params![touch.session_id.as_str(), touch.path],
                |row| row.get(0),
            )
            .map_err(backend)?;
        Ok(inserted > 0 && touches == 1)
    }

    fn baseline(&self, session: &SessionId, path: &str) -> Result<Option<Baseline>, StoreError> {
        let conn = self.lock();
        let raw: Option<String> = conn
            .query_row(
                "SELECT baseline FROM session_file_change
                 WHERE session_id = ?1 AND path = ?2",
                params![session.as_str(), path],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        raw.map(|text| dec(&text)).transpose()
    }

    fn touched_paths(&self, session: &SessionId) -> Result<Vec<TouchedPath>, StoreError> {
        let conn = self.lock();
        // `baseline` is deliberately not selected — it can be megabytes per row and
        // a name list never reads it. The two flags a caller actually needs are
        // derived from the column instead, which SQLite answers from the stored JSON
        // without shipping it back.
        let mut stmt = conn
            .prepare(
                "SELECT path, touches, tool,
                        baseline = '\"Created\"',
                        baseline LIKE '{\"FromHead\":%',
                        reviewed_at IS NOT NULL AND reviewed_at >= last_touch_at
                 FROM session_file_change WHERE session_id = ?1
                 ORDER BY last_touch_at DESC, path ASC",
            )
            .map_err(backend)?;
        let rows = stmt
            .query_map(params![session.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(backend)?;
        let mut out = Vec::new();
        for row in rows {
            let (path, touches, tool, created, from_head, reviewed) = row.map_err(backend)?;
            out.push(TouchedPath {
                path,
                touches: touches.max(0) as u32,
                tool: dec(&tool)?,
                created: created != 0,
                from_head: from_head != 0,
                reviewed: reviewed != 0,
            });
        }
        Ok(out)
    }

    fn mark_reviewed(
        &self,
        session: &SessionId,
        path: &str,
        at: Option<Timestamp>,
    ) -> Result<bool, StoreError> {
        let conn = self.lock();
        // UPDATE-only: a mark on a path with no ledger row would be a mark on
        // nothing, and would resurrect as a phantom row if the file were later
        // touched.
        let changed = conn
            .execute(
                "UPDATE session_file_change SET reviewed_at = ?3
                 WHERE session_id = ?1 AND path = ?2",
                params![session.as_str(), path, at.map(|t| t.as_millis())],
            )
            .map_err(backend)?;
        Ok(changed > 0)
    }

    fn ignored_paths(&self, session: &SessionId) -> Result<Vec<String>, StoreError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT path FROM review_ignore WHERE session_id = ?1")
            .map_err(backend)?;
        let rows = stmt
            .query_map(params![session.as_str()], |row| row.get::<_, String>(0))
            .map_err(backend)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(backend)?);
        }
        Ok(out)
    }

    fn set_ignored(
        &self,
        session: &SessionId,
        path: &str,
        ignored: bool,
    ) -> Result<(), StoreError> {
        let conn = self.lock();
        if ignored {
            conn.execute(
                "INSERT INTO review_ignore (session_id, path) VALUES (?1, ?2)
                 ON CONFLICT(session_id, path) DO NOTHING",
                params![session.as_str(), path],
            )
            .map_err(backend)?;
        } else {
            conn.execute(
                "DELETE FROM review_ignore WHERE session_id = ?1 AND path = ?2",
                params![session.as_str(), path],
            )
            .map_err(backend)?;
        }
        Ok(())
    }

    fn clear_ignored(&self, session: &SessionId) -> Result<(), StoreError> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM review_ignore WHERE session_id = ?1",
            params![session.as_str()],
        )
        .map_err(backend)?;
        Ok(())
    }

    fn touched_counts(&self) -> Result<Vec<(SessionId, u32)>, StoreError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT session_id, count(*) FROM session_file_change
                 GROUP BY session_id",
            )
            .map_err(backend)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(backend)?;
        let mut out = Vec::new();
        for row in rows {
            let (session, count) = row.map_err(backend)?;
            out.push((SessionId::new(session), count.max(0) as u32));
        }
        Ok(out)
    }

    fn forget_session_changes(&self, session: &SessionId) -> Result<(), StoreError> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM session_file_change WHERE session_id = ?1",
            params![session.as_str()],
        )
        .map_err(backend)?;
        conn.execute(
            "DELETE FROM review_comment WHERE session_id = ?1",
            params![session.as_str()],
        )
        .map_err(backend)?;
        // The ignore list describes a review pass, so it dies with it. Leaving it
        // behind would silently hide files from the *next* pass — which is exactly
        // the thing the operator would not think to check.
        conn.execute(
            "DELETE FROM review_ignore WHERE session_id = ?1",
            params![session.as_str()],
        )
        .map_err(backend)?;
        Ok(())
    }

    fn add_comment(&self, c: &ReviewComment) -> Result<(), StoreError> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO review_comment
                (id, session_id, path, side, start_line, end_line, body, at, sent_at,
                 scope, anchor_text, resolved_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                c.id,
                c.session_id.as_str(),
                c.path,
                enc(&c.side)?,
                c.start_line as i64,
                c.end_line as i64,
                c.body,
                c.at.as_millis(),
                c.sent_at.map(Timestamp::as_millis),
                enc(&c.scope)?,
                c.anchor_text,
                c.resolved_at.map(Timestamp::as_millis),
            ],
        )
        .map_err(backend)?;
        Ok(())
    }

    fn comments(&self, session: &SessionId) -> Result<Vec<ReviewComment>, StoreError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, path, side, start_line, end_line, body, at, sent_at,
                        scope, anchor_text, resolved_at
                 FROM review_comment WHERE session_id = ?1
                 ORDER BY at ASC, id ASC",
            )
            .map_err(backend)?;
        let rows = stmt
            .query_map(params![session.as_str()], raw_comment)
            .map_err(backend)?;
        let mut out = Vec::new();
        for raw in rows {
            out.push(comment_from_raw(raw.map_err(backend)?)?);
        }
        Ok(out)
    }

    fn update_comment(&self, c: &ReviewComment) -> Result<bool, StoreError> {
        let conn = self.lock();
        let rows = conn
            .execute(
                "UPDATE review_comment
                 SET path = ?2, side = ?3, start_line = ?4, end_line = ?5, body = ?6,
                     sent_at = ?7, scope = ?8, anchor_text = ?9, resolved_at = ?10
                 WHERE id = ?1",
                params![
                    c.id,
                    c.path,
                    enc(&c.side)?,
                    c.start_line as i64,
                    c.end_line as i64,
                    c.body,
                    c.sent_at.map(Timestamp::as_millis),
                    enc(&c.scope)?,
                    c.anchor_text,
                    c.resolved_at.map(Timestamp::as_millis),
                ],
            )
            .map_err(backend)?;
        Ok(rows > 0)
    }

    fn delete_comment(&self, id: &str) -> Result<(), StoreError> {
        let conn = self.lock();
        conn.execute("DELETE FROM review_comment WHERE id = ?1", params![id])
            .map_err(backend)?;
        Ok(())
    }

    fn mark_comments_sent(&self, ids: &[String], at: Timestamp) -> Result<(), StoreError> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock();
        // One transaction: a partially-stamped batch would re-send some comments.
        let tx = conn.transaction().map_err(backend)?;
        {
            let mut stmt = tx
                .prepare("UPDATE review_comment SET sent_at = ?1 WHERE id = ?2")
                .map_err(backend)?;
            for id in ids {
                stmt.execute(params![at.as_millis(), id]).map_err(backend)?;
            }
        }
        tx.commit().map_err(backend)?;
        Ok(())
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

struct RawComment {
    id: String,
    session_id: String,
    path: String,
    side: String,
    start_line: i64,
    end_line: i64,
    body: String,
    at: i64,
    sent_at: Option<i64>,
    scope: String,
    anchor_text: Option<String>,
    resolved_at: Option<i64>,
}

fn raw_comment(row: &Row) -> rusqlite::Result<RawComment> {
    Ok(RawComment {
        id: row.get(0)?,
        session_id: row.get(1)?,
        path: row.get(2)?,
        side: row.get(3)?,
        start_line: row.get(4)?,
        end_line: row.get(5)?,
        body: row.get(6)?,
        at: row.get(7)?,
        sent_at: row.get(8)?,
        scope: row.get(9)?,
        anchor_text: row.get(10)?,
        resolved_at: row.get(11)?,
    })
}

fn comment_from_raw(r: RawComment) -> Result<ReviewComment, StoreError> {
    Ok(ReviewComment {
        id: r.id,
        session_id: SessionId::new(r.session_id),
        scope: dec(&r.scope)?,
        path: r.path,
        side: dec(&r.side)?,
        start_line: r.start_line.max(0) as u32,
        end_line: r.end_line.max(0) as u32,
        body: r.body,
        anchor_text: r.anchor_text,
        at: Timestamp::from_millis(r.at),
        sent_at: r.sent_at.map(Timestamp::from_millis),
        resolved_at: r.resolved_at.map(Timestamp::from_millis),
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
    use moonlight_domain::changes::{Baseline, BaselineGap, ChangeTool, CommentScope, DiffSide};
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

    fn touch(session: &str, path: &str, at: i64, baseline: Baseline) -> FileTouch {
        FileTouch {
            session_id: SessionId::new(session),
            path: path.into(),
            at: Timestamp::from_millis(at),
            tool: ChangeTool::Edit,
            baseline,
        }
    }

    #[test]
    fn first_touch_captures_the_baseline_and_later_ones_only_count() {
        let store = Store::open_in_memory().unwrap();
        let sid = SessionId::new("sess-1");

        let first = store
            .record_touch(&touch(
                "sess-1",
                "/repo/a.rs",
                10,
                Baseline::Content("before\n".into()),
            ))
            .unwrap();
        assert!(first, "the first touch establishes the baseline");

        // A later write must NOT overwrite the pre-image with post-edit content —
        // that is the whole point of the ledger.
        let again = store
            .record_touch(&touch(
                "sess-1",
                "/repo/a.rs",
                30,
                Baseline::Content("AFTER THE EDIT\n".into()),
            ))
            .unwrap();
        assert!(!again, "a repeat touch does not re-baseline");

        let files = store.touched_paths(&sid).unwrap();
        assert_eq!(files.len(), 1, "one row per (session, file)");
        assert_eq!(files[0].touches, 2);
        assert_eq!(
            store.baseline(&sid, "/repo/a.rs").unwrap(),
            Some(Baseline::Content("before\n".into())),
            "the pre-image is the FIRST touch, not the latest"
        );
    }

    #[test]
    fn touched_paths_are_per_session_newest_first() {
        let store = Store::open_in_memory().unwrap();
        store
            .record_touch(&touch("s1", "/repo/old.rs", 10, Baseline::Created))
            .unwrap();
        store
            .record_touch(&touch("s1", "/repo/new.rs", 50, Baseline::Created))
            .unwrap();
        // Another session's edits must not bleed into this one's review.
        store
            .record_touch(&touch("s2", "/repo/other.rs", 99, Baseline::Created))
            .unwrap();

        let paths: Vec<_> = store
            .touched_paths(&SessionId::new("s1"))
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert_eq!(paths, vec!["/repo/new.rs", "/repo/old.rs"]);
    }

    #[test]
    fn a_head_baseline_is_flagged_in_the_lightweight_view() {
        let store = Store::open_in_memory().unwrap();
        let mut shell = touch(
            "s1",
            "/repo/gen.rs",
            10,
            Baseline::FromHead("committed\n".into()),
        );
        shell.tool = ChangeTool::Shell;
        store.record_touch(&shell).unwrap();

        let paths = store.touched_paths(&SessionId::new("s1")).unwrap();
        assert!(paths[0].from_head, "flagged without decoding the baseline");
        assert!(!paths[0].created);
    }

    #[test]
    fn a_shell_write_round_trips_with_its_head_baseline() {
        let store = Store::open_in_memory().unwrap();
        let mut t = touch(
            "s1",
            "/repo/gen.rs",
            10,
            Baseline::FromHead("committed\n".into()),
        );
        t.tool = ChangeTool::Shell;
        store.record_touch(&t).unwrap();

        let sid = SessionId::new("s1");
        let files = store.touched_paths(&sid).unwrap();
        assert_eq!(files[0].tool, ChangeTool::Shell);
        assert!(files[0].tool.is_inferred());
        assert_eq!(
            store.baseline(&sid, "/repo/gen.rs").unwrap(),
            Some(Baseline::FromHead("committed\n".into()))
        );
    }

    #[test]
    fn an_observed_baseline_survives_a_later_shell_write() {
        // The exact pre-image read at an `Edit` must not be replaced by the coarser
        // HEAD blob when a shell command touches the same file afterwards.
        let store = Store::open_in_memory().unwrap();
        store
            .record_touch(&touch(
                "s1",
                "/repo/a.rs",
                10,
                Baseline::Content("exact pre-image\n".into()),
            ))
            .unwrap();
        let mut shell = touch(
            "s1",
            "/repo/a.rs",
            20,
            Baseline::FromHead("coarser\n".into()),
        );
        shell.tool = ChangeTool::Shell;
        store.record_touch(&shell).unwrap();

        let sid = SessionId::new("s1");
        assert_eq!(
            store.baseline(&sid, "/repo/a.rs").unwrap(),
            Some(Baseline::Content("exact pre-image\n".into())),
            "first touch wins, and the first touch was the exact one"
        );
        assert_eq!(store.touched_paths(&sid).unwrap()[0].touches, 2);
    }

    #[test]
    fn paths_and_counts_answer_without_reading_baselines() {
        let store = Store::open_in_memory().unwrap();
        store
            .record_touch(&touch(
                "s1",
                "/repo/edited.rs",
                10,
                // A large pre-image the badge/name queries must not drag back.
                Baseline::Content("x".repeat(200_000)),
            ))
            .unwrap();
        store
            .record_touch(&touch("s1", "/repo/made.rs", 20, Baseline::Created))
            .unwrap();
        store
            .record_touch(&touch("s2", "/repo/other.rs", 30, Baseline::Created))
            .unwrap();

        let paths = store.touched_paths(&SessionId::new("s1")).unwrap();
        assert_eq!(paths.len(), 2, "scoped to the session, newest first");
        assert_eq!(paths[0].path, "/repo/made.rs");
        assert!(paths[0].created, "derived without decoding the baseline");
        assert_eq!(paths[0].touches, 1);
        assert!(!paths[1].created, "an edited file was not created");
        assert!(
            !paths[0].from_head && !paths[1].from_head,
            "neither baseline came from VCS"
        );

        let mut counts = store.touched_counts().unwrap();
        counts.sort_by(|a, b| a.0 .0.cmp(&b.0 .0));
        assert_eq!(
            counts,
            vec![(SessionId::new("s1"), 2), (SessionId::new("s2"), 1)]
        );
    }

    #[test]
    fn counts_are_empty_before_anything_is_touched() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.touched_counts().unwrap().is_empty());
        assert!(store
            .touched_paths(&SessionId::new("nobody"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn baseline_gaps_round_trip() {
        let store = Store::open_in_memory().unwrap();
        store
            .record_touch(&touch(
                "s1",
                "/repo/blob.bin",
                10,
                Baseline::Unavailable {
                    reason: BaselineGap::Binary,
                },
            ))
            .unwrap();
        assert_eq!(
            store
                .baseline(&SessionId::new("s1"), "/repo/blob.bin")
                .unwrap(),
            Some(Baseline::Unavailable {
                reason: BaselineGap::Binary
            })
        );
        // A file this session never touched has no pre-image at all.
        assert_eq!(
            store
                .baseline(&SessionId::new("s1"), "/repo/never.rs")
                .unwrap(),
            None
        );
    }

    fn comment(id: &str, session: &str, at: i64) -> ReviewComment {
        ReviewComment {
            id: id.into(),
            session_id: SessionId::new(session),
            scope: CommentScope::Line,
            path: "/repo/a.rs".into(),
            side: DiffSide::After,
            start_line: 4,
            end_line: 8,
            body: "no backoff".into(),
            anchor_text: Some("    retry(op)".into()),
            at: Timestamp::from_millis(at),
            sent_at: None,
            resolved_at: None,
        }
    }

    #[test]
    fn comments_round_trip_and_delete() {
        let store = Store::open_in_memory().unwrap();
        let sid = SessionId::new("s1");
        store.add_comment(&comment("c1", "s1", 10)).unwrap();
        store.add_comment(&comment("c2", "s1", 20)).unwrap();
        store.add_comment(&comment("other", "s2", 30)).unwrap();

        let got = store.comments(&sid).unwrap();
        assert_eq!(got.len(), 2, "scoped to the session, oldest first");
        assert_eq!(got[0].id, "c1");
        assert_eq!(got[0].side, DiffSide::After);
        assert_eq!((got[0].start_line, got[0].end_line), (4, 8));
        assert!(got[0].sent_at.is_none());
        assert_eq!(got[0].scope, CommentScope::Line);
        assert_eq!(got[0].anchor_text.as_deref(), Some("    retry(op)"));
        assert!(got[0].resolved_at.is_none());

        store.delete_comment("c1").unwrap();
        let after = store.comments(&sid).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].id, "c2");
    }

    #[test]
    fn a_comment_wider_than_a_line_keeps_its_scope() {
        // A file- or review-scoped comment carries no line range. If the scope did
        // not survive the round trip it would come back looking like a comment on
        // line 0 of whatever file happened to be in the row.
        let store = Store::open_in_memory().unwrap();
        let sid = SessionId::new("s1");

        let mut file_wide = comment("c1", "s1", 10);
        file_wide.scope = CommentScope::File;
        file_wide.start_line = 0;
        file_wide.end_line = 0;
        file_wide.anchor_text = None;
        store.add_comment(&file_wide).unwrap();

        let mut general = comment("c2", "s1", 20);
        general.scope = CommentScope::Review;
        general.path = String::new();
        general.start_line = 0;
        general.end_line = 0;
        general.anchor_text = None;
        store.add_comment(&general).unwrap();

        let got = store.comments(&sid).unwrap();
        assert_eq!(got[0].scope, CommentScope::File);
        assert_eq!(got[0].path, "/repo/a.rs");
        assert_eq!(got[1].scope, CommentScope::Review);
        assert!(got[1].path.is_empty());
        assert!(got.iter().all(|c| c.anchor_text.is_none()));
    }

    #[test]
    fn resolving_a_comment_keeps_it_in_the_record() {
        // Resolved is not deleted: the settled half of a review is the record of why
        // the code ended up as it did, and it has to survive reopening the tab.
        let store = Store::open_in_memory().unwrap();
        let sid = SessionId::new("s1");
        store.add_comment(&comment("c1", "s1", 10)).unwrap();

        let mut settled = store.comments(&sid).unwrap()[0].clone();
        settled.resolved_at = Some(Timestamp::from_millis(99));
        assert!(store.update_comment(&settled).unwrap());

        let got = &store.comments(&sid).unwrap()[0];
        assert_eq!(got.resolved_at, Some(Timestamp::from_millis(99)));
        assert!(got.is_resolved());

        // And reopening it clears the stamp rather than leaving a second copy.
        let mut reopened = got.clone();
        reopened.resolved_at = None;
        assert!(store.update_comment(&reopened).unwrap());
        let got = store.comments(&sid).unwrap();
        assert_eq!(got.len(), 1);
        assert!(!got[0].is_resolved());
    }

    #[test]
    fn an_edited_comment_re_anchors_to_the_line_it_now_names() {
        // The anchor text moves with the line range: a comment corrected onto a
        // different line must not keep the old line's text, or it would come back
        // reading as outdated the moment it was fixed.
        let store = Store::open_in_memory().unwrap();
        let sid = SessionId::new("s1");
        store.add_comment(&comment("c1", "s1", 10)).unwrap();

        let mut moved = store.comments(&sid).unwrap()[0].clone();
        moved.start_line = 20;
        moved.end_line = 20;
        moved.anchor_text = Some("    retry_with_backoff(op)".into());
        assert!(store.update_comment(&moved).unwrap());

        let got = &store.comments(&sid).unwrap()[0];
        assert_eq!(
            got.anchor_text.as_deref(),
            Some("    retry_with_backoff(op)")
        );
        assert!(!got.outdated_against(Some("    retry_with_backoff(op)")));
    }

    #[test]
    fn editing_a_sent_comment_can_put_it_back_in_the_queue() {
        // Correcting a comment that landed wrong is the whole point: the body
        // changes, and clearing `sent_at` is what makes it go again.
        let store = Store::open_in_memory().unwrap();
        let sid = SessionId::new("s1");
        store.add_comment(&comment("c1", "s1", 10)).unwrap();
        store
            .mark_comments_sent(&["c1".to_string()], Timestamp::from_millis(50))
            .unwrap();
        assert!(store.comments(&sid).unwrap()[0].sent_at.is_some());

        let mut fixed = store.comments(&sid).unwrap()[0].clone();
        fixed.body = "actually, use exponential backoff".into();
        fixed.start_line = 12;
        fixed.end_line = 12;
        fixed.sent_at = None;
        assert!(store.update_comment(&fixed).unwrap());

        let got = &store.comments(&sid).unwrap()[0];
        assert_eq!(got.body, "actually, use exponential backoff");
        assert_eq!((got.start_line, got.end_line), (12, 12));
        assert!(got.sent_at.is_none(), "and it is pending again");
    }

    #[test]
    fn updating_an_unknown_comment_reports_that_it_matched_nothing() {
        let store = Store::open_in_memory().unwrap();
        let ghost = comment("nope", "s1", 10);
        assert!(!store.update_comment(&ghost).unwrap());
    }

    #[test]
    fn marking_sent_stamps_only_the_named_comments() {
        let store = Store::open_in_memory().unwrap();
        let sid = SessionId::new("s1");
        store.add_comment(&comment("c1", "s1", 10)).unwrap();
        store.add_comment(&comment("c2", "s1", 20)).unwrap();

        store
            .mark_comments_sent(&["c1".to_string()], Timestamp::from_millis(99))
            .unwrap();

        let got = store.comments(&sid).unwrap();
        assert_eq!(got[0].sent_at.map(Timestamp::as_millis), Some(99));
        assert!(got[1].sent_at.is_none(), "unsent comments stay unsent");

        // An empty batch is a no-op, not an error (nothing to send).
        store
            .mark_comments_sent(&[], Timestamp::from_millis(100))
            .unwrap();
    }

    #[test]
    fn forgetting_a_session_drops_its_changes_and_comments() {
        let store = Store::open_in_memory().unwrap();
        store
            .record_touch(&touch("s1", "/repo/a.rs", 10, Baseline::Created))
            .unwrap();
        store.add_comment(&comment("c1", "s1", 10)).unwrap();
        store
            .record_touch(&touch("s2", "/repo/b.rs", 10, Baseline::Created))
            .unwrap();

        store
            .set_ignored(&SessionId::new("s1"), "target/", true)
            .unwrap();

        store.forget_session_changes(&SessionId::new("s1")).unwrap();

        assert!(store
            .touched_paths(&SessionId::new("s1"))
            .unwrap()
            .is_empty());
        assert!(store.comments(&SessionId::new("s1")).unwrap().is_empty());
        assert!(
            store
                .ignored_paths(&SessionId::new("s1"))
                .unwrap()
                .is_empty(),
            "a closed pass must not hide files from the next one"
        );
        assert_eq!(
            store.touched_paths(&SessionId::new("s2")).unwrap().len(),
            1,
            "another session's ledger is untouched"
        );
    }

    #[test]
    fn a_review_mark_expires_when_the_session_writes_the_file_again() {
        // The point of storing a time rather than a flag: nothing has to remember
        // to un-mark a file, so no path can forget to.
        let store = Store::open_in_memory().unwrap();
        let sid = SessionId::new("s1");
        store
            .record_touch(&touch("s1", "/repo/a.rs", 10, Baseline::Created))
            .unwrap();
        assert!(!store.touched_paths(&sid).unwrap()[0].reviewed);

        assert!(store
            .mark_reviewed(&sid, "/repo/a.rs", Some(Timestamp::from_millis(20)))
            .unwrap());
        assert!(store.touched_paths(&sid).unwrap()[0].reviewed);

        store
            .record_touch(&touch("s1", "/repo/a.rs", 30, Baseline::Created))
            .unwrap();
        assert!(
            !store.touched_paths(&sid).unwrap()[0].reviewed,
            "a file the agent rewrote is unreviewed again"
        );

        // Marking it once more covers the new write.
        store
            .mark_reviewed(&sid, "/repo/a.rs", Some(Timestamp::from_millis(40)))
            .unwrap();
        assert!(store.touched_paths(&sid).unwrap()[0].reviewed);
        // And it can be taken back by hand.
        store.mark_reviewed(&sid, "/repo/a.rs", None).unwrap();
        assert!(!store.touched_paths(&sid).unwrap()[0].reviewed);
    }

    #[test]
    fn marking_a_path_outside_the_ledger_changes_nothing() {
        let store = Store::open_in_memory().unwrap();
        let sid = SessionId::new("s1");
        assert!(
            !store
                .mark_reviewed(
                    &sid,
                    "/repo/never-touched.rs",
                    Some(Timestamp::from_millis(1))
                )
                .unwrap(),
            "UPDATE-only: a mark never invents a ledger row"
        );
        assert!(store.touched_paths(&sid).unwrap().is_empty());
    }

    #[test]
    fn ignores_are_per_session_and_reversible() {
        let store = Store::open_in_memory().unwrap();
        let s1 = SessionId::new("s1");
        store.set_ignored(&s1, "src/generated.rs", true).unwrap();
        store.set_ignored(&s1, "target", true).unwrap();
        // Re-ignoring is a no-op rather than a constraint violation.
        store.set_ignored(&s1, "target", true).unwrap();
        store
            .set_ignored(&SessionId::new("s2"), "other.rs", true)
            .unwrap();

        let mut got = store.ignored_paths(&s1).unwrap();
        got.sort();
        assert_eq!(got, vec!["src/generated.rs", "target"]);

        store.set_ignored(&s1, "target", false).unwrap();
        assert_eq!(
            store.ignored_paths(&s1).unwrap(),
            vec!["src/generated.rs"],
            "un-ignoring drops only the named path"
        );

        store.clear_ignored(&s1).unwrap();
        assert!(store.ignored_paths(&s1).unwrap().is_empty());
        assert_eq!(
            store.ignored_paths(&SessionId::new("s2")).unwrap().len(),
            1,
            "another session's review is untouched"
        );
    }
}
