# Handoff 06 — DB panel: add-source modal, connection test, clean tree

Status: **DONE + VERIFIED (Parts 0–2)** (2026-07-02). `cargo build -p moonlight-desktop` green;
`cargo test -p moonlight-desktop` → 263 passed / 0 failed (incl. the 4 new DB tests). The build was
transiently red mid-session from a *concurrent* conversation_id/statusline refactor (attach_command 5th
arg + obs.rs tuple) in files we never touched — that resolved itself as the other work landed.
NOTE: visual/UX check of the modal + tree still wants the operator to launch the GPUI app (macOS Metal).

## Implementation done
- Part 0 `db_source.rs`: `test_connection()`, `PgConfig::from_parts()`, `pg_dsn()` + percent-encoder,
  driver `test` fns, tests. ✅ compiles.
- Part 1 modal: new `db_add_source.rs` (`AddSourceForm`/`Driver`/`TestState`), workspace-hosted
  `render_add_source_modal` + open/close/test/save/browse methods, `ChromeRequest::OpenAddDataSource`,
  DbObserver header collapsed to a single `＋ Add`. ✅ compiles.
- Part 2 tree: schema layer (PG) + Columns/Keys/Indexes folders + glyph icons; tests updated. ✅ compiles.

## BLOCKER (not ours)
Crate won't link: `attach_command` gained a 5th param `conversation_id: Option<&str>` (concurrent
conversation-id/AGY refactor — `session_monitor.rs`, `crates/domain/ports/store.rs`,
`crates/persistence/*` are all modified in the tree though we never touched them). 5 call sites still
pass 4 args: session_monitor.rs 494 & 727, workspace.rs 276, 2388, 2503 (site 274 already updated with
`None`). Until those land, no full build/test run. Our files show **0 errors** in the filtered build.

---
Reference screenshots:
DataGrip "Data Sources and Drivers" dialog (add modal) + DataGrip database tree (clean display).

## Goal (operator ask)

1. Adding a data source opens a **popup modal** with a proper connection form (today it's a
   native file picker for SQLite + an inline DSN bar for Postgres).
2. From that modal, **test the connection** before saving.
3. Once a source exists, **display its contents cleanly** — DataGrip-style tree: schema layer,
   per-table Columns / Keys / Indexes sub-folders, glyph icons + type hints.

## Assumptions (made while operator away — correct if wrong)

- **Postgres input = structured fields + editable DSN.** Host / Port / Database / User /
  Password fields that assemble the DSN, plus a raw URL field that overrides (DataGrip's model).
- **Tree = full DataGrip parity** (schema layer for PG, Columns/Keys/Indexes folders, icons).

## Current state (mapped)

- `apps/desktop/src/views/panels/db_source.rs` — driver: `DataSource {Sqlite(PathBuf), Postgres(PgConfig)}`,
  `PgConfig {dsn,label}`, `load_schema`/`load_page`/`run_query`, saved-sources persistence in
  `.moonlight-local/db_sources.json`, `pg_label`. **No `test_connection`.**
- `apps/desktop/src/views/panels/db_observer.rs` — right-dock tree. Header has `+ File`
  (native picker → `pick_sqlite`) and `+ Connect` (toggles inline `render_connect_bar` with a
  single DSN `InputState`). `add_source` persists+selects+lazy-loads schema. `build_tree`/
  `push_schema` flatten sources → Tables/Views groups → table → **columns directly** + Indexes group.
- `apps/desktop/src/views/panels/db_grid.rs` / `db_console.rs` — center data editor + SQL console
  (unchanged by this work; item-3 SC is the **tree**, not the grid).
- Reusable modal pattern: `Workspace::render_create_run_config_modal` (workspace.rs ~3047) —
  full-window backdrop + centered box + `Input::new(..)` fields + segmented `render_kind_btn`.
  State on `Workspace` (`show_create_run_config`, `run_config_*_input`), rendered at root
  (workspace.rs ~3033), opened by `open_create_run_config_modal`.
- Workspace already holds `db_observer: Entity<DbObserverPanel>` (workspace.rs:449) → modal can
  call `db_observer.update(cx, |p,cx| p.add_source(src, cx))`.
- Cross-panel channels: `ChromeRequest` (hides; DbObserver already emits `HideRightDock`) and
  `OpenRequest` (center tabs). Modal must be **window-level** (not inside the narrow right dock,
  which would clip the ~700px dialog) → host on Workspace, trigger via a new `ChromeRequest`.

## Plan

### Part 0 — driver: test + DSN assembly (`db_source.rs`)
- `pub fn test_connection(src: &DataSource) -> Result<String, String>` — returns a server-version
  string; run off the UI thread by callers (same `background_executor` pattern as `load_schema`).
  - SQLite: `open_ro` + `SELECT sqlite_version()` → `"SQLite {v}"` (also proves the file opens RO).
  - Postgres: `connect(cfg)` + `query_one("SELECT version()")` → short `"PostgreSQL 16.12"`.
- `PgConfig::from_parts(host, port, db, user, password)` → assembles
  `postgresql://user:pass@host:port/dbname` with a tiny local percent-encoder for user/pass
  (no new dep). `label` from `pg_label`. Keep raw-DSN path (paste overrides parts).
- Tests: `from_parts` DSN shape + encoding; `test_connection` for SQLite (PG needs a live server → skip).

### Part 1 — Add-Data-Source modal (new `db_add_source.rs`, hosted on Workspace)
- `AddSourceForm` struct (new module) holds: `driver: Driver{Sqlite,Postgres}`, the `InputState`
  entities (sqlite path; pg name/host/port/database/user/password/url), and
  `test: TestState{Idle,Testing,Ok(String),Err(String)}`.
- Workspace gains `add_source_form: Option<AddSourceForm>` + `render_add_source_modal(..)`
  (mirrors run-config modal) wired into the root `.when(..)` layer.
- Modal UI: segmented **driver toggle**; SQLite → path input + **Browse…** (native picker);
  Postgres → Name, Host, Port, Database, User, Password (masked if `InputState` supports it,
  else plain), URL (editable, overrides parts). Inline **Test Connection** result line.
  Buttons: Test / Cancel / Save.
- Trigger: replace DbObserver header `+ File`/`+ Connect` with one **`+`** button emitting new
  `ChromeRequest::OpenAddDataSource`; Workspace `subscribe_in` opens the form.
- Save: build `DataSource` (Sqlite path, or `PgConfig::from_parts`/raw URL) →
  `db_observer.update(|p,cx| p.add_source(src, cx))` → close modal.
- Remove now-dead DbObserver bits: `connect_open`, `dsn_input`, `toggle_connect`,
  `render_connect_bar`, `pick_sqlite` (moves into modal), old `add_button` uses.

### Part 2 — clean tree (`db_observer.rs`, item 3)
- **Schema layer:** when `TableMeta.schema.is_some()` (Postgres), group tables/views under a
  Schema node (`public`, …). SQLite (`schema=None`) stays flat.
- **Per-table sub-folders:** replace direct column children with
  `Columns (N)` → columns, `Keys (N)` → PK/FK cols (only if any), `Indexes (N)` (existing).
- **Glyph icons** per `NodeKind` (DB / Schema / Table / View / Columns / Column / Key(gold) /
  Index) via `kind_color`; prefer geometric/text glyphs over emoji for render reliability.
- Update `NodeKind`, `build_tree`, `push_schema`, `render_row`, `kind_color`, and the two
  `build_tree` unit tests (columns now under a Columns folder; add a schema-grouping test for PG).

### Part 3 — out of scope
- Center data grid restyle (already clean). Editing/writing data (driver is read-only by design).

## Build / verify
- `cargo build -p moonlight-desktop`, `cargo test -p moonlight-desktop`.
- Manual: add a SQLite file + a Postgres DSN via modal, Test both, expand tree to Columns/Keys.

## Open questions for operator
- Confirm the two assumptions above (PG structured+DSN; full-parity tree).
- Password masking depends on gpui-component `InputState` API — verify `.masked()` exists during impl.
