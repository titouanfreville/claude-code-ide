# SCOPE — Observed external CC sessions (catalog) + per-session rename/color

> Status: **Slices 1–2 BUILT** (rename + color, 2026-06-04, green); slices 3–5 (catalog) **scoped, not built**.
> Decisions locked with the operator. Engine+UI lanes. See `00-status-and-tasks.md` for the 1–2 build notes.
> Companion to the boot-rehydration fix (see `00-status-and-tasks.md`), which restored *managed* sessions
> on boot. This scope covers the **"or in CC" half**: surfacing **all** CC sessions (not just app-managed),
> plus the operator's new ask to **rename/color** a session.

## Operator decisions (locked)
1. **Catalog UI surface** = a **center tab** opened from a header button (e.g. "☰ Sessions" / "History").
2. **History depth** = **recent window, default 30 days** (older sessions still resumable via CLI).
3. **Metadata scope** = **per-space** (`.moonlight/`), and **use CC's native rename** for the name.
4. **Rename/color applies to ALL sessions** (managed + observed), not just managed.

## Findings that shape the design (verified 2026-06-04)
- **Detection already discovers active external sessions.** `JsonlDetectionSource` (`crates/detection`) scans
  `~/.claude/projects/**/*.jsonl` and emits `Discovered` for any transcript modified within its 6h
  `active_window`; the supervisor adds them as **observe-only** tiles (unadopted → PDP never gates). So
  *recently-active* external sessions already appear in the fleet. The gap is **history beyond the window**
  + **persistence** + **an explicit browse/adopt surface**.
- **Cheap per-session metadata, no full parse:** each `~/.claude/projects/<encoded-cwd>/<id>.jsonl` gives
  `sessionId` (= filename stem), `cwd`, `aiTitle`, `gitBranch` from a small head slice, plus file **mtime**
  (last-active) + size. **Catalog = 432 sessions / 488 MB total → metadata-only scan, never tail.**
- **CC native rename = name-only, write-through, NOT read-back.** CC supports `/rename`, `Ctrl+R` in the
  resume picker, and `claude -n <name>`. The name is stored in an **undocumented internal store** — NOT in
  the transcript and NOT in `~/.claude/sessions/<pid>.json` (which holds `{pid, sessionId, cwd, startedAt,
  status, updatedAt, version, kind}` — no name). ⇒ We can **set** a CC name (launch `-n`, or send `/rename`
  into the embedded terminal) but must **mirror it locally** for display. **CC has NO color** → color is ours.
- **`~/.claude/sessions/<pid>.json` is a free live-status registry** (sessionId + cwd + `status` +
  `updatedAt`). Use it for the catalog's "running now?" column without tailing. Keyed by pid; multiple pids
  per sessionId possible → take the latest `updatedAt`.

## Data model
- **Per-space `.moonlight/session-meta.json`** (NEW; separate from the managed roster `sessions.json` so the
  display layer covers observed ids too): `{ "<session-id>": { "name": Option<String>, "color":
  Option<Accent>, "name_source": "cc" | "local" } }`. Defensive IO (empty on missing/unreadable), self-ignored
  under `.moonlight/` (already `.gitignore *`). `Accent` = a small fixed palette enum (theme accents), not raw
  hex, so colors stay on-theme and serialize stably.
- The existing managed roster `.moonlight/sessions.json` stays as-is (membership + resume snapshot). `label`
  there remains the managed fallback; `session-meta.json` is the authoritative display name/color for ALL
  sessions in the space.

## Work breakdown (each a testable slice; 1–2 ship rename/color independently)
> ✅ **Slices 1 & 2 done** (2026-06-04). 3–5 remain. The slice-2 build deviated slightly: rename/color live in
> the **session focus view** (`session_monitor.rs`); surfacing them on fleet tiles is a noted follow-up.
1. **Per-space metadata store** — `apps/desktop/src/views/session_meta.rs` (gpui-free core + unit tests):
   `SessionMeta { name, color, name_source }`, `load(root) / upsert(root, id, meta) / get(id)` over
   `.moonlight/session-meta.json`. No engine changes.
2. **Rename + color UI** (the operator's new ask) — in the **session focus view** (`session_monitor.rs`) and
   on fleet tiles (`grid_home.rs`): an edit affordance (rename field + a color swatch picker from the accent
   palette). Writes the store → tiles/monitor/status-bar re-render (they already `cx.observe`). For a
   **live/managed** session ALSO write-through CC native rename: send `/rename <name>` into the embedded
   terminal via `ShellDeps::session_io` (so CC's own picker matches); `name_source="cc"`. Color is local-only.
   Display everywhere prefers `session-meta.name` → CC `aiTitle` → short id.
3. **Catalog scanner** — `apps/desktop/src/views/session_catalog.rs` (gpui-free core + tests): lazy scan of
   `~/.claude/projects/*` dirs, **30-day mtime window**, **space-scoped** (entry kept iff its `cwd` == active
   space root; Overview = all). `CatalogEntry { id, project_root, name, ai_title, git_branch, last_active,
   size, live_status, is_managed }`. Cheap: readdir + mtime + ≤4 KB head/tail for `cwd`/`aiTitle`/`gitBranch`;
   merge live `status` from `~/.claude/sessions/*.json` (by sessionId, latest `updatedAt`); merge name/color
   from `session-meta`. **Decode cwd from the transcript head, NOT by un-dashing the dir name** (dir encoding
   is lossy when a path contains `-`). Run off the GPUI thread; cache keyed by (path, mtime). Cap + "show more".
4. **Catalog center-tab UI** — new `OpenRequest::SessionCatalog { space_root }` (`center_requests.rs`) +
   `views/panels/session_catalog.rs` render: a header button ("☰ Sessions") opens the tab; rows sorted
   newest-first, grouped by project (Overview) or flat (space), each showing color dot · name/title · branch ·
   last-active · live-status · managed badge; **Resume** action → `OpenRequest::Session` (embedded
   `claude --resume <id>` rooted at `cwd`). Reuses the space-scoped tab machinery.
5. **Adopt-from-catalog** — a per-row **Adopt** → write a managed record (`store.upsert_managed` +
   `record_managed_session`) so the session joins the governed fleet (then boot-rehydration keeps it). Optional
   `Command::AdoptObserved { id, root }` if engine-side seeding is cleaner than UI-side store write.

## Risks / open questions
- **CC name divergence:** if the user renames in CC's own picker (outside the app), our mirror goes stale
  (no reliable read-back). Acceptable v1; a later poller could reconcile. Surfaced, not solved.
- **cwd→space mapping:** rely on the transcript's `cwd` field, not the encoded dir name (lossy). Confirm a
  cheap head-read reliably yields `cwd` on the first ~4 KB (it did in sampling).
- **Scan cost:** 432 files × metadata-only is cheap, but still do it async + cached; never on the render path.
- **Live-status staleness:** `sessions/<pid>.json` may linger after a crash; treat `updatedAt` age > window
  as not-live.

## Non-goals (v1)
- Full-text/transcript search; cross-machine catalog; editing CC's internal name store directly; color in CC
  (CC has none — ours only).

## Suggested order
Ship **1 + 2** first (rename/color — self-contained, immediate UX win, no scan), then **3 → 4 → 5**
(catalog → browse → adopt). Slices 1 & 3 are pure/gpui-free → unit-tested like the rest of the engine work.
