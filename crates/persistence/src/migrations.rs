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
