//! Forward-only schema migrations, tracked with SQLite's `PRAGMA user_version`.
//!
//! Each entry in [`MIGRATIONS`] is one migration step. Step `i` (1-based) is
//! applied when the database's `user_version` is `< i`; after the last applicable
//! step `user_version` is bumped to the migration count. **Never edit or reorder
//! an existing entry** — only append a new one (the version number is its index).

use rusqlite::Connection;

use moonlight_domain::errors::StoreError;

/// Ordered DDL steps. A step may contain multiple statements (run via
/// `execute_batch`). Append-only — see the module docs.
const MIGRATIONS: &[&str] = &[
    // 1 — managed-session identity + restart-survivable state.
    "CREATE TABLE managed_session (
        id          TEXT PRIMARY KEY NOT NULL,
        root        TEXT,
        mode        TEXT NOT NULL,
        phase       TEXT NOT NULL,
        adopted     INTEGER NOT NULL,
        paused      INTEGER NOT NULL,
        created_at  INTEGER NOT NULL,
        last_seen   INTEGER NOT NULL
    );",
    // 2 — append-only audit log of autonomous actions (FR33-34; T7 foundation).
    "CREATE TABLE audit_entry (
        id          TEXT PRIMARY KEY NOT NULL,
        session_id  TEXT NOT NULL,
        at          INTEGER NOT NULL,
        action      TEXT NOT NULL,
        revertible  INTEGER NOT NULL
    );
    CREATE INDEX idx_audit_session_at ON audit_entry (session_id, at);",
    // 3 — operator-pinned phase (manual override of workflow auto-advance).
    "ALTER TABLE managed_session ADD COLUMN phase_pinned INTEGER NOT NULL DEFAULT 0;",
    // 4 — last observed session title (CC custom `/rename` name or auto ai-title),
    // so a rehydrated session shows its name instead of the raw id.
    "ALTER TABLE managed_session ADD COLUMN title TEXT;",
    // 5 — operator soft-hide: mask a "not relevant anymore" session from the
    // default fleet view (recoverable via "show hidden"; transcript untouched).
    "ALTER TABLE managed_session ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0;",
    // 6 — which agent CLI backend drives the session (`claude` / `agy`). Stored as
    // serde-JSON like the other enum columns; the default `"ClaudeCode"` backfills
    // every pre-existing row (they were all Claude Code).
    "ALTER TABLE managed_session ADD COLUMN agent TEXT NOT NULL DEFAULT '\"ClaudeCode\"';",
    // 7 — the backend's own conversation id, when it differs from our managed `id`
    // (Antigravity has no `--session-id`, so its `conversationId` is discovered after
    // launch and persisted here). Nullable: NULL for Claude and for an AGY session
    // whose conversation hasn't been correlated yet.
    "ALTER TABLE managed_session ADD COLUMN conversation_id TEXT;",
    // 8 — the operator's trust tier for the session, so a manual "trust this session"
    // (or a project-trust default) survives restart/reset instead of reseeding to the
    // default-deny `Observed`. Stored as serde-JSON like the other enum columns; the
    // default `"Observed"` backfills every pre-existing row (matches the old reseed).
    "ALTER TABLE managed_session ADD COLUMN trust_tier TEXT NOT NULL DEFAULT '\"Observed\"';",
    // 9 — per-session ledger of the files an agent wrote (FR22). One row per
    // (session, file): `baseline` is the file's content the FIRST time the session
    // touched it, so the review surface can diff the session's whole effect rather
    // than its last edit. Keyed on the pair so the capture path can upsert.
    "CREATE TABLE session_file_change (
        session_id      TEXT NOT NULL,
        path            TEXT NOT NULL,
        first_touch_at  INTEGER NOT NULL,
        last_touch_at   INTEGER NOT NULL,
        touches         INTEGER NOT NULL,
        tool            TEXT NOT NULL,
        baseline        TEXT NOT NULL,
        PRIMARY KEY (session_id, path)
    );
    CREATE INDEX idx_change_session_touch ON session_file_change (session_id, last_touch_at);",
    // 10 — operator review comments anchored to a line range of a reviewed file.
    // Written as soon as they're typed (a closed tab must not lose them) and
    // batched: `sent_at` is NULL until the review carrying them reaches the session.
    "CREATE TABLE review_comment (
        id          TEXT PRIMARY KEY NOT NULL,
        session_id  TEXT NOT NULL,
        path        TEXT NOT NULL,
        side        TEXT NOT NULL,
        start_line  INTEGER NOT NULL,
        end_line    INTEGER NOT NULL,
        body        TEXT NOT NULL,
        at          INTEGER NOT NULL,
        sent_at     INTEGER
    );
    CREATE INDEX idx_comment_session_at ON review_comment (session_id, at);",
    // 11 — when the operator last marked this file reviewed (GitHub's "viewed"),
    // NULL until they do. A *time* rather than a flag, so the mark expires on its
    // own: `record_touch` advances `last_touch_at`, and any mark older than that
    // stops counting. A file the agent rewrites after you signed it off therefore
    // comes back unreviewed with no bookkeeping to forget.
    "ALTER TABLE session_file_change ADD COLUMN reviewed_at INTEGER;",
    // 12 — paths hidden from a session's review. Separate from the ledger because a
    // row here can be a *directory* prefix (ignoring `target/` hides files that were
    // never listed individually), and because ignoring a file must not touch its
    // change record. Dropped with the ledger when the pass closes.
    "CREATE TABLE review_ignore (
        session_id  TEXT NOT NULL,
        path        TEXT NOT NULL,
        PRIMARY KEY (session_id, path)
    );",
    // 13 — what a comment is about, whether the code under it still reads the same,
    // and whether it has been settled.
    //
    // `scope` widens a comment beyond a line range (file-wide, or about the review
    // itself); the default backfills every existing row, which was line-anchored by
    // construction. `anchor_text` is the anchored line as it read when the comment
    // was written — a line *number* is not an anchor, so this is what lets a comment
    // notice the code moved out from under it instead of arguing with whatever
    // occupies that number now; NULL for older rows, which are therefore never
    // called outdated rather than guessed about. `resolved_at` retires a comment
    // from the diff without deleting it, because why the code looks the way it does
    // is exactly what the settled half of a review records.
    "ALTER TABLE review_comment ADD COLUMN scope TEXT NOT NULL DEFAULT '\"Line\"';
     ALTER TABLE review_comment ADD COLUMN anchor_text TEXT;
     ALTER TABLE review_comment ADD COLUMN resolved_at INTEGER;",
];

/// Apply every migration the database hasn't seen yet, in order. Idempotent: a
/// fully-migrated database is left untouched.
pub fn apply(conn: &Connection) -> Result<(), StoreError> {
    let current: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(backend)?;

    let target = MIGRATIONS.len() as i64;
    for (i, ddl) in MIGRATIONS.iter().enumerate() {
        let version = i as i64 + 1;
        if version > current {
            conn.execute_batch(ddl).map_err(backend)?;
        }
    }

    if target > current {
        // `user_version` takes a literal, not a bound parameter.
        conn.pragma_update(None, "user_version", target)
            .map_err(backend)?;
    }
    Ok(())
}

fn backend(e: rusqlite::Error) -> StoreError {
    StoreError::Backend(e.to_string())
}
