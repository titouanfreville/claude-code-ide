# MoonlightCode — Status & Task List

> Canonical "where we are + what's next" so either session/agent can resume after a
> context clear. Update the **Status** date + check off tasks as they land.

**Last updated:** 2026-06-05 (**Git tool window DONE** — bottom tool: Log view (50 commits) + Console view fed by the shared GitConsole sink (toolbar checkout + Commit-tool ops record there); the stripe map is now fully live except Debug (DAP). Before that: Commit, Problems, Services tool windows, stripe re-layering slice 1) · **Build:** green · **Tests:** engine 27 / desktop **162** / domain 7 / trust 10 / persistence 7 / mcp-server **13** / control **68**
**Verify:** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test` · run app via RustRover ▶ "Run moonlight"

### Working rules
- Build/test every slice; report results faithfully. Coordinate on the UI lane (read-before-edit / handoff).

## STUCK/INCOMPLETE SESSION WARNING SIGNAL — Slices 1+3 (foundation + self-report) DONE (2026-06-08) — green, 356 tests (+8); Slice 2 (auto-detect) remains
**Self-report vertical SHIPPED** (the operator's literal ask: "CC can indicate a locked session"):
- **domain** `AttentionKind`{NeedsInput,Stuck,Incomplete,Errored} + `from_status`/`severity`/`glyph`/`label`/`is_warning` (re-exported). **`McpVerb::ReportBlocked`** (Safe · `is_read_only=true` so it passes the frozen-phase gate · `min_autonomous_tier=Observed` → never gated; a cry for help runs in any phase/tier, no approval).
- **engine** `EngineEvent::SessionAlert{session, Option<AttentionKind>}` + `Command::FlagSession{…}` + supervisor `flag_session` (republishes; transient, not persisted).
- **mcp-server** `danger_class(ReportBlocked)=Safe`; `report_blocked` transport tool (`reason` param) + instructions. **Executor:** `PhaseVerbExecutor` (now the "engine-command verbs" layer) handles `ReportBlocked` → `Command::FlagSession{Stuck}`, echoes/audits the reason.
- **UI** `grid_home::FleetModel` folds `SessionAlert` into an `alerts: HashMap` overlay; **edge-triggered clear on transition→Running** (so a Stuck reported mid-turn doesn't flicker); `attention(session)` = overlay **or** `from_status`. `session_tile` draws a ⚠ badge for `is_warning()` kinds (Stuck/Incomplete amber/red) beside the status badge. +8 tests (domain 2, grid 3, executor 1, + earlier).
- ⚠️ **Runtime-verify (live CC):** a managed session calls `report_blocked` → ⚠ appears on its grid tile (any phase, no approval); the agent resuming work (→Running) clears it; an Errored session shows ⚠ too.
- **Slice 2 (IDE auto-detect) — DONE (2026-06-08), 361 tests:** detection-side **stall → `Incomplete`** (collision-free; domain+detection+engine only). New `DetectionEvent::Alert{session, Option<AttentionKind>}`; `JsonlDetectionSource` flags a session **handed a turn (a `user` line — operator prompt or tool result) that then stays silent ≥ `DEFAULT_STALL_WINDOW` (180s)** → `Alert{Incomplete}`, cleared (`Alert{None}`) when it writes again. Edge-emitted via `FileState.alerted`; `with_stall_window` for tests (+1). Supervisor maps `DetectionEvent::Alert` → `flag_session` → `EngineEvent::SessionAlert` (same path as self-report). Also fixes the "session stuck on a prompt shows Running forever, no signal" hole. **Conservative by design:** a long *tool* runs under an `Assistant` turn (not `User`) so it's never mis-flagged; the `Assistant`-mid-generation kill is NOT caught (indistinguishable from a normal your-turn — **no Stop hook is installed**, so `StopClean` isn't reliable). ⚠️ Runtime-verify: kill a managed CC right after it's handed a turn → after ~3 min its tile shows ⚠ Incomplete; resuming clears it.
- **REMAINING (follow-ups):** (a) **definitive PTY-exit detection** (managed terminal child exits → `Incomplete`) — needs `TerminalPanel`/`SessionIo` exit wiring, coordination-sensitive (touches the session-owner lane); (b) **`Assistant`-mid-generation interrupt** — needs a reliable end-of-turn marker (install a Stop hook, or track pending `tool_use` with no result); (c) **space-tab attention dot** for the overlay (plumb the alert state into `workspace`'s SpaceTab builder; today it only covers Waiting/Errored from status). Still OUT of `session_monitor.rs` (other lane).

**Modeling note:** NO new `Session` struct field (would force editing `session_monitor.rs::seed_session` — the other lane's file — + persistence). Non-status alerts ride a **live engine event + UI overlay**, like `status` is recomputed; `NeedsInput`/`Errored` derive from `SessionStatus`. **COORDINATION:** another session reworks the **session header (`session_monitor.rs`)** + session-restore — this lane stays OUT of that file (warning UI = grid tile, + space-tab dot later).

## LAUNCH-LINE FIX + SERVICES UI REDESIGN (JetBrains Services) — DONE (2026-06-08) — green, 340 tests, clippy clean (my files)
**Two operator bugs/asks:**
1. **Session would not open — dangling quote.** Root cause: the `--append-system-prompt '<~1.2KB IDE_CONTEXT>'` was injected **inline** into the launch command, which is *typed into the embedded shell as one line* (`emu.write_str("{cmd}\n")`); the long line overflowed the terminal's line-input limit and truncated mid-string, leaving an unterminated quote so `claude` never ran. **Fix (`mcp_host.rs`):** write `IDE_CONTEXT` to `<support>/session-context.txt` (like the statusline `--settings` file pattern — `obs::support_dir` made `pub(crate)`) and pass **`--append-system-prompt "$(cat '<path>')"`** so the typed line stays short. Short apostrophe-free `IDE_CONTEXT_SHORT` inline fallback if the file can't be written. New tests: launch flag stays <400 chars; `session_launch_flags` quotes are balanced (single+double both even).
2. **Services view unreadable → redesigned to JetBrains' Services tree** (`panels/services.rs`, via /frontend-design). Was: ragged two-line blocks, almost all text at 10px, low-contrast muted everywhere, weak hierarchy. Now: **fixed-height (24px) single-line rows** with a shared `row(depth)` builder (depth indent + aligned `disclosure`/`dot_slot` leading column so triangles & status dots line up); **category headers** (`group()` helper) in medium-weight 13px with a faint top hairline separating them; **monospace** column for paths/urls/ids/log summaries (Menlo); readable type (11–13px, `text_primary`/`secondary` not `muted`); flat status dots colour-coded (Done/Errored/Idle/Running); **pill `op_button`s** (bordered, accent hover). Four categories: Control server (singleton row) · MCP servers (endpoints → verb-log children at depth 2) · Docker (containers + ops) · HTTP (call history). Data/snapshot/poll layers + all tests unchanged.
- ⚠️ **Runtime-verify** (RustRover ▶ Run moonlight): (1) launching a managed session now opens cleanly (no `quote>` hang); the agent still gets the phase/MCP context (it reads the session-context.txt file). (2) ⚙ Services reads as a clean JetBrains-style tree: aligned triangles/dots, mono paths/urls, legible rows, collapsible categories, container op-pills.

## SERVICES VIEW ENRICHMENT — Slices A+B (MCP names+logs, Docker+ops) — DONE (2026-06-08) — green, 329 tests (+14), clippy clean (my files)
**Operator RQ:** Services view is too empty; add Docker, k8s, HTTP history, local MCP servers w/ logs+names, CC skills. **Phase-1 scope chosen:** MCP(names+logs) + Docker(+ops) + HTTP history(actionable). k8s + CC-skills deferred. Panel restructured into **collapsible sections** (`panels/services.rs`).
- **Slice A — restructure + MCP section:** `ServicesPanel` now holds per-section state/collapse + a richer `Snapshot`. The MCP section lists each embedded per-session host with its **friendly name** (`store.managed(id).title`) and a **live verb-call log** (`store.recent_audit(id, 6)` → classified `LogLine`s: ran/failed/denied/approved/phase/feedback, glyph+colour, relative "ago" computed at render so the snapshot only changes on real rows). Control server stays a single always-on row. Pure helpers `log_line`/`rel_ago`/`verb_label`/`clip` + tests (6).
- **Slice B — Docker section + ops:** NEW `src/docker.rs` (GPUI/tokio-free): `probe()` shells `docker ps -a --no-trunc --format '{{json .}}'`, `parse_ps` (serde, skips garbage lines), `compact_ports` (dedup/sort published host ports), `compose_project` (from labels), `run_op(Start/Stop/Restart)`, `logs_command`. Unavailable (CLI absent/daemon down) → calm hint. Panel runs the docker probe on a **background-executor task** (never the foreground poll; 3s) + own collapse flag. Container rows: lamp(running)+name+status+image/ports/compose, with **start/stop/restart** (off-thread + immediate re-probe) and **logs** → streams `docker logs -f` into the shared **Run console** (`run_registry.start`). New `op_button` helper. Tests (4).
- ⚠️ **Runtime-verify** (RustRover ▶ Run moonlight): (1) ⚙ Services → MCP section shows launched sessions by name + a live log as the agent calls verbs (try `request_phase`/`run_*`); (2) with docker running, the Docker section lists containers, lamps match state, stop/start/restart work and re-probe within ~1s, logs opens a `docker logs` tab in the Run console; (3) with docker stopped/absent → "docker not available" hint, no jank.

## SERVICES VIEW — Slice C1 (HTTP / Postman-lite foundation) — DONE (2026-06-08) — green, 339 tests (+10), clippy clean (my files)
Operator reshaped "HTTP request history" → **Postman-inspired**: variables + a variable manifest (environments). **Decided:** client = **ureq** (blocking via `spawn_blocking`; reuses rustls); UI = **center-tab builder (C2) + compact Services summary**; **foundation first (C1)**.
- **`ureq = "2"`** added (workspace + apps/desktop).
- **NEW `src/http.rs`** (GPUI-free): `HttpHistory` shared ring (`record`/`len`/`recent`, cap 50) of `HttpCall{method,url,status,ms,ok,bytes,at_millis,error}` (no req headers/body kept → no token leakage). Manifest `HttpManifest{active, environments{HttpEnv{vars, allowed_hosts}}}` + `load_manifest(root)` from `.moonlight/http/environments.json` (missing/bad → empty default). `interpolate({{var}})`, `host_allowed` (**SSRF gate:** allowlist by exact/dot-suffix; **empty allowlist ⇒ loopback/private only**), `parse_payload` (JSON `{method,url,headers,body}` OR bare URL→GET), `send` (ureq, 5 redirects, 15s). 7 tests.
- **NEW `src/http_verbs.rs` `HttpVerbExecutor`** — handles `McpVerb::HttpRequest`: parse → load manifest at session root → interpolate vars → **host-scope check (refuse before any network call)** → `spawn_blocking(send)` → record to history → compact `method url → status · ms · bytes` + body preview. Delegates others. 3 tests. Wired into the executor stack in main.rs: **phase → http → run → shell**.
- **`HttpHistory` added to `ShellDeps`** (+ init_shell param + main.rs build/clone — one in the executor, one in the shell). **Services HTTP section** (`services.rs`): collapsible; header `env: <name> · N calls`; recent rows (method · url · status-colored · ms · ago). Snapshot gained `http_env/http_total/http_calls` (env name loaded from the active space's manifest each 2s poll).
- **REMAINING — C2 (the Postman UI):** a dedicated **center "HTTP" tab** (request builder: method+URL bar with `{{var}}` autocomplete, Params/Headers/Body tabs, Send, response pane status/timing/headers/body), **save requests** to a `requests.json` manifest, env switcher, and an **[open ▸]** button on the Services HTTP summary to launch the tab. Operator-`Send` should reuse the same `http::send` + `HttpHistory` so agent + operator calls share one history.
- ⚠️ **Runtime-verify (live CC):** with a managed session, call `http_request` `{"url":"http://localhost:<port>/..."}` → it runs and the Services HTTP section shows the call (status/ms); a public host (`https://api.github.com`) is **refused** ("host not allowed") unless added to `.moonlight/http/environments.json` `allowed_hosts`; `{{base_url}}` interpolates from the active env.

## DEFAULT SESSION CONTEXT — IDE-managed sessions launch knowing the phases + MCP — DONE (2026-06-08) — green, 316 tests (+2), clippy clean (my files)
**Operator RQ:** "provide some context data per default to sessions owned in the IDE — encourage MCP use, explain the phase part + its usage." Mechanism: `--append-system-prompt` at every managed launch site (per-session, never touches the project CLAUDE.md).
- **`views/mcp_host.rs`:** new `const IDE_CONTEXT` (one paragraph: the 6-phase gate + what each allows + that the agent cannot self-switch + use `request_phase` (operator-approved) + prefer the `moonlight` run verbs over ad-hoc shell + denials carry reasons). **Single line, no apostrophes** — the launch command is *typed into the embedded shell* (`emu.write_str("{cmd}\n")`) inside a single-quoted arg, so a literal newline would submit early and an apostrophe would break quoting (test guards both). New `ide_context_flag()` (` --append-system-prompt '…'`) + **`session_launch_flags(url)` = `mcp_config_flag(url)` + `ide_context_flag()`** — they ride together (guidance only makes sense when the verbs are wired). +2 tests.
- **All three launch paths now call `session_launch_flags`** instead of `mcp_config_flag`: `attach_command` (covers session_monitor `new_in`/restore + workspace resume/registry arms), `relaunch_terminal` (`--resume … --permission-mode`), and the `NewManagedSession` arm (workspace.rs). Gated on `mcp_url.is_some()` (host healthy) exactly as the MCP flag was — degraded no-MCP launches still omit it (no misleading verb talk).
- ⚠️ **Runtime-verify (live CC):** launch a managed session → its first turn already knows the phase model + verbs (ask it "what phase are you in and how do you change it?" → it should cite `request_phase`); confirm the typed launch line is one shell command (no premature submit) and `claude` accepts the `--append-system-prompt` arg.

## MCP `request_phase` — CC can REQUEST a phase change (operator-approved) — DONE (2026-06-08) — green, 314 tests (+8), clippy clean (my files)
**Operator RQ:** (1) confirm/expose the IDE MCP to CC — already built + injected per managed session via inline `--mcp-config` (this very terminal session sees `mcp__moonlight__*`; `denied: approval window elapsed` on a live `run_status` proved the resolve→PDP→keystone→audit loop). NB: terminal-launched `claude` *outside* the IDE can't reach it (ephemeral per-session port, in-process host) — a fixed-port/persistent host is the follow-up if wanted. (2) "CC can't change phase — should not do so alone, but be able to when required." Decisions: **any target phase** (not just next); **always operator-approved** for now (a UI auto-phasing toggle is future — the PDP branch is the seam).
- **Domain** (`trust.rs`): new `McpVerb::RequestPhase` (`min_autonomous_tier=Trusted`, `is_read_only=false`) + **`McpVerb::is_phase_control()`**. `phase.rs`: **`Phase::from_token(&str)->Option<Phase>`** (tolerant: labels + `auto`/`autoimplement`/`implement` aliases; `next` is *not* here — the executor handles it). +tests.
- **PDP** (`crates/trust`): new **step 1b control-plane branch** — `if verb.is_phase_control() → Prompt` *before* the write gate, so a frozen phase (Discovery/Plan/Commit) can't deny the request (else a session could never ask its way out) and it never reaches `Allow` on its own. +2 tests (prompts in every phase incl. frozen; prompts regardless of tier).
- **mcp-server**: `danger_class(RequestPhase)=Safe` (approval comes from the PDP branch, not danger). `transport.rs`: `RequestPhaseParams{phase}` + **`request_phase` tool** (phase = `discovery|plan|auto|test|review|commit|next`, empty=next) + instructions string.
- **desktop**: NEW **`phase_verbs.rs` `PhaseVerbExecutor`** — wraps the executor stack (phase → run verbs → shell run_with_coverage); on `RequestPhase` emits `Command::AdvancePhase` (next/empty) or `Command::SetPhase{phase}` (named token), unknown→refused with the valid set, never a silent no-op; +4 tests. `main.rs`: **command channel hoisted above the actor** so the executor holds `commands.clone()` (same channel the cockpit phase controls use); `mod phase_verbs;`.
- **Flow at runtime:** CC calls `request_phase` → PDP `Prompt` → `KeystoneApprovalGate` raises a cockpit approval (45s hold) → Approve → executor sends the engine Command → supervisor `apply_phase`/`advance_phase` transitions + audits; Deny refuses with the reason. The operator stays the authority.
- ⚠️ **Runtime-verify (live CC):** in a managed session, `/mcp` lists `request_phase`; call it (`phase:"auto"` from Plan) → a cockpit approval appears → Approve → the session's phase chip moves to Auto + a PhaseChanged audit row lands; Deny → the agent gets `denied: …`; an unknown token returns the valid set without moving anything.

## UI — GIT TOOL WINDOW (stripe re-layering follow-up) — DONE (2026-06-05) — green, desktop 162 (+3), clippy clean (my files)

**What:** The ⎇ stripe slot is live — a bottom-dock Git tool window with two views behind chips:
- **Log**: the active space's last 50 commits (`git/log.rs`: `LogEntry` + NUL/`\x1e`-separated
  `--pretty` parse, +2 tests) — accent hash · refs badge (`HEAD -> main, origin/main`) · subject ·
  author · relative time. Root-scoped, follows space switches; 3s poll, rebuild only on change.
- **Console**: every git op the IDE itself runs, with outcome — **errors stop being toast-only**.
  New shared **`GitConsole`** sink (`views/git_console.rs`: `Arc<Mutex<Vec<GitOp>>>` + atomic dirty
  seq, ring-capped 200, +1 test) in `ShellDeps`; recorded from: **toolbar checkout**
  (`workspace::checkout_branch`) and the **Commit tool**'s stage/unstage/commit (`record_op`). ✓/✘
  rows with the detail line (stderr red).

**Wiring:** `BottomTool::Git` + workspace-owned `GitPanel` entity + dock arm; live ⎇ `branch_icon`
stripe button (the now-unused `glyph_icon` removed — only Debug still uses `reserved_btn`).
⚠️ **Runtime-verify** (RustRover ▶ Run moonlight): (1) ⎇ fronts the Git window; Log lists this repo's
history with the refs badge on HEAD; (2) switch space → log follows; (3) checkout a branch from the
toolbar → a ✓ `git checkout <branch>` row lands in Console (try a dirty-tree failing checkout → ✘ row
with stderr AND the toast still fires); (4) stage/unstage/commit in the Commit tool → rows appear;
(5) chips switch views; hide ✕ collapses the dock.

## UI — COMMIT TOOL WINDOW (stripe re-layering follow-up, the operator's "hybrid") — DONE (2026-06-05) — green, desktop 159 (+2), clippy clean (my files)

**What:** The ⎘ stripe slot is live — JetBrains' Commit view as the **left dock's alternate tool**
(Project ⇄ Commit swap), closing the "staging lands in the index but commit is manual" gap:
- **Index-faithful split**: *Staged* / *Changes* groups from porcelain **XY** (new `git/commit.rs`:
  `CommitEntry{rel, staged, unstaged}` + `parse_entries` keeps both sides — a file can be in both).
  The point is coherence with the review flow: review-tab **Accept stages hunks** → they appear under
  *Staged* → **Commit commits exactly the index**, never a blind `-a`.
- Per-row **＋ stage / − unstage** (`git add` / `git restore --staged`; **unborn-branch fallback** to
  `git rm --cached` when there's no HEAD yet — found by the temp-repo round-trip test); row click opens
  the file in an editor tab; VCS-colored status letters (file-tree language).
- **Message input + Commit button** (gpui-component `Input`, lazily created on first render): disabled
  until something is staged + a message typed; result line shows git's summary or stderr inline; message
  clears on success.
- **Review changes ▸** (the hybrid's center half): enabled when a session tab is focused — emits
  `OpenRequest::CodeReview{session, root}` so the rich per-hunk review opens from here. No session →
  explanatory hint instead.

**Wiring:** `LeftTool::{Project,Commit}` on the workspace (mirrors `BottomTool`): `set_left_tools` now
fronts either [tree(÷structure)] or [commit]; `toggle_left_tool` = JetBrains stripe behavior (front /
swap content / collapse when already frontmost); `toggle_structure` fronts Project first (the outline
lives under the tree). Commit panel **registered** for layout restore; 2s status poll + immediate
refresh after every index op; uniform hide ✕ (`HideLeftDock`). Rail: live ⎘ commit-node icon
(lit-aware), Project button now lights only when *Project* fronts the open dock. +2 tests
(`parse_entries` XY matrix; real temp-repo stage→commit→unstage round-trip incl. unborn-branch unstage).
⚠️ **Runtime-verify** (RustRover ▶ Run moonlight): (1) ⎘ swaps the left dock to Commit (tree button
swaps back; collapse on second click); (2) edit files → they appear under *Changes* within ~2s; ＋ moves
one to *Staged*, − back; (3) Accept a hunk in a review tab → the file shows under *Staged* here; (4) type
a message → Commit enables → commits the index; summary line appears; `git log` confirms; (5) with a
session tab focused, "Review changes ▸" opens the per-hunk review on this repo; (6) on a fresh repo with
no commits, − (unstage) still works (rm --cached fallback).

## UI — PROBLEMS TOOL WINDOW (stripe re-layering follow-up) — DONE (2026-06-05) — green, desktop 157 (+4), clippy clean (my files)

**What:** The ⚠ stripe slot is live — JetBrains' Problems view as the bottom dock's fourth tool window:
**LSP diagnostics grouped by file** with severity glyphs (✘ red / ⚠ amber / ℹ blue / ➤ hint), 1-based
line:col, message + `[source]`, file headers with per-severity counts, and **click-to-open** (emits
`OpenRequest::File` on the center channel — line-jump is a follow-up; the editor opens at top).
**Scope (by design):** servers only diagnose `didOpen`ed documents → this is *problems in files opened in
an editor tab*, not a workspace sweep (watched-files support = later slice). Empty state says so.

**Plumbing (the real meat):**
- `lsp/protocol.rs` — `Diagnostic` struct + `parse_diagnostics` (publishDiagnostics → rows, empty = clear)
  + `uri_to_path`; +2 tests.
- `lsp/client.rs` — the **reader thread now consumes `publishDiagnostics`** into a shared
  `Arc<Mutex<HashMap<uri, rows>>>` + `AtomicU64` dirty seq (requests never see them; diagnostics stay live
  with no request in flight). Initialize capabilities advertise `publishDiagnostics`. New
  `diagnostics()` (cleared files dropped, sorted) + `diagnostics_seq()`.
- `lsp/mod.rs` — pool-level `diagnostics()` / `diagnostics_seq()` (merge across live clients);
  `Diagnostic` re-exported.
- **`ShellDeps.lsp_pool`** (NEW) — the one shared `Arc<LspPool>`; `StructurePanel` now uses it (falls back
  to its own in static/test views), so the outline's servers are the same processes feeding Problems.
- `panels/problems.rs` (NEW) — workspace-owned bottom tool (1.5s poll on the pool seq, rebuild+notify only
  on change), `Counts` + `lamp()` (red errors > amber warnings > none); uniform hide ✕; +2 tests.
- `workspace.rs` — `BottomTool::Problems` + entity (observed → chrome lamp live) + dock arm;
  `RailSnapshot.problems_lamp`. `activity_rail.rs` — live ⚠ button with corner lamp (run-icon pattern).
⚠️ **Runtime-verify** (RustRover ▶ Run moonlight): (1) open a Rust file with an error (e.g. an undefined
ident) in an editor tab → within a few seconds the ⚠ stripe lamp goes red and the Problems window lists
the file + rows from rust-analyzer; (2) fix the error → rows clear, lamp goes out (server publishes empty);
(3) click a row → the file opens/fronts in the center; (4) the Structure outline still works (same pool);
(5) clean files → "No problems" empty state, Terminal/Run/Services unchanged.

## UI — SERVICES TOOL WINDOW (stripe re-layering follow-up) — DONE (2026-06-05) — green, desktop 153 (+2), clippy clean (my files)

**What:** The ⚙ stripe slot is live — a JetBrains-style **Services** view as the bottom dock's third tool
window, showing the app's long-lived background services with status lamps:
- **Control server** (hook IPC): lamp from a **unix-socket connect probe** (`probe_socket`) on
  `~/.moonlight/control.sock` (`main.rs::control_socket_path` now `pub(crate)`) — green "listening" =
  exactly what the hook CLI sees (this instance's server *or* another's, since the spawn refuses to hijack).
  Note: a connect-probe is "reachable now", not liveness proof — macOS can briefly accept on a dead file.
- **MCP host**: runtime up/down (`ShellDeps.mcp_host`) + the live **per-session endpoints** (short id +
  URL) via new `McpHostHandle::endpoints()` (sorted snapshot of the URL cache); empty-state hint when up
  but no sessions yet.

**Pieces:** `panels/services.rs` (NEW — workspace-owned `Render` entity like Terminal/Run, **2s poll**
that notifies only on snapshot change; own bar with the uniform hide ✕ → `HideBottomDock`; +2 tests:
socket probe, short-id) · `BottomTool::Services` + `services` entity + dock body arm (`workspace.rs`) ·
rail: `reserved_btn` → live `rail_btn` with lit-aware `gear_icon` (`activity_rail.rs`).
⚠️ **Runtime-verify** (RustRover ▶ Run moonlight): (1) ⚙ stripe button fronts the Services window
(lit), toggles the dock; (2) Control server row green "listening" while the app runs (red "no listener"
if you delete/another instance owns `~/.moonlight/control.sock`); (3) MCP host row green "up"; launching
a managed session adds its endpoint row (short id + http://127.0.0.1:…/mcp) within ~2s; (4) the bar's ✕
hides the bottom dock; (5) Terminal/Run behavior unchanged.

## UI — JETBRAINS STRIPE RE-LAYERING, SLICE 1 — DONE (2026-06-05) — green, desktop 151, clippy clean (my files)

**What:** First slice of the operator-approved re-layering toward the JetBrains New-UI reference
(`JetBrains model interface with structure.png`). Feature decisions (operator interview): left stripe top =
Project · Commit · Structure · Fleet; bottom = Terminal · Run · Problems · Git · Services · Debug; right
stripe = **DB observer only**; status bar **kept at 30px** (readability > density — most surfaces are
resizable anyway). Commit tool (future slice) = **hybrid**: left-dock changed-files/staging list, rich
per-hunk review stays a center tab.

**Pieces:**
- **`views/chrome_requests.rs` (NEW)** — `ChromeRequests` entity + `ChromeRequest::{HideLeftDock,
  HideStructure, HideBottomDock, HideRightDock}`: the chrome twin of `CenterRequests`. Tool-window headers
  emit it; the workspace `subscribe_in`s (`handle_chrome`, idempotent hides). In `ShellDeps` (built in
  `init_shell`, no main.rs plumbing).
- **Activity rail** (`panels/activity_rail.rs`) — two JetBrains groups: **top** Fleet · Project ·
  Commit° · Structure (new `structure_icon`, indented-outline glyph); **bottom** Terminal · Run ·
  Problems° · Git° · Services° · Debug°. `°` = `reserved_btn` (0.4 opacity, tooltip "— planned", no-op)
  claiming the slot its tool window will occupy. New `right_stripe(db_open, cx)` — mirrored edge rail
  (hairline side via `EdgeSide`) with one DB cylinder icon → `toggle_db_observer`.
- **Structure = independent stripe toggle** (`workspace.rs`) — tree + structure entities are now
  **workspace-owned** (like the bottom tools); `set_left_tools(...)` (re)builds the left `DockItem`
  (tree alone ⇄ tree ÷ structure 220px) preserving dock width/open (`left_dock_geometry`). Used by the
  default layout, **after a layout restore** (the restored left dock is rebuilt around the owned
  entities), and by `toggle_structure` (turning the outline on also reveals a hidden dock).
- **Right dock** (`workspace.rs::toggle_db_observer`) — lazily `set_right_dock` (340px) hosting
  `DbObserverPanel` on the app's own `moonlight.db` (`main.rs::store_path` now `pub(crate)`).
- **Uniform tool-window hide ✕** (`panels/mod.rs::tool_hide_button`) — one quiet header button, same
  everywhere: file tree + structure + DB observer via `Panel::title_suffix` (DB only when
  `stripe_hide` — center-tab DB instances keep the tab ×), terminal tab strip + run onglet bar append
  it far-right. All routes through `ChromeRequests`.

**Still open in this lane (next slices, operator-picked):** Commit tool window (hybrid),
**Problems** (LSP diagnostics view — plumbing exists in `src/lsp`), **Git** (log + command console),
**Services** (MCP host/hook-server status), Debug = DAP phase 3. Also: `structure_open` isn't persisted
(defaults true each boot); right-dock DB panel does persist via layout dump ("DbObserver" registered) but
its `stripe_hide` flag is not restored (restored instance shows no hide ✕ — cosmetic).
⚠️ **Runtime-verify** (RustRover ▶ Run moonlight): (1) left stripe shows the two groups + greyed reserved
slots with "planned" tooltips; (2) Structure stripe button hides/shows the outline while the tree keeps the
dock (width preserved), and works after a restart (restored layout); (3) right stripe's DB button opens the
right dock on moonlight.db; toggle + header ✕ close it; (4) every tool window header shows the same ✕ —
tree/structure/DB via the dock title row, terminal + run bars far-right — and each ✕ hides the right thing;
(5) reserved buttons do nothing (no dead cursor-pointer).

## PLAN-GATE EXPLORATION FIX + SLICE 3 MCP WIRING — DONE (2026-06-05) — green, control 68 / desktop 139, clippy(control) clean
Operator RQ (hit live 4× while planning *in this very repo*): "plan mode must allow all exploration;
reject only project writes — except plans/specs." Plus the chosen next step: wire the built MCP host.
- **Part A — PDP/classify fixes** (engine lane, `crates/control`):
  - **Quote-aware Bash segmentation** (`classify.rs::split_segments`): `&&`/`||`/`;`/`|` split only
    *outside* quotes (backslash-escape aware) — `grep "a\|b" f | head` is one Safe pipeline again
    (was: split mid-pattern → unknown program → Risky → denied in Plan). Unbalanced quotes → Risky.
  - **MCP tools classified by leading verb** (`classify_mcp_tool`): `mcp__<srv>__{get,list,read,search,
    find,query,preview,describe,stat,show,view,lookup,fetch,inspect,hover}*` → Safe; trailing-verb names
    (`execute_sql_query`) deliberately stay Risky. Operator escape hatch: **`safe_tools: ["…"]`** in
    `.moonlight/config.json` (user+workspace layers appended) → exact tool names vouched Safe — threaded
    server→`evaluate(vouched_safe)`; the paused gate still denies everything.
  - **Home-anchored AI roots** (`paths.rs`): roots may be `~/`-anchored or absolute (matched against the
    write's absolute path); **`~/.claude/plans` added to `DEFAULT_AI_ROOTS`** — the harness plan file is
    writable in Plan phase (plans ARE that phase's output). Sibling `~/.claude/*` stays frozen.
  - +tests: quoted-operators, unbalanced-quotes, MCP-verb matrix, home-root scoping, safe_tools layers,
    and an e2e socket test (`plan_phase_allows_exploration_and_plan_writes_over_socket`).
  - ⚠ The hook runs the **installed debug binary** — rebuilt here; **restart CC sessions** to pick it up.
- **Part B — Slice 3 wiring** (composition root + additive HOT-lane edits):
  - `main.rs`: `BusNotifier` hoisted to a shared `Arc` (hook server + MCP gate); `ActorService` composed
    (DefaultPdp · `BusPolicyView` kept live by a bus-fold task · `ShellVerbExecutor` with `test_command`
    from `~/.moonlight/config.json` (default `cargo test`) · `StoreAuditSink(store)` ·
    `KeystoneApprovalGate(pending, notifier, DEFAULT_HOLD)`); **`McpHostHandle::build`** stands the host
    on its own 1-worker tokio runtime (leaked for app lifetime) → **`ShellDeps.mcp_host`**.
  - **`views/mcp_host.rs` (NEW)**: sync `url_for` (block_on ephemeral bind, per-session URL cache so a
    relaunch reuses the endpoint), `url_for_session` (ShellDeps global), `mcp_config_flag`. +2 tests.
  - **`--mcp-config '{"mcpServers":{"moonlight":{"type":"http","url":"…"}}}'` injected at every launch
    site**: NewManagedSession arm + registry restore arm + SessionById arm (`workspace.rs`),
    `attach_command` (new `mcp_url` param) / `try_resume` / `relaunch_terminal` / `new_observed`
    (`session_monitor.rs`).
- ⚠️ **Runtime-verify (live CC)**: (1) launch a managed session → `/mcp` lists `moonlight` with
  `run_with_coverage`; (2) call it → the test command runs in the session repo, the compact summary
  returns, a `VerbExecuted` audit row lands in moonlight.db; (3) a `Prompt`-class verb (low tier /
  frozen phase) raises a cockpit approval (45s hold) — Approve runs it, Deny refuses with the reason;
  (4) gating: in Plan, quoted-pipe greps + read-verb MCP tools + `~/.claude/plans` writes pass while
  project edits stay denied; (5) verify CC accepts the `--mcp-config` flag shape (inline JSON, http type).
- **Next**: executors for verbs 2–4 — `open_review` (reuse `OpenRequest::CodeReview`), `query_db`,
  `http_request` — then T10 worktree, T9 economy/HUD.

## RUN CONSOLE + MCP RUN VERBS (phases 1+2) — DONE (2026-06-05) — green; desktop **150** / mcp-server **13** / trust 10 / domain 7; my-files clippy clean

**What:** JetBrains-style **Run tool window** + the run surface exposed to CC over MCP. The toolbar ▶ no longer types into the operator's shell — it launches a **captured child process** (piped stdio via `sh -c`, not a PTY) whose logs land in a dedicated **Run tab** in the bottom dock. The same state is callable by CC sessions through five policy-gated MCP verbs.

**Architecture — one shared state, two worlds:** `apps/desktop/src/run.rs` `RunRegistry` (GPUI-free + tokio-free: std threads + mutex, capped 10k-line ring, `seq` dirty counter, try_wait poller so `stop` can still kill). Created in `main.rs` **before** `ActorService`; handed to BOTH `ShellDeps` (UI) and `RunVerbExecutor` (MCP). An agent-started run renders live in the operator's Run tab and vice versa.

**Pieces:**
- `views/panels/run_console.rs` — `RunConsolePanel`: control row (status lamp+word, command, ↻ Rerun / ⏹ Stop / ⌫ Clear) over scrolling log body (stderr tinted error-red, auto-follow via 100ms seq-poll → `scroll_to_bottom`).
- `workspace.rs` — bottom dock is now a real **multi-tool tab container**: `BottomTool::{Terminal,Run}` chips (Run chip carries a status lamp), `select_bottom_tool`, body switches entity. `run_active_target` → `registry.start` + fronts the Run tab.
- Domain `McpVerb` += `RunStart/RunStop/RunStatus/RunLogs/RunListTargets` + `is_read_only()` (the trust PDP's read-verb list now derives from it — single source of truth). Tiers: reads=ReadOnly·Safe, start/stop=Standard·Risky (frozen phases deny them; tests pin this).
- `crates/mcp-server/transport.rs` — five new tools on `VerbToolServer` via a shared `dispatch` helper; instructions string updated.
- `apps/desktop/src/run_verbs.rs` — `RunVerbExecutor` wrapping the shell executor. **Containment:** `run_start` only launches a *detected* target (`run_config::detect` closed set; arbitrary commands refused with the valid list) — the Standard tier can't be leveraged into a general shell. +5 tests incl. the rm-rf refusal.
- `async-trait` promoted from dev-dep to dep in apps/desktop (the executor implements the domain port in non-test code).

**Play⇄stop swap (follow-up, same day):** while a run is live the toolbar's ▶ is **replaced by a red ⏹** (`run_running` on `ToolbarSnapshot`; `stop_active_target`); flips back when the run exits — kept live by `cx.observe(&run_console)` (the console's 100ms dirty-poll is the chrome's notify source for off-thread registry changes).

**RUN-CONSOLE FEATURES (operator list: restart/stop/scroll/clear/search) — DONE (2026-06-05) — desktop 151, clippy clean.** `run_console.rs` grew the JetBrains console kit: a **left control strip** (↻ Restart — kills + relaunches, available while running · ⏹ Stop on live runs · ⌫ Clear · ⤒ scroll-to-top · ⤓ scroll-to-end · ⌕ search); **follow-mode scrolling** (tail-follow on by default + on run start; an upward wheel disengages it — free scrolling — ⤓ re-engages; the poll loop only auto-scrolls while `follow`); **Cmd+F search** (`actions!(moonlight_run)` + `RunConsole` key context registered in `init_shell`; gpui-component `InputState` search row with live query, `k/n` count, ‹ › + Enter/Shift+Enter navigation, Esc closes; matching lines tinted, current match stronger). Lines are `whitespace_nowrap` at a fixed 16px height (body scrolls both axes) so search-jump offsets are exact (`line × LINE_H`). `RunConsolePanel::new` now takes `window` (eager InputState). ⚠ Runtime-verify: wheel up during a chatty run stops the auto-scroll, ⤓ resumes; Cmd+F after clicking the console; Enter cycles matches; Esc returns focus.

**MULTI-RUN ONGLETS rework (operator: "should be in another onglet bar, not another term bar") — DONE (2026-06-05) — desktop 151 / mcp-server 13, my-files clippy clean.** The Run view is now a **separate tool window with its own tab bar**, JetBrains-faithful: `RunRegistry` holds **one entry per run target** (concurrent runs; rerun **reuses** the target's tab via per-entry `epoch` so stale reader/waiter threads die); `run_console.rs` renders an **onglet bar** — per-run tab = status lamp + label + hover-✕ (kill+close), newly started runs auto-front (`last_started`), controls (↻/⏹/⌫) scope to the active onglet. The bottom dock lost its shared chip header — each tool window owns its bar (terminal = shell tabs, Run = run onglets); switching/toggling moved to **rail stripe buttons** (`toggle_bottom_tool`; new `run_icon` with a corner status lamp visible while the dock is closed). Toolbar ▶/⏹ scopes to the *selected target* (`command_running`). MCP: `run_status` lists all onglets; `run_logs`/`run_stop` gained a `target` param (run's command; empty = latest; wire shape `"<target>\n<tail>"`); unknown target → refused with the current run list. ⚠ Runtime-verify: launch `cargo build` then `cargo test` from the toolbar → two onglets, both live; ✕ closes one; rail Run button toggles the window + shows the lamp; CC `run_start`×2 → two tabs.

⚠️ **Runtime-verify** (RustRover ▶ Run moonlight): (1) toolbar ▶ opens the bottom dock's **Run tab** with live captured output (try `cargo build`); Terminal/Run chips switch; Run chip lamp = blue→green/red. (1b) while running the toolbar ▶ becomes a red ⏹ — click stops; it returns to ▶ on its own when the run exits naturally. (2) ⏹ kills, ↻ relaunches, ⌫ clears. (3) from a CC session: `run_list_targets` → `run_start` → `run_logs` round-trip; the run appears in the IDE's Run tab; `run_start` **denied in Plan phase** and for non-detected commands. (4) audit log shows the verbs.

### Phase 3 — DEBUGGER (next slice, planned)
"Debug mode" = launch the target under a **DAP adapter**; expose debugger features to CC via MCP. Domain already reserves `McpVerb::StartDebug` (Trusted·Risky).
- **Adapter defaults (JetBrains-equivalent), overridable via `.moonlight/config.json` `debug_adapters`:** Go → **Delve** (`dlv dap`; GoLand's engine) · Rust → **lldb-dap** (RustRover bundles LLDB) · C/C++ → **lldb-dap** (CLion macOS default) · Python → **debugpy** (PyCharm uses pydevd; debugpy is the DAP-native equivalent) · Node → **js-debug** (V8 inspector) · PHP → **Xdebug** via the php-debug DAP bridge (JetBrains speaks DBGp natively).
- **DAP client:** Content-Length framed JSON like LSP — pattern off `src/lsp`'s client. Start with launch/breakpoints/continue/step/stack/locals/eval/stop.
- **MCP tools:** `debug_start(target)`, `debug_set_breakpoint(file,line)`, `debug_continue/step_over/step_in/step_out`, `debug_stack`, `debug_locals`, `debug_eval`, `debug_stop` — all through the same ActorService gate.
- **UI:** a Debug tab in the bottom dock (reuse the run console + a "⏸ stopped at file:line" strip; breakpoint gutter in the editor later).

### Later — Explorer-over-MCP (operator-confirmed scope)
The run verbs are the v1 "explorer" surface. A richer slice later: symbol search, go-to-file, outline — the IDE's search/navigation as read verbs. Seam: same `McpVerb` + `VerbToolServer` + executor pattern.

## UI — JetBrains main toolbar (custom titlebar) + space/branch/run widgets — DONE (2026-06-05) — green, clippy clean, desktop **135** tests

**What:** Reclaimed the wasted native macOS title row into a **JetBrains New-UI main toolbar**, hosted in a transparent custom titlebar at the traffic-light level. Layout: `● ● ●  ⌹ project ▾  ⎇ branch ▾  …flex…  ▶ run ▾  ＋Session  ⌗Explorer`. The project-space **tab bar** (Overview + open spaces) drops to row 2; its old ＋Session/＋Space actions moved up (＋Space → the project dropdown's "Open Project…").

**Pieces:**
- `main.rs::window_options` → `gpui_component::TitleBar::title_bar_options()` (appears_transparent + traffic-light inset; 80px left pad handled by `TitleBar`).
- New `views/panels/toolbar.rs` — `main_toolbar(ToolbarSnapshot, cx) -> TitleBar`. **Project selector** (dropdown of open spaces + "Open Project…"), **branch selector** (lists local branches → checkout; hidden when not a git repo), **Run widget** (split `▶ | target ▾`: ▶ sends the active run command to the bottom-dock terminal; ▾ picks a detected target), ＋Session, Explorer toggle. Dropdowns are `deferred(...).with_priority(1)` absolute overlays anchored under their chip (paint over the rows below), styled like the status-bar inbox popover.
- New `views/git_info.rs` — `current_branch` (reads `.git/HEAD`, cheap per-frame), `local_branches`/`checkout` (shell `git`, on interaction). +3 tests.
- New `views/run_config.rs` — `detect(root)` → cargo/npm run targets (`RunKind`/`RunConfig`). +3 tests. (HTTP-request / single-file / user-defined configs deliberately deferred — `RunConfig` is the seam.)
- `project_space.rs` — `toggle_overview()` (overview ⇄ remembered space, falls back to first, no-op when empty) + `last_space` field. +3 tests.
- `workspace.rs` — toolbar state (`space_menu_open`/`branch_menu_open`/`run_menu_open`/`run_target`) + methods (`toggle_overview`, `toggle_*_menu`, `close_toolbar_menus`, `set_run_target`, `run_active_target`, `checkout_branch`); render builds `ToolbarSnapshot` (branch list / run rows filled only while their menu is open) and renders `TitleBar` as row 1, tab bar as row 2.
- `activity_rail.rs` — grid icon now calls `toggle_overview` (overview ⇄ restore-previous), tooltip "(toggle)". `spaces.rs` — trimmed to pure tabs.

⚠️ **Runtime-verify** (RustRover ▶ Run moonlight): (1) the toolbar occupies the titlebar row with the **traffic lights** at its left and no duplicate native title; window drag/zoom still work. (2) **project ▾** lists spaces + "Open Project…"; **branch ▾** lists branches and checking one out runs `git checkout` (error → toast). (3) **▶** runs the selected target in the bottom terminal; **target ▾** switches it. (4) ＋Session launches; ⌗ toggles the explorer (lit when open). (5) the rail grid toggles overview⇄previous. (6) **dropdowns paint above the tab bar/dock** (the `deferred` overlay) — the one thing most worth eyeballing. NB: run configs are cargo/npm-detected shell commands only; `run_target`/menu-open aren't persisted.

## APP RENAME → CC-NATIVE (custom-title append) (2026-06-05) — desktop 119 incl. new test
Operator: "could we rename using CC's session naming, so `/resume` shows titles, not UIDs?" **Verified:** CC
has NO separate name index — the durable name source is the `custom-title` line in the session's transcript
(the record `/rename` itself appends; `sessions/<pid>.json` is just the live process's cache). So:
- **`transcript::append_custom_title(id, name)`** appends that exact record (`{"type":"custom-title",
  "customTitle":…,"sessionId":…}`) — one atomic O_APPEND write, newline-defensive (a mid-write tail is never
  merged/corrupted). +1 test (`append_custom_title_writes_cc_native_record`).
- **`commit_rename` is now 3-way:** live PTY → real `/rename` typed in; **idle session with a transcript →
  native `custom-title` append** (CC's `/resume` picker will show it); ghost (no transcript) → app-local.
  CC-side renames already flow back (custom-title detection, previous slice) — naming is now fully
  bidirectional.
- ⚠️ Build verified green at desktop 119 BEFORE a final cosmetic write_all tweak; the parallel lane's
  in-flight `SpaceTab.attention`/activity_rail edit currently breaks `cargo test` — re-run after it lands.
- ⚠️ **Runtime-verify**: rename an **idle** (no-terminal) session in the app → quit CC picker check:
  `claude` → `/resume` in that repo shows the new name; the transcript's last line is the custom-title record.

## SESSION NAMES + /clear ID-DRIFT (2026-06-05) — green, desktop 118 / engine 27
Operator's two grid/storage pains: (1) tiles often show the raw **session id**; (2) restoring a session can
come up **empty** — "probably `/clear` also changes the session UID?" **Verified TRUE** against real
transcripts: `11863bbd…` ends right after a `/clear` (10:39) and `6c5ec39c…` (a NEW id, same cwd, carrying
the operator's custom title) begins immediately after on the same pid — `/clear` continues the terminal under
a **new session id**, so the old managed record points at the dead pre-clear conversation forever. Also
discovered: the transcript **does** persist `custom-title` (CC `/rename`) and `agent-color` (`/color`) lines
— so CC-side renames ARE readable (earlier "no read-back" finding was wrong).
- **(1) Names — three layers:**
  - **Detection** (`crates/detection`): parse `custom-title` → new `Signal::CustomTitle`; `FileState.custom_titled`
    gives it **precedence** — once the operator `/rename`s, the rolling auto `ai-title` no longer overrides.
    Both flow out as `TitleObserved`.
  - **Persistence** (migration 4 `ALTER TABLE managed_session ADD COLUMN title TEXT`): `ManagedSession.title`
    + `ManagedStateUpdate.title`; supervisor `TitleObserved` now **persists** (UPDATE-only); `to_session()`
    restores it → rehydrated tiles keep their names.
  - **Backfill** (`transcript::latest_title`): bounded 256 KiB tail read — last `custom-title` else last
    `ai-title`; `FleetModel::backfill_titles` fills only `title: None` sessions at grid pull-seed, covering
    records persisted before titles existed + idle observed sessions detection isn't tailing.
- **(2) ↻ Reset is now id-correct:** it no longer sends `/clear`. New engine primitive
  **`Command::ForgetSession`** → supervisor drops the fleet entry + `remove_managed` + publishes new
  **`EngineEvent::SessionRemoved`** (grid drops the tile; transcript on disk untouched). `reset_session`
  (monitor): carries name/color to a fresh id (`SessionMetaCache::put`), removes the old roster entry, sends
  `ForgetSession`, emits `OpenRequest::NewManagedSession{new_id, current phase}` (normal launch path records
  store+roster+tab+terminal), closes the dead tab.
- **Tests** (+5): engine `forget_session_drops_the_fleet_entry_and_announces`; desktop
  `backfill_titles_fills_only_missing_ones` + `session_removed_drops_the_tile`; persistence title round-trip
  in `update_managed_state_is_update_only`; detection `custom-title` parse. Full suite green, clippy clean.
- **Known gap (follow-up):** a **manual** `/clear` typed in the embedded terminal still drifts the id
  (only the Reset button is fixed). Complete fix = reconcile `~/.claude/sessions/<pid>.json` (pid → current
  sessionId) against managed records and rebind. Also `agent-color` lines could sync CC-side `/color`
  changes back into session-meta (read-back now known possible).
- ⚠️ **Runtime-verify**: (1) restart → previously-named sessions show names, not ids (backfill); (2) rename
  in CC (`/rename`) while a session runs → its tile/tab pick the custom name and keep it (no ai-title
  override); (3) ↻ Reset → old tile disappears, a fresh tab opens with the same name/color under a new id,
  and restoring it later is NOT empty; (4) old session's transcript still on disk.

## SESSION COLOR → CC `/color` + IDE display (2026-06-05) — green, desktop 116
Operator: rename writes through to CC but color didn't; also surface the color in the IDE. CC **does** have a
native `/color` (accepts `red, blue, green, yellow, purple, orange, pink, cyan, default`) — earlier research
was stale.
- **Palette realigned to CC's exact set** (`session_meta.rs`): `SessionColor` = Red/Orange/Yellow/Green/Cyan/
  Blue/Purple/Pink (was Slate/Teal/Amber/…). New `token()` (= CC `/color` token, lowercase) + `from_token()`
  (case-insensitive). **Color is now persisted as the token STRING** (`SessionMeta.color: Option<String>`,
  read via `palette_color()`), so it round-trips to CC *and* migration is safe: old values that still match
  (Blue/Green/Red/Purple/Orange/Pink) keep working, retired ones (Slate/Teal/Amber/"default") degrade to "no
  color" **without failing the record** (names preserved — a hard enum would have dropped the whole entry).
- **Write-through** (`session_monitor::pick_color`): for a managed session (we own the PTY) sends
  `/color <token>` to the embedded terminal — clear → `/color default` — mirroring the `/rename` path.
- **IDE display of the color** (all three the operator asked for): **tab title** — `panels::tab_title` gained
  a `label_color: Option<Hsla>` arg (4 other callers pass `None`; the session passes its color); **session
  view background** + **header accent** — new `theme::blend(base, accent, amount)` washes the monitor panel
  (0.10) and gives the header a tinted band + border (0.22). Fleet tiles already show the color dot.
- **Tests**: +`token_round_trips_and_retired_colors_degrade`; existing color tests updated to the new palette
  (`palette_color()` + token assertions). Full suite green; clippy clean on touched files.
- ⚠️ **Runtime-verify**: pick a color on a managed session → the embedded CC terminal runs `/color <name>`
  and CC adopts it; the tab label, panel background, and header band tint to match; clear → `/color default`
  and the tints drop. Observed (no-PTY) sessions tint locally only. (Note: parallel lane is mid-refactor on
  `attach_command`/obs `chain_statusline` — coordinate.)

## FLEET REHYDRATION — REAL FIX: grid pull-seeds from the store (2026-06-05) — green, desktop 112 / engine 26 / domain 7
The boot-rehydration below shipped but the operator still saw only ~2 sessions where 13 exist in the active
space. **Root cause:** the supervisor's `hydrate_from_store()` publishes the fleet as a **one-shot
`SessionUpserted` broadcast at engine-loop start**, but the `EventBus` (tokio `broadcast`) only delivers to
receivers that subscribed **before** the send — and the dock builds `GridHome` (where `bus.subscribe()` runs)
**lazily on the first frame**, *after* `open_window` returns and the loop has already fired hydrate. So the
grid missed all 16 upserts; the 2 visible were the live-detected ones that keep arriving on later poll ticks.
(Same reason the ⟳ Refresh button did nothing: `hydrate_from_store` skips sessions already in the supervisor
fleet, so post-boot it re-publishes nothing.)
- **Fix = the grid PULLS its fleet from the store, not (only) the bus.** New shared reconstruction
  **`ManagedSession::to_session()`** (`crates/domain/src/ports/store.rs`) — single source of truth so the
  supervisor (`hydrate_from_store` now calls it) and the UI can't drift (Idle at rest, Observed trust,
  operator fields from the record). `FleetModel::seed_missing(&[ManagedSession])` (`grid_home.rs`) adds only
  records the model lacks (**merge-only** — never downgrades a live `Running` session back to the record's
  `Idle`). `GridHome::new` gained a `store: Option<Arc<dyn ManagedSessionStore>>` arg and **pull-seeds at
  construction** (both call sites in `workspace.rs` pass `deps.store`); ⟳ Refresh now **re-pulls locally**
  (immediate) *and* still sends `Command::RehydrateFleet` (re-sync supervisor/gate for out-of-band sessions).
- The supervisor's bus hydrate is **kept** (it populates the supervisor fleet + feeds the gate-fold, which
  *does* subscribe early at `main.rs:132`). Tests: +1 desktop (`seed_missing_pull_seeds_without_clobbering_live_sessions`),
  domain `to_session` covered via engine hydrate tests. Full suite green.
- ⚠️ **Runtime-verify**: launch with a populated `moonlight.db` → the active space's grid shows **all** its
  managed sessions immediately (not just the live ones); ⟳ Refresh re-syncs; a currently-running session is
  not reset to Idle by the pull.

## SESSION RENAME + COLOR (catalog scope, slices 1–2) — DONE (2026-06-04) — green, desktop 109 (+4)
Operator's "small feature": rename / pick a color for a session in the **focus view**. First two slices of
the catalog scope (`session-catalog-and-rename-scope.md`); slices 3–5 (the external-session catalog) remain.
- **Slice 1 — per-space metadata store** `apps/desktop/src/views/session_meta.rs` (gpui-free core, +4 tests):
  `SessionColor` (9-color fixed palette enum → `hsla()`/`label()`/`ALL`), `NameSource{Local,Cc}`,
  `SessionMeta{name,color,name_source}` persisted at `<root>/.moonlight/session-meta.json` (separate from the
  managed roster so it covers **observed** ids too; self-ignored via `.gitignore *`). `load/get/upsert` +
  `set_name`/`set_color` (merge-preserving). Registered `pub mod session_meta;`.
- **Slice 2 — rename + color UI** in `session_monitor.rs` (the focus view): header now shows a **color dot**
  (toggles a swatch palette with a "clear" option + the selected color's name) and the name with a **✎ pencil**
  → inline `InputState` rename (Save/Cancel). `commit_rename` persists the name and, for a **managed** session
  (we own the PTY), **writes through to CC's native `/rename`** via the embedded terminal (`name_source=Cc`);
  observed sessions are local-only. Display everywhere prefers `meta.name` → `aiTitle`/`s.label()` → id, so the
  **tab title** updates live (`title_text`). Metadata root resolved from `resume_root` else `attached_path`
  (`expand_home`); controls hidden when no root. Cached `meta` reloaded after each edit (+ status-bar re-announce).
- **Decisions honored:** per-space storage; rename applies to **all** sessions (managed + observed); name uses
  **CC-native rename** write-through (read-back is unreliable → we mirror locally); color is ours (CC has none).
- ⚠️ **Runtime-verify**: (1) open a session → ✎ → type a name → Save: header + tab update; for a managed
  session the embedded CC terminal shows `/rename <name>` taking effect; (2) click the color dot → pick a
  swatch → the dot fills, persists across reopen (`.moonlight/session-meta.json`); "clear" removes it; (3) an
  observed (no-PTY) session renames locally (no terminal write); (4) a session with no resolvable root hides
  the rename/color controls.

## SESSION NAME/COLOR ON FLEET TILES (slice-2 follow-up) — DONE (2026-06-04) — green, desktop 110 (+1)
The custom name + color now show on the **grid tiles**, live, via a cached read-model (no per-render disk IO).
- **`SessionMetaCache`** (`session_meta.rs`): an observable in-memory cache (`loaded` roots + `by_id` map).
  `ensure_space(root)` loads a space's file once; `get(id)` is a pure lookup; `set_name`/`set_color` persist
  **and** update the map. Held as a shared **`Entity<SessionMetaCache>`** in `ShellDeps` (created in
  `init_shell` like `SessionIo` — **no main.rs plumbing**). +1 test (`cache_loads_once_and_reflects_writes`).
- **Grid** (`grid_home.rs`): `GridHome` caches the entity + `cx.observe`s it (a rename/recolor → re-render).
  Render resolves each visible session's meta from the cache (`ensure_space` per session's `attached_path`,
  works for overview *and* scoped spaces), passing it to `session_tile`.
- **Tile** (`session_tile.rs`): `session_tile(s, meta)` now prefers `meta.display_name()` over the CC title
  and draws a small **color dot** beside the title when set (kept distinct from the status traffic-light rail).
- **Focus view** (`session_monitor.rs`): `commit_rename`/`pick_color` now write **through the cache entity**
  (`cache.update(.. cx.notify())`) so the grid updates live; they fall back to a direct disk write only when
  no shell global exists (static/test views) — matching the existing `try_global` pattern.
- ⚠️ **Runtime-verify**: rename or recolor a session in its focus tab → its **grid tile updates immediately**
  (name + color dot), in both Overview and a scoped space; persists across restart; observed sessions too.

## FLEET BOOT-REHYDRATION (lost-sessions regression) — DONE (2026-06-04) — green, engine 26 / desktop 105
Operator: *"I lost app sessions — the full list of managed/CC sessions is no longer displayed, though I can
still `claude --resume` them."* **Root cause (a regression from the spaces top-tab redesign):** the cockpit
fleet grid is fed **only by live `EngineEvent` deltas** — `SessionSupervisor` boots with `fleet:
HashMap::new()`, `GridHome::new` "starts empty and mirrors the bus", and `FleetModel::apply_event` only
*adds* a tile on `SessionUpserted` (thin events are ignored for unknown sessions). The store method that
could refill it (`all_managed()`) had **zero non-test callers**, so nothing replayed the persisted fleet on
startup. The old left **Spaces rail** used to read `.moonlight/sessions.json` and show idle sessions; the
"SPACES = TOP-TAB SWITCHER" redesign removed that rail and assumed the scoped fleet grid would show them —
but the grid never loads persisted sessions. So after the redesign + any restart, idle/persisted managed
sessions vanished from the UI. **No data was lost** — `moonlight.db` (12 managed), `.moonlight/sessions.json`
rosters, and the CC transcripts were all intact.
- **`SessionSupervisor::hydrate_from_store()`** (`crates/engine/supervisor.rs`): reads `all_managed()`,
  reconstructs an **idle** `Session` per record (phase/mode/adopted/paused/phase_pinned/root/last_seen from
  the row; `trust_tier = Observed` since trust isn't persisted; detection promotes Idle→Running if live),
  inserts into `fleet`, and publishes a `SessionUpserted` so every subscriber (grid, gate view, policy view)
  shows it. **Idempotent** — skips a session already in the live fleet (never clobbers fresher state).
- **Boot wiring** (`apps/desktop/main.rs`): `run_engine_loop` calls `supervisor.hydrate_from_store()` before
  the first poll. Ordering is safe: the window — and thus `GridHome`'s `bus.subscribe()` — is built (line
  189) *before* `run_engine_loop` (line 198), so the broadcast upserts are received (12 ≪ 256 buffer).
- **Manual refresh** (operator asked): new **`Command::RehydrateFleet`** (`crates/engine/lib.rs`) →
  `handle_command` → `hydrate_from_store()` (falls through `route_approval`'s wildcard, no dispatch change).
  UI: a subtle **"⟳ Refresh"** secondary button in the `GridHome` header action row (next to ＋ New session)
  sends it via `self.commands`. Reloads the fleet from the store on demand.
- **Tests** (+2 engine, 24→26): `hydrate_from_store_seeds_the_fleet_and_publishes_upserts` (two records →
  fleet + two upserts, fields faithful, status Idle), `hydrate_from_store_does_not_clobber_a_live_session`
  (a Running session survives a stale Idle record; no upsert for the already-live id). `FakeStore` gained a
  `managed_all` field feeding `all_managed()`.
- **Still open (the "or in CC" half):** only **app-managed** sessions rehydrate. Surfacing **observed
  external** CC sessions (scanning `~/.claude/projects/*.jsonl`) is a separate, larger feature — **now SCOPED
  in `session-catalog-and-rename-scope.md`** (center-tab catalog, 30-day window, per-space rename/color via
  CC-native rename write-through + local color). Includes the operator's new rename/color ask. Not yet built.
- ⚠️ **Runtime-verify**: (1) launch a couple managed sessions, quit the app, relaunch → they reappear as idle
  tiles in the fleet/Overview (scoped to their space); (2) click ⟳ Refresh → no duplicates and no change to
  live sessions; (3) a currently-running session isn't reset to idle by a refresh.

## SLICE 3 START — MCP-ACTOR CORE (T8) — DONE-increment (2026-06-04) — engine lane, green, +7 mcp-server tests
Operator: "Start slice 3." First increment of the bidirectional-MCP innovation, kept entirely in `crates/`
(no UI-lane collision; the parallel lane is HOT on `apps/desktop`). Proves the **mandatory verb shape**
(architecture): *resolve → PDP → execute → audit → compact result* for one thin verb (`run_with_coverage`).
- **Domain ports** (`crates/domain/src/ports/mcp.rs`): three new collaborator ports next to `McpActor`/
  `PolicyDecisionPoint` — `SessionPolicySnapshot{phase,trust_tier,root}` + `SessionPolicyView` (resolve the
  session's live policy), `VerbExecutor` (async; runs an approved verb, returns raw output), `AuditSink`
  (infallible best-effort `record(session, AuditAction, revertible)`, mirroring the supervisor's audit).
- **`crates/mcp-server`** (was a doc-only stub): **`ActorService`** impl of `McpActor` — resolves the
  snapshot (else `SessionNotFound`), builds a `PermissionRequest` (verb danger via `danger_class`: read verbs
  Safe, `HttpRequest`/`StartDebug` Risky so frozen phases gate them), asks the **single `DefaultPdp`**, then:
  Allow → execute → `compact_output` (RTK-style: keep error/fail/`test result` lines, else tail, cap 40) →
  audit `VerbExecuted` → `ActorResult{ok}`; Deny → audit `Denied`, `ok:false`; **Prompt → soft-deny**
  ("needs operator approval") since MCP verbs have **no held-approval channel yet** (the keystone is
  hook-bound — a dedicated MCP approval hold is the next decision). Plus **`ShellVerbExecutor`** (real:
  shells a configured `test_command` in the session root via `tokio::process`, combines stdout+stderr;
  non-`RunWithCoverage` verbs → `Unsupported`). Cargo.toml fleshed out (async-trait, tokio, tracing;
  dev-dep `moonlight-trust` so tests gate against the **real** PDP). 7 tests (allow/run/compact/audit;
  low-tier refused-not-run; frozen-phase deny; unknown-session err; executor-failure; compaction; real
  shell-exec + unsupported-verb).
- **Note on T1:** the PDP **already** gates MCP verbs by trust tier (`McpVerb::min_autonomous_tier` +
  `DefaultPdp` step 3) — so verb-gating is done. T1's remaining gap is *tool-use* danger-by-tier (Bash/Edit
  via the hook pass `verb:None`), independent of this.
### Slice 3 increments 2 + 3 — DONE (2026-06-04) — engine lane, green
- **Concrete adapters** (`crates/mcp-server/src/adapters.rs`): **`BusPolicyView`** — a `SessionPolicyView`
  folded from the engine bus (`SessionUpserted` → {phase, trust_tier, root}; `PhaseTransitioned` updates
  phase). Trust isn't persisted, so the snapshot is read from *live* facts (operator's current intent), not
  the store. **`StoreAuditSink`** — an `AuditSink` over the durable `ManagedSessionStore` (`append_audit`,
  `<millis>-<seq>` ids like the supervisor; best-effort). mcp-server now deps `moonlight-engine`; dev-deps
  `moonlight-persistence` (audit test writes a real in-memory DB). +2 tests.
- **MCP approval hold** (increment 3): new domain port **`ApprovalGate`** + `ApprovalDecision`
  (`ports/mcp.rs`). `ActorService` gained an `approval` collaborator: a PDP **`Prompt`** now calls
  `approval.request(...)` → **Approve** runs it (audits `Approved` then `VerbExecuted`), **Deny**/timeout
  refuses (audits `Denied`). **`DenyingApprovalGate`** (mcp-server) is the default-deny used where no channel
  is wired. **`KeystoneApprovalGate`** (`crates/control`) bridges to the **same** `PendingApprovals` registry
  the plan/danger holds use — registers, notifies via `ApprovalNotifier`, awaits the operator's
  `route_approval` decision (timeout → deny). +2 control tests, +2 mcp-server tests (approve runs / deny refuses).

### Slice 3 (1) rmcp transport — BINDING DONE, wiring forked (2026-06-04)
- **`rmcp = "=1.7.0"`** added to `crates/mcp-server` (features `server, macros, transport-io`; pinned).
  rmcp re-exports `schemars` 1.0 + `serde`, but I added `schemars="1"` + `serde derive` directly to match
  the `#[tool]` macro (as rmcp's own tests do). **`transport.rs`**: `VerbToolServer` — an rmcp tool server
  (`#[tool_router]`/`#[tool]`/`#[tool_handler]`) exposing **`run_with_coverage`**, delegating to the
  `Arc<dyn McpActor>`; `serve_stdio` runs it. **Compiles against the real rmcp API** (validates usage) — 11
  mcp-server tests green, clippy clean. (Gotcha: `ServerInfo` is `#[non_exhaustive]` → build via `default()`
  + field set, not a struct literal.)
- **Operator chose embedded in-app HTTP (A) → BUILT (2026-06-04, compile-validated against rmcp 1.7 + axum 0.8).**
  Model: **one tiny MCP server per managed session** on an ephemeral loopback port (no dynamic routing; the CC
  session is carried by *which* port it talks to → everything stays in-process so the live adapters apply).
  - `crates/mcp-server` deps: rmcp `+transport-streamable-http-server`, `axum 0.8` (`default-features=false,
    ["http1","tokio"]`). **`transport.rs`**: `serve_http(actor, session, listener)` mounts `StreamableHttpService`
    (factory→`VerbToolServer`, `LocalSessionManager`, default config) at `/mcp` via `axum::Router::nest_service`
    + `axum::serve`. **`McpHost`** = shared session-agnostic `Arc<dyn McpActor>` + the live `BusPolicyView`;
    `McpHost::spawn_for(session) -> "http://127.0.0.1:<port>/mcp"`. 11 tests green, clippy clean.
  - **REMAINING = composition wiring only (HOT lane + live-CC; left for a coordinated/test-running session).**
    The parallel UI lane is actively rewriting `main.rs` + `workspace.rs` (`ShellDeps`), so these were NOT
    applied blind. Recipe:
    1. **main.rs:** build once — share `Arc::new(BusNotifier{bus})` with `spawn_control_server`;
       `let policy = Arc::new(BusPolicyView::new());`
       `let actor: Arc<dyn McpActor> = Arc::new(ActorService::new(Arc::new(DefaultPdp), policy.clone(),
       Arc::new(ShellVerbExecutor::new(vec!["cargo".into(),"test".into()])), Arc::new(StoreAuditSink::new(store.clone())),
       Arc::new(KeystoneApprovalGate::new(pending.clone(), notifier, Duration::from_secs(45)))));`
       `let mcp_host = McpHost::new(actor, policy.clone());` + a **bus-fold task** (mirror the gate_view fold:
       `rx.recv → policy.apply(&event)`) so the policy view tracks live phase/trust/root.
    2. **Cross-runtime:** `spawn_for` binds+spawns → needs a **tokio** runtime, but the launch arm is on GPUI.
       Give `McpHost` a **multi-thread** `tokio::runtime::Handle` (not the control server's current_thread) + a
       sync `url_for(session)` that `handle.block_on`s the bind+spawn. Put `McpHost` in `ShellDeps`.
    3. **workspace.rs launch arm:** before `claude --session-id <id> …`, call `deps.mcp_host.url_for(id)` and
       append `--mcp-config '{"mcpServers":{"moonlight":{"type":"http","url":"<url>"}}}'` (verify CC's exact
       flag/shape live).
    4. ⚠️ **Runtime-verify (live CC):** a managed session sees the `moonlight` MCP server + `run_with_coverage`;
       calling it runs tests + returns the compact summary + lands a `VerbExecuted` audit; an over-tier verb
       raises a cockpit approval (KeystoneApprovalGate) before running.
- **(4) more executors** — `open_review` (submit a diff to the review queue → reuse `OpenRequest::CodeReview`),
  `query_db`, `http_request` (each its own adapter/dep). Engine-lane + testable; do after the transport
  proves the loop.
- After that: **T10** worktree isolation (`crates/worktree`), **T9** economy/governor/HUD.
- ⚠️ **Runtime-verify** (deferred to the test-running session): none yet runs against live CC — this
  increment is logic-only (unit-tested against the real PDP). Live verification waits on the rmcp transport.

## SIGNATURE-PRIMITIVE FOLLOW-ONS — Option C + T4 + per-hunk staging — DONE (2026-06-04) — green, 192 tests
Operator: "need all before commit." Completed the three follow-ons left open by the per-hunk
rejection-as-feedback section below. All build+test+clippy green (clippy: only 2 pre-existing parallel-lane
warnings in `classify.rs`/`session_monitor.rs`, none in these files).
- **Option C — real inject channel (replaces Option A's UI terminal-write).** Delivery now lives *behind
  the `ControlPort`*: new **`SteerControl`** adapter (`crates/control/src/lib.rs`, `ControlLevel::Steer`)
  whose `inject_feedback` queues `Feedback` on an mpsc channel; everything else is `Unavailable` (CC owns
  sessions, PDP/hook enforce). `main.rs` builds it (`steer_tx`) into the supervisor (replacing
  `ObserveOnlyControl`) and spawns **`run_steer_drain`** (GPUI executor) which drains `steer_rx` and writes
  each message into the target session's embedded terminal via `ShellDeps::session_io` (no live terminal →
  dropped with a warn). Supervisor `RejectHunk` reverted to `inject()` (now really delivers + audits);
  `record_injected_feedback` (the Option-A stopgap) removed. `code_review.rs` no longer touches `SessionIo`
  — Reject just emits `Command::RejectHunk`. +2 control tests (queues / fails-when-drainer-gone), engine
  test renamed `reject_hunk_injects_via_the_port_and_audits`.
- **T4 — summary capture.** New `Signal::Summary` (`detection/jsonl.rs`, `extract_assistant_text` = the
  assistant message's `text` blocks, capped 2000, **skipped on plan turns**) → `DetectionEvent::SummaryObserved`
  (`detection/lib.rs`, deduped per session) → `EngineEvent::SummaryObserved` (supervisor pass-through for
  tracked sessions). UI: the workspace bus-bridge caches `summaries` per session and passes the latest into
  `OpenRequest::CodeReview { …, summary }`; `code_review.rs` renders a **"What this covers"** band above the
  diff (persisted in `dump()`). Added the ignore-arm in `grid_home::apply_event`. +1 parser test.
- **Per-hunk selective staging (FR23).** `git/diff.rs`: `split_file_diff -> (header, bodies)` (parse_hunks
  now shares it) + **`restage_file(root, rel, header, accepted_bodies, untracked)`** — idempotent: `git reset
  -q -- <file>` to HEAD, then `git apply --cached --recount` the reassembled (header + accepted `@@` blocks)
  patch (`--recount` makes a hunk-subset apply cleanly, no line-number drift); untracked = whole-file add/unstage.
  `code_review.rs` keeps `diff_header`, and **Accept/Reject restage the file** (`restage_selected`) so the
  index always equals the accepted set; accepted chip shows "✓ accepted · staged"; staging errors surface in
  the panel. +2 tests (pure split; real temp-repo `restage_file_stages_only_accepted_hunks`).
- **Updated memory:** [[moonlightcode-feedback-delivery]] now reflects Option C shipped.
- **Still open (future):** the commit action itself (Commit phase has no UI commit yet — staging lands in the
  index, the operator commits manually for now); delivery for terminal-less/observed sessions (SDK-resume);
  rename/copy hunk staging edge cases.
- ⚠️ **Runtime-verify**: (1) Reject a hunk on a managed session → the feedback line appears in its embedded
  terminal (via the SteerControl→drain path, no longer a direct panel write); (2) the review tab shows the
  "What this covers" band from the agent's last prose; (3) Accept a hunk → `git diff --cached` in the repo
  shows *only* that hunk staged; rejecting/un-accepting unstages it; (4) a 2nd accept adds to the staged set
  without disturbing the first (recount); (5) terminal-less session: reject logs a warn, no crash.

## UI — rail tool icons + full-width workspace-owned bottom dock — DONE (2026-06-05) — green, clippy clean, 119 desktop tests
Operator: *"icons should represent **tools**, not the bar (bottom = terminal + later git/services; top =
files, commit history, structure); and the bottom dock should take full width (left dock above it), not be
squeezed by the left dock."*
- **Rail = tool icons** (`panels/activity_rail.rs`): replaced the panel-position rectangles with
  **div-composed tool glyphs** — `folder_icon` (Project files), `terminal_icon` (a window + `❯`), and the
  `grid_icon` (Sessions overview). Grouped **project tools top** / **runtime tools bottom** (flex gap
  between). `tool_color(lit)` = accent when open, muted idle. Future tools (commit history, git, services,
  structure-as-its-own-toggle) slot in once the docks become multi-tool containers.
- **Bottom dock is now full-width + workspace-owned** (`workspace.rs`): gpui-component's `DockArea`
  hardcodes side-docks-outer / bottom-inner (its render nests the bottom dock inside the center column,
  so it can't span under the left dock). So the terminal is **lifted out of the `DockArea`** into a
  workspace-composed strip: the render is now `rail | v_flex[ dock(left+center, flex_1) , bottom_dock
  (full width, when open) ]`. New `Workspace` fields: `terminal: Entity<TerminalPanel>` (created in
  `new`), `bottom_open`, `bottom_height` (px), `resize_anchor`. `bottom_dock()` renders a **top resize
  grip** (drag → window-level `on_mouse_move` adjusts height, clamp 120–640; `on_mouse_up` ends), a
  **tool-tab header** (Terminal now), and the terminal body. `toggle_bottom_dock` flips `bottom_open`;
  `RailSnapshot.bottom_open` reads the field. `reset_default_layout` no longer sets a bottom dock;
  `set_dock_collapsible` bottom=false. **`DOCK_VERSION` 5→6** (persisted v5 layouts rebuilt).
- ⚠️ **Runtime-verify** (RustRover ▶ Run moonlight): (1) the bottom dock spans the **full width** under
  the left rail and the left rail now ends above it; (2) drag the top grip to resize (cursor =
  up/down-resize); (3) the rail's `❯ Terminal` close (✕) + the rail terminal toggle both hide/show it;
  (4) the workspace-owned terminal still spawns/echoes correctly outside the DockArea (focus + PTY);
  (5) v5→v6 layout rebuild leaves no stray empty bottom dock. NB: bottom open/height aren't persisted yet
  (reset to open/240px on restart) — follow-up.

## UI REFINEMENT — activity rail + file tree + structure — DONE (2026-06-05) — green, clippy clean, 119 desktop tests
Operator: *"toggle should be referenced at the level it displays (term → bottom) + clearer icons; refine
central view + structure + file tree."* UI lane.
- **Activity rail** (`panels/activity_rail.rs`): the **Terminal toggle now sits at the bottom** of the rail
  (a `flex_1` spacer drops it there) — each toggle lives at the edge its dock opens on. Icons are now
  **position-aware, div-composed glyphs** (no ambiguous unicode): `panel_icon(Left/Bottom)` = a bordered
  "window" with the matching edge filled (left strip = Explorer dock, bottom strip = Terminal dock);
  `grid_icon` = a 2×2 cell grid for the Fleet. Lit = accent, idle = muted; kept the phosphor-tick lit state.
  `rail_btn` now takes an `AnyElement` icon instead of a glyph string.
- **File tree** (`panels/file_tree.rs`): JetBrains-style **indent guides** (faint vertical lines under each
  ancestor's disclosure column, absolute-positioned full-height) + a **crisp accent bar** on the selected
  row (echoes the rail's lit tick), on top of the existing wash.
- **Structure** (`panels/structure.rs`): symbol rows now carry a **color-coded kind chip** (`kind_color`
  via the ANSI palette — callables blue, data-types green, enums magenta, contracts yellow, containers
  cyan) instead of uniform grey labels; same **indent guides** as the tree; and a centered **empty state**
  ("≣ / No symbols / Open a code file…").
- ⚠️ **Runtime-verify** (RustRover ▶ Run moonlight): the Terminal toggle is at the bottom-left; the
  Explorer/Terminal icons read as left/bottom panels and light up when open; tree + outline show indent
  guides; selected file has the accent bar; outline kind-chips are colored.

## STATUS BAR — quota self-fetcher + restore OMC HUD — CODE-COMPLETE (2026-06-05) — ⚠ build red from a concurrent refactor, see below
Operator: *"self fetcher for UI and restor OMC HUD pls."* Both implemented; **my files compile clean** —
the tree is currently red only from the parallel session's `SessionColor`/`tab_title` refactor
(`session_tile.rs`, `session_monitor.rs`, `session_meta.rs`, `plan_review.rs`), so full build/test is
**pending their fix** (no errors attributed to any file I touched).
- **Self-fetcher** (`obs.rs`): quotas now fetched **directly from Anthropic**, no OMC needed.
  `fetch_quota()` → `GET https://api.anthropic.com/api/oauth/usage` with `Authorization: Bearer <token>`
  + `anthropic-beta: oauth-2025-04-20`; token read from `~/.claude/.credentials.json`
  (`claudeAiOauth.accessToken`). Done via **`curl --config -`** (options on stdin) so the token never
  appears in argv/`ps` — **no new crate dep**. Response `{five_hour,seven_day,seven_day_sonnet:{utilization}}`
  → `parse_usage_api` (utilization is 0–100, rounded). Public **`quota()` layers**: self-fetch → OMC
  cache (`load_quota`, now private) → `None`. Poller (`workspace.rs`) fetches quota on the **first tick
  and every ~2 min** (`tick % 100`), not every 1.2s (it's network-bound). +1 test (`parse_usage_api`).
- **Restore OMC HUD = chain-preserve** (the [[moonlightcode-preserve-omc-hud]] intent, *not* removing
  `--settings`): `obs::chain_statusline(input)` runs the operator's **global** `~/.claude/settings.json`
  `statusLine.command` (the OMC HUD) with the same stdin and echoes its stdout; `main::run_statusline`
  now writes obs **and** prints the chained HUD (falls back to our compact line only if there's no global
  statusline). Recursion-guarded (skips a `moonlight … statusline` command). So embedded terminals show
  the OMC HUD again while we still capture per-session obs — no `--settings` removal, no obs regression.
- ⚠️ **Verify once the tree is green**: (1) the bar's `5h/wk/sn` pills fill from the **direct fetch** even
  with OMC's daemon stopped (delete/rename the OMC cache to prove no-OMC path); (2) an embedded session's
  in-terminal status line shows the **OMC HUD** again (not our compact line); (3) `cargo test` incl. the
  new `parse_usage_api`. Note: `fetch_quota` shells `curl` (always on macOS); on first run the OAuth token
  must be present + valid.

## STATUS BAR — real obs stats: ctx% + 5h/wk/sn quota — DONE (2026-06-05) — green, 247 tests
Operator: *"stats not the ones I want — ctx gives raw context not a % usage, and I don't see the
hourly/weekly/sn stats."* UI lane, additive.
- **ctx as a %** (`obs.rs` + bar): `SessionObs.ctx_limit` (the model's window — 1M for `1m` variants,
  else 200k via `context_limit(id,name)`); the bar now renders ctx as an OMC-style bar+% pill
  (`ctx[####------]N%` = `ctx_tokens / ctx_limit`) instead of the raw `84k`.
- **5h / weekly / Sonnet-weekly quota** (`obs.rs::load_quota` + `obs_store` + poller + bar): read from
  **oh-my-claudecode's usage cache** `<config>/plugins/oh-my-claudecode/.usage-cache-anthropic.json`
  (`{data:{fiveHourPercent, weeklyPercent, sonnetWeeklyPercent}, error}`), the exact figures the OMC HUD
  shows. The obs poller (`workspace.rs`, 1.2s) now also `load_quota()` → `ObsStore.set_quota`; the bar
  shows `5h/wk/sn` pills **account-wide (always, independent of the selected session)**. `parse_quota` +
  `context_limit` unit-tested (+2). `error:true` cache → no data (don't show stale as fresh).
- **Bar refactor**: split `StatusSnapshot.quota: QuotaView` (global) from `obs: Option<ObsView>`
  (per-session model/persona/time/ctx%); `obs_zone(quota, obs)` renders the 3 quota pills always, then
  the per-session cluster (or `—`). Reuses `quota_pill`/`ascii_bar`/`quota_color` (color by utilization).
- **Source note / follow-up**: quotas come from OMC's cache (operator runs OMC) — exact + zero new
  deps/credentials. The operator earlier chose **"MoonlightCode fetches usage itself"** (no-OMC fallback):
  still a separate slice — needs the Anthropic usage endpoint pinned (NOT in OMC's
  `rate-limit-wait/rate-limit-monitor.js`) + the OAuth token read + an HTTP client; `load_quota` is the
  layering point. Also still pending: transcript-direct obs sourcing + dropping the `--settings`
  statusline override to preserve the OMC HUD in embedded terminals ([[moonlightcode-preserve-omc-hud]]).
- ⚠️ **Runtime-verify**: the bar shows `5h[bar]N% wk[bar]N% sn[bar]N%` (matching the OMC HUD's numbers),
  and `ctx[bar]N%` for the selected session (was raw `84k`). With OMC running the quota pills fill in
  within ~1.2s; without OMC they show `—`.

## STATUS BAR — toasts styled + bottom-right — DONE (2026-06-05) — green, 245 tests
Operator: *"Notif could be styled and should popup at bottom right of screen (like it came from the
notification part)."* Two small edits — no custom overlay; gpui-component's toast layer is
theme-configurable.
- **Placement** (`theme.rs::install`): `theme.notification.placement = gpui::Anchor::BottomRight` +
  `margins.bottom = px(40.)` — toasts now rise from the bottom-right, clearing the 30px status bar so
  they read as coming from the 🔔 corner. (Built-in slide-up animation + 5s autohide + stacking kept.)
- **Styling** (`workspace.rs` toast loop): replaced the plain `Notification::info/warning/error` with
  `Notification::new().autohide(true).content(…)` rendering the **kind glyph in its color** (same
  `status_bar::kind_style` language as the bell popover) + the message in app typography, inside
  gpui-component's themed popover frame. Added `use super::theme;` to workspace.
- ⚠️ **Runtime-verify**: trigger a notification → a styled toast (colored glyph + text) slides up at the
  **bottom-right** above the status bar, auto-hides ~5s, stacks upward; no longer appears top-right.

## PLAN-REVIEW PROPAGATION — wait-for-menu instead of fixed delay — DONE (2026-06-04) — green, 244 tests
Operator: *"Plan in app review is still not propagated correctly to CC."* Root cause: after Approve, the
app drove CC's post-`ExitPlanMode` continuation menu (1 accept+auto · 2 step · 3 refine · 4 reject) by
injecting the digit at a **fixed 900 ms** — but the round-trip (cockpit → engine → `pending.resolve` →
hook returns over the socket → CC re-renders the menu) is variable and often outlasts 900 ms, so the
keystroke landed before the menu existed and was lost. Delivery itself was fine (`send_text`→`write_str`→
`Msg::Input`→PTY stdin, verified).
- **Fix** (`plan_review.rs::select_native_option`): **poll the embedded terminal until CC's menu is
  actually on screen, then inject** the digit (`1`/`3`), with a 6 s fallback + a `tracing::info!(option,
  waited_ms, matched, …)` line. Robust both when the held hook is still resolving *and* when CC already
  showed the menu on its own (e.g. hook not held) — it injects whenever the menu is visible.
- **New screen-read helpers**: `Emulator::visible_text()` (`term/emulator.rs`, mirrors the renderer's
  `renderable_content` walk → plain text) + `TerminalPanel::visible_text()` (`panels/terminal.rs`).
  Menu detection `is_continuation_menu(screen)` matches CC-menu-specific phrases ("auto-accept" /
  "keep planning") so it never false-matches numbered plan markdown. +1 test.
- Also: a `tracing::warn!` when no embedded terminal is registered for the session (the *other* failure
  mode — the session's monitor tab isn't open, so there's nothing to drive).
- ⚠️ **NEEDS LIVE-CC VERIFY** (couldn't test against a live session here): Approve a held plan → CC leaves
  plan mode → the app injects `1\r` once the menu renders → CC continues in auto. Check the log line: if
  `matched=false` (fell back to the 6 s timeout) CC's wording differs from the markers — **tune
  `is_continuation_menu`**. Refine = option `3`. If the warn about "no embedded terminal" fires, the
  session monitor tab must be open/registered for the digit to land.

## STATUS BAR — bar polish + toasts + OMC-stat layout — DONE (2026-06-04) — green, 242 tests
Operator returns: bigger bar · notifications should pop up (toast) · stats should match the default OMC
HUD (no financial figures). UI lane, additive.
- **Bigger bar** (`status_bar.rs`): `BAR_HEIGHT` 24→30, `text_xs`→`text_sm`. One line (operator picked
  "one line, fewer stats").
- **Toasts** (`workspace.rs`): each *new* notification now pops as a transient, auto-hiding toast
  (gpui-component `WindowExt::push_notification`, ~5s autohide). Wired via `cx.observe_in(notifications,
  window, …)` (gives the `Window` push needs); `last_toast_id` gates "only the genuinely new" so changes
  like mark-read/clear don't re-toast. Kind→type: Error→error, Input/Approval→warning, else info.
- **Obs layout = OMC stat set, no $** (`status_bar.rs` `obs_zone` + `ObsView`): dropped the cost ($)
  segment; now renders **model · 5h/wk/sn quota pills · output-style · session-time · ctx**. New
  `quota_pill` (OMC-style `5h[####------]62%`, color by utilization via `ascii_bar`/`quota_color`).
  `ObsView` gained `five_h/weekly/sonnet: Option<u8>`.
- **STAGED next (the meaty, security-sensitive slice the operator chose):** the bar shows quota pills as
  `—` because the %s need an **authenticated usage fetch**. Operator picked **"MoonlightCode fetches
  usage itself"** (works without OMC). That slice: read the Anthropic OAuth token from
  `~/.claude/.credentials.json`, call the usage endpoint (OMC's exact endpoint NOT yet pinned — it's not
  in `rate-limit-wait/rate-limit-monitor.js`; it's elsewhere in the OMC bundle / or the Anthropic usage
  API), parse `fiveHourPercent`/`weeklyPercent`/`sonnetWeeklyPercent` + reset times, cache+refresh on a
  timer. Reference shape: OMC caches the same at `~/.claude/plugins/oh-my-claudecode/.usage-cache-anthropic.json`
  (`{data:{fiveHourPercent, fiveHourResetsAt, weeklyPercent, weeklyResetsAt, sonnetWeeklyPercent, …}}`).
  Also in that slice: switch obs **sourcing to transcript-direct** (exact ctx%/session-time) and **remove
  the `--settings` statusline override** (`obs::with_statusline` at 4 launch sites + the `statusline`
  subcommand) to honor [[moonlightcode-preserve-omc-hud]] (keep OMC HUD in embedded terminals).
- ⚠️ **Runtime-verify**: (1) the bar is taller + readable; (2) a new notification (phase change / needs-
  input / review) pops a toast that fades after a few seconds; (3) the obs cluster shows model/persona/
  session-time/ctx with `5h/wk/sn —` quota placeholders, no `$`.

## STATUS BAR — notification click-to-navigate — DONE (2026-06-04) — green, 236 tests
Clicking a notification row now **jumps to its session's tab** (besides marking it read + closing the
popover). UI lane, additive.
- New **`OpenRequest::SessionById { id, root }`** (`center_requests.rs`) — keyed `session:{id}` (dedups
  onto an open tab) + `target_root` (lands in the session's space). `open_in_center` reconstructs the
  monitor from the managed store (resume if managed, else read-only transcript) — mirrors layout-restore;
  pure view (no adoption change).
- `Notification.session` (already stored) now flows into `NotifRow.session`; the row click calls new
  **`Workspace::open_notification(id, session)`** (replaces `mark_notification_read`) → marks read, closes
  the popover, resolves the session's `root` from the store, emits `SessionById`.
- ⚠️ **Runtime-verify**: click a session-bearing notification → its focus tab opens/fronts (resumes if
  managed) in its own space; clicking it again reuses the tab.

## STATUS BAR — CC status notifications (phase-end + needs-input) — DONE (2026-06-04) — green, 221 tests
Extends the Phase-2 notification center: the 🔔 now also fires when a session **changes phase** ("a
phase ended / the next began") or enters **`WaitingInput`** ("CC needs your input"). UI lane, additive —
all in the existing bus-folding task (`workspace.rs`), no engine changes.
- **2 new `NotificationKind`s** (`notifications.rs`): `Phase` (glyph `↗`, accent) and `Input` (glyph `◐`,
  WaitingInput amber) + `kind_style` arms (`status_bar.rs`).
- **Transition detection** (`workspace.rs` bus task): tracks `phases`/`statuses` maps per session.
  `SessionUpserted` (where real transitions publish the full row) diffs phase → `phase_notification`
  ("`{prev} → {new} · {label}`") and status → `status_notification`. `PhaseTransitioned` (thin path) and
  `SessionStateChanged` are handled too, **deduped against the same maps** (a transition that arrives via
  both notifies once). `status_notification` surfaces `WaitingInput` ("Needs your input") and `Errored`
  (replaces the old Errored-only arm). **Only witnessed changes notify** (`prev.is_some()` / map had a
  prior) — first-sight is skipped, so a fleet already in those states at startup doesn't spam.
- Two cx-free helpers `phase_notification` / `status_notification` keep the arms DRY.
- ⚠️ **Runtime-verify**: (1) drive a managed session past a phase boundary (e.g. Discovery→Plan, or the
  Test "tests done" advance) → a `↗` notification with the prev→new phases + a bell badge; (2) when CC
  blocks for the operator (status → WaitingInput) → a `◐` "Needs your input" notification; (3) no
  notification storm on app startup for a pre-existing busy fleet (first-sight suppressed); (4) clicking
  a row still marks it read / drops the badge.

## STATUS BAR — Phase 3 (Claude-obs via statusline JSON) — DONE (2026-06-04) — green, 210 tests
The right-zone obs cluster now shows **real per-session metrics** from Claude Code's statusline JSON.
Decoupled from the gating socket (no engine/control changes); external observation, off-bus (like the
session-monitor transcript poll).
- **`moonlight statusline` subcommand** (`main.rs` + new **`apps/desktop/src/obs.rs`**): CC runs it as
  the `statusLine` command for **app-launched sessions only** — wired via a `--settings '<file>'` flag
  appended to every `claude` launch (`obs::with_statusline`, applied at all 4 sites: workspace
  new/restore, session_monitor resume_command/relaunch). `--settings` **merges** (CC: "additional
  settings"), so the user's global statusline (OMC HUD) + our PreToolUse gating hook are untouched; the
  flag only overrides statusline for embedded sessions. `obs::ingest` parses model/cost/duration/
  output_style/exceeds_200k (defensive `#[serde(default)]`), derives **ctx tokens** from a bounded
  256 KiB tail of `transcript_path` (latest `usage`, prompt-side), and atomically drops
  `<support>/obs/<session_id>.json`. Echoes a compact line so CC's statusline isn't blank. +4 tests.
- **UI read-model** `views/obs_store.rs` (`Entity<ObsStore>` in `ShellDeps`): the `Workspace` polls
  `obs::load_all()` every 1.2 s into it (loop gated on the workspace weak handle, since the store is
  global) and re-renders the bar. `status_bar.rs` `obs_zone(Option<ObsView>)` renders model · time+ctx ·
  cost · persona for the **selected session** (`ActiveContext::Session.id`, now read), `—` when a
  file/grid is frontmost.
- **NOT in the statusline payload → still `—`**: session/weekly **quota**, **skills**, **subagents**.
  Those need a different source (transcript/ccusage or hooks) — a Phase 4 if wanted.
- ✅ **CLI verified**: piping a sample payload to `moonlight statusline` prints
  `⌬ Opus 4.8 · ◷ 1h12m · — ctx · $0.42` and writes a correct obs JSON.
- ⚠️ **Runtime-verify (GUI)**: (1) launch a managed session → focus its tab → the right zone fills in
  (model/time/cost/persona; ctx once the transcript has a usage line); switch to a file → obs → `—`.
  (2) **Critical safety check**: confirm `--settings` did NOT disable gating — an Edit is still **denied**
  in Discovery for an app session (the user's PreToolUse hook must survive the merge). (3) the embedded
  terminal's own statusline now shows our compact line instead of the OMC HUD (acceptable; app bar has
  obs) — chain-preserving the OMC HUD is a follow-up. (4) observed/external sessions get no obs (only
  app-launched ones carry `--settings`).

## STATUS BAR — Phase 2b (clickable EOL via editor-command channel) — DONE (2026-06-04) — green, 200 tests
The deferred click-to-change, done safely for **EOL** (LF ↔ CRLF). UI lane, additive.
- **New `views/editor_commands.rs`** — `EditorCommands` event hub (`Entity` in `ShellDeps` + `init_shell`
  + `main.rs` seed) emitting `EditorCommand::SetEol(Eol)`. The status bar can't reach the active editor
  directly, so it broadcasts here; every `CodeEditorPanel` subscribes and **only the active one acts**
  (verified: the dock calls `set_active(false)` on the outgoing tab, tab_panel.rs:230 → exactly one active).
- **EOL is a save-time attribute, not buffer content** (sidesteps gpui-component CRLF normalization):
  `Eol::toggled()` + `Eol::apply(text)` (normalize→`\n`, then to target) on `active_context.rs`;
  `CodeEditorPanel::set_eol` flips `self.eol` + marks dirty + re-announces (no buffer edit, no `window`);
  `save()` writes `self.eol.apply(&value)`. +1 test (`eol_toggle_and_apply_normalize_then_convert`).
- **UI** (`status_bar.rs`): the EOL pill is now a clickable `eol_item` (hover affordance) → `Workspace::
  set_editor_eol(eol.toggled())` → channel. `LeftZone::File.eol` now carries `Eol` (bar renders the label).
- **Still display-only**: encoding (always UTF-8 — files read via `from_utf8_lossy`) and indent
  (conversion = risky reflow; deferred). Those would ride the same channel when wanted.
- ⚠️ **Runtime-verify**: open a file → click `LF`/`CRLF` in the bar → it flips, the tab shows the dirty
  dot, and ⌘S writes the file with the chosen ending (check bytes); switching tabs targets the right editor.

## STATUS BAR — Phase 2 (notification center) — DONE (2026-06-04) — green, 189 tests
The 🔔 is now a real in-app **notification inbox**, folded from the engine bus. UI lane, additive.
- **New `views/notifications.rs`** — a gpui-free, unit-tested store: `Notifications` (newest-first,
  capped at 100, unread count) of `Notification{ id, kind, text, session, read }` +
  `NotificationKind{ Review, Approval, Advance, Error }`. `push`/`mark_read`/`mark_all_read`/`clear`.
  +3 tests. Stored as `Entity<Notifications>` in `ShellDeps` (+ `init_shell` param + `main.rs` seed).
- **Bus folding** (`workspace.rs`): the existing `PlanProposed`/`ReviewReady` bridge task now also
  tracks session **labels** (off `SessionUpserted`) and pushes a notification for `PlanProposed`,
  `ReviewReady`, `ApprovalRequested`, `PhaseAdvanceRequested`, and errored `SessionStateChanged` —
  each text is "<what> · <session label or short id>". `Workspace` gains `cx.observe(notifications)`
  (re-renders the bar) + `mark_all_notifications_read`/`clear_notifications`/`mark_notification_read`.
- **UI** (`status_bar.rs`): the bell shows an **unread badge**; clicking toggles a popover with a
  header (**Mark all read** · **Clear**) over a newest-first list — each row = kind glyph+color
  (`kind_style`) + text, dims when read, marks itself read on click. `StatusSnapshot` carries
  `notifications: Vec<NotifRow>` + `unread`, built in `Workspace::render`.
- **DEFERRED — click-to-change metadata (EOL/encoding/indent):** intentionally NOT done this slice.
  The bar holds the *resolved* `ActiveContext` (path/caret/eol/indent), not a handle to the active
  `CodeEditorPanel`, so a bar-driven buffer edit can't update the editor's cached `eol`/`indent`
  (the editor's next caret-move announce would revert it) and risks indentation reflow. Needs a small
  **active-editor command channel** (bar → the frontmost `CodeEditorPanel`) — that's Phase 2b. EOL is
  the only safe one-shot transform; indent/encoding conversion is the heavier part. Encoding is always
  UTF-8 today (files read via `from_utf8_lossy`).
- ⚠️ **Runtime-verify**: (1) a session reaching review / proposing a plan / erroring raises a bell
  badge; (2) opening the popover lists them newest-first with the right glyph/color; (3) clicking a
  row dims it + drops the badge count; Mark-all-read / Clear work; (4) labels read as the session
  title (short id before the first `SessionUpserted`). Follow-ups: click-to-navigate (the `session`
  field is already stored), toast bridge via `Root::render_notification_layer`, persistence.

## STATUS BAR — Phase 1 (shell + local data) — DONE (2026-06-04) — green, 174 tests
JetBrains/RustRover-style **bottom status bar**, the mirror of the top `space_tab_bar`:
window chrome rendered as the last child of `Workspace::render`'s v_flex (NOT a dock
Panel → survives layout/space switches, **no `DOCK_VERSION` bump**). UI lane, fully additive.
- **New `views/active_context.rs`** — a shared `Entity<ActiveContext>` (clone of the
  `ActiveEditor` pattern) the frontmost center tab announces into: `None | File{path,
  line, column, eol, indent} | Session{id, label, status, phase}`. Carries `Eol`/`Indent`
  detect helpers (`Eol::detect`, `Indent::detect` — tabs vs smallest ≥2-space run, ignores
  ` *` comment artifacts). +2 unit tests.
- **New `views/panels/status_bar.rs`** — `status_bar(StatusSnapshot, cx) -> impl IntoElement`.
  Three zones: LEFT = file path (rel. to `ProjectSpace::root()`) + `Ln L:C` + EOL + UTF-8 +
  indent, OR `◐ name : State · phase-chip` when a session tab is frontmost; CENTER-RIGHT =
  `⬡ project` + `🎨 Moonlight`; RIGHT = the Claude-obs cluster as `—` placeholders
  (model/ctx/time/quota/skills/subagents/persona — Phase 3 data gap); FAR-RIGHT = `🔔`
  with a stub popover (`Workspace::toggle_notifications`).
- **Wiring**: `ShellDeps.active_context` (+ `init_shell` param + `main.rs` seed); `Workspace`
  gains `notifications_open` + a `cx.observe(active_context)` → re-renders the bar on any
  caret move / tab switch / live session update; `render` builds the snapshot.
- **Caret**: `CodeEditorPanel` `cx.observe`s its `InputState` (notifies on cursor move; blink
  is a *separate* entity so no spam) and pushes `ActiveContext::File` deduped on position;
  `set_active` announces; EOL/indent detected on open + re-detected on format. `SessionMonitor`
  announces `ActiveContext::Session` from `set_active` + re-announces on every folded
  `EngineEvent` while active. Both gate pushes on a new `active: bool`.
- **Obs source = decided later** (Phase 3): `SessionObservability` side-record (OFF `Session`)
  + `EngineEvent::ObservabilityUpdated`; source TBD (statusline JSON vs hooks vs transcript).
  Plan file: `~/.claude/plans/resume-modular-stroustrup.md`.
- ⚠️ **Runtime-verify** (RustRover ▶ Run moonlight): (1) open a file → LEFT shows rel-path +
  `Ln:Col` updating as the caret moves + correct EOL/indent; (2) focus a session tab → LEFT
  flips to `name : state · phase`, back to a file reverts; (3) switch space → `⬡ project`
  updates; (4) `🔔` toggles the stub popover; (5) bar persists across a space switch + restart.
  Minor v1 gaps: closing the last dynamic tab can leave the prior context shown (no panel
  re-announces None); EOL/indent are detect-on-open (no click-to-change yet — Phase 2).

## PER-HUNK REJECTION-AS-FEEDBACK (the signature primitive) — DONE (2026-06-04) — green, 174 tests
Operator goal "point 2": complete the product's differentiating loop (FR18/FR23) — reject a hunk →
structured feedback reaches the agent → it self-corrects. Root cause it was half-built: the domain
model (`review.rs`: `ReviewHunk`, `HunkDecision::Reject{feedback}`, `FeedbackOrigin::HunkRejection`)
was fully designed but **unused**, the code-review tab showed only whole-file diffs + a whole-review
`DenyAction`, and the only `ControlPort` wired is `ObserveOnlyControl` — so `inject_feedback` (the
FR18-20 port verb) **never delivers** (returns `Unavailable`); the one real delivery path is the
held-hook deny, which isn't available at review time (no hook is held after the agent is Done).
- **Delivery decision — "Option A" (operator-chosen after an A/B/C discussion):** deliver feedback by
  **writing it into the session's embedded CC terminal** via the existing `SessionIo` registry (same
  mechanism `plan_review` uses to drive CC's native dialog). Real today for managed/resumed sessions;
  the limitation (terminal-less sessions can't be pushed) is surfaced in-UI. The cleaner long-run target
  is **Option C** (a `Steer`-level `ControlPort` adapter whose `inject_feedback` delivers through a
  UI-drained channel — B's placement + A's working mechanism, *not* a `UserPromptSubmit` hook, which
  can't wake an idle agent). `Feedback`/`Command::RejectHunk` stay in place so C is a localized swap.
- **Hunk parsing** (`apps/desktop/src/git/diff.rs`): new pure `parse_hunks(diff, session, file_path)
  -> Vec<ReviewHunk>` splits a file's unified diff on `@@` headers (drops the `diff --git`/`---`/`+++`
  preamble; binary/rename → empty → caller shows raw diff). +3 tests.
- **Per-hunk UI** (`panels/code_review.rs`, rewritten): the selected file renders as a list of **hunk
  cards**, each with **Accept / Reject**. Reject opens an inline reason field (mirrors `plan_review`);
  Send builds `Feedback{origin: HunkRejection}`, writes it into the session terminal via
  `SessionIo.terminal(session).send_text(...)`, and emits `Command::RejectHunk` for the audit trail.
  Accept is **visual-only in v1** (selective per-hunk *staging* — partial `git apply` — is a follow-on;
  the steering loop is reject→feedback, which is what changes agent behavior). The dead whole-review
  "Request changes"/`DenyAction` button is removed; **Approve** still advances Review → Commit. A
  terminal-less session shows "feedback captured but not pushed" instead of silently auditing nothing.
- **Honest audit** (`crates/engine/supervisor.rs`): `Command::RejectHunk` now routes to a new
  `record_injected_feedback` (audit `FeedbackInjected` + publish `AuditAppended`) **instead of**
  `inject()` → the observe-only port (which would log "not delivered"). Delivery happened UI-side, so
  the engine's job is just the FR33 audit. +1 test (`reject_hunk_audits_feedback_without_touching_the_port`).
- **Marks progress on:** FR18 (reject→feedback), FR23 (per-hunk accept/reject), T5 (review tab now
  per-hunk, not just per-file). **Still open:** FR23 selective *staging*; T4 summary ("what this
  covers"); T6 real inject channel (→ Option C); delivery for terminal-less/observed sessions.
- ⚠️ **Runtime-verify**: (1) a session reaching Review opens the tab with per-file hunks; (2) Reject a
  hunk → reason field → Send writes the feedback line into the embedded CC terminal and the agent picks
  it up as a new turn (managed/resumed session); (3) Accept marks the hunk; (4) a session with no live
  terminal shows the "not pushed" note rather than a phantom audit; (5) Approve advances Review → Commit.

## WORKFLOW-ADVANCEMENT ENGINE (Part B of phase rework) — DONE (2026-06-04) — green, 154 tests
Operator rules: **auto-advance by default**; CC's own done-signal advances the phases CC owns
(Discovery, AutoImplement); **Test/Review/Commit are operator-confirmed gates** ("no more work", CC
idle); a **manual pin overrides auto** (A ≫ B) — a pinned session *asks* before moving and approving
returns it to auto. The workflow is **cyclic** (Commit → Discovery starts the next unit of work).
State machine: `Discovery →(CC done) Plan →(plan keystone) AutoImplement →(CC done) Test →(op:
"tests done") Review →(op: approve review) Commit →(op: "new cycle") Discovery`.
- **Domain** (`phase.rs`): `Phase::next()` is now **total + cyclic** (`Commit→Discovery`, returns
  `Phase` not `Option`); added `Phase::auto_advances_on_done()` (true only for Discovery/AutoImplement).
  `Session.phase_pinned: bool` added (`session.rs`); `ManagedSession`/`ManagedStateUpdate` carry it
  (`ports/store.rs`). `Phase::on_done` **removed** (FR13 revert-to-Plan is gone).
- **Persistence**: **migration 3** `ALTER TABLE managed_session ADD COLUMN phase_pinned INTEGER NOT
  NULL DEFAULT 0` + wired through every SQL site (`store.rs`); pin survives restart.
- **Engine** (`lib.rs`/`supervisor.rs`): new `Command::AdvancePhase` (advance to `next()` + **unpin**)
  and `Command::SetPhasePinned`; new `EngineEvent::PhaseAdvanceRequested { session, to }`.
  `apply_phase(session, phase, pinned)` is the single transition — sets phase+mode+pin, persists,
  audits, nudges CC on Plan, **publishes the full-row `SessionUpserted`** (carries the pin; replaces
  the thin `PhaseTransitioned` from this path), and **fires `ReviewReady` on entering Review** (the
  code-review tab now opens on Review-entry, not on every Done). `Command::SetPhase` (manual pick)
  **pins** (`apply_phase(.., true)`). `apply_status` on `Done`: if `phase.auto_advances_on_done()` →
  auto-advance when unpinned, else emit `PhaseAdvanceRequested` (pinned); other phases hold. The
  `PhaseObserved` reconcile is **guarded by `phase_pinned`** (detection can't clobber a pinned phase).
- **UI**: `session_monitor.rs` — pin chip (🔒 → `SetPhasePinned(false)`), a `Test`/`Commit`
  **advance button** (gated on CC-idle), and a **pinned-ask banner** ("Advance to <to>?") folded from
  `PhaseAdvanceRequested` (`pending_advance` field). `code_review.rs` Approve now also sends
  `AdvancePhase` (Review→Commit). `grid_home.rs` ignores `PhaseAdvanceRequested` (banner lives in the
  session card; surfacing it in the grid's "needs you" is a follow-up).
- ⚠️ **Runtime-verify**: (1) launch in Discovery → CC finishes → **auto** → Plan; approve plan →
  AutoImplement; CC finishes → **auto** → Test; (2) Test "Tests done → Review" button disabled while
  CC runs, enabled when idle → Review (code-review tab opens); (3) code-review **Approve** → Commit;
  Commit "Committed → new cycle" → **Discovery** (loop); (4) **pin** (click a phase) shows 🔒, next
  done-checkpoint raises "Advance to <next>?" instead of moving, **Approve** advances+unpins, the 🔒
  chip unpins without advancing; (5) restart → a pinned session stays pinned (migration 3); (6) no
  orphaned `claude` across phase changes; detection plan/auto reconcile doesn't override a pinned phase.
- **Follow-ups**: surface `PhaseAdvanceRequested` in the grid "needs you" queue; auto-fire
  Commit→Discovery once a real git-commit is detected (today it's the operator "new cycle" button);
  sharper checkpoints once **T4** (summary capture) + **T6** (precise hooks→app done signals) land.

## MANUAL PHASE CONTROL (Part A of phase rework) — DONE (2026-06-04) — green, 148 tests
Operator: *"the phase part on a session is not clear — we only have the current phase, not the
incoming one, and it never goes to Test/Commit. Should be settable manually, or is it linked to the
missing review part?"* **Root cause:** the `Phase` enum models a 6-stage workflow
(`Discovery→Plan→AutoImplement→Test→Review→Commit`) but only **3 stages were reachable** — the only
transitions were `toggle_mode` (→ Plan/Discovery/AutoImplement), `on_detection::PhaseObserved` (plan↔auto
reconcile → Plan/AutoImplement), and `on_done` (→ Plan). **`Test`/`Review`/`Commit` were orphan states**
nothing ever entered, and the selector conflated Mode (Plan/Discovery/Auto substrate) with Phase. It IS
linked to the missing review part: there is no **workflow-advancement engine** (Auto→Test→Review→Commit)
and nothing wires review-gate approval to advance the phase — that's **Part B** (next).
- **Decision (operator):** do **A then B**, and **A (human) always overrides B (auto)** — an explicit
  operator phase pick wins over any future auto-advancement. UI: **separate Mode and Phase** rows.
- **Domain** (`crates/domain/src/phase.rs`): `Phase::ALL` (ordered 6) + `Phase::next() -> Option<Phase>`
  (suggested forward step; `Commit` terminal → `None`). +2 tests.
- **Engine** (`crates/engine`): new **`Command::SetPhase { session, phase }`** (any of 6, operator
  authority). `toggle_mode` refactored to delegate to a shared **`apply_phase`** (sets phase + derived
  mode, persists, audits, nudges CC `set_phase` only when entering Plan, publishes `PhaseTransitioned`).
  +1 test (`set_phase_reaches_engine_only_phases_and_derives_mode`).
- **UI** (`session_monitor.rs`): the static "Phase" field is now a **clickable pipeline stepper**
  `Discovery › Plan › Auto › Test › Review › Commit` (current lit via `phase_color`, others clickable for a
  steerable managed session) **+ a "→ next: X" hint** (the incoming phase). New `request_phase` →
  `Command::SetPhase` + optimistic flip; **relaunches CC only when the native substrate changes**
  (plan↔auto) — moving among the auto-phases (Discovery/Auto/Test/Review/Commit) is a PDP-only flip, no
  relaunch. The Mode row (`mode_selector`, Plan/Discovery/Auto) stays as the coarse substrate — Mode and
  Phase are now **two separate rows**.
- **Why detection doesn't clobber a manual Test/Review/Commit today:** `PhaseObserved` only sees CC's
  plan-vs-auto; for an auto observation while we hold a non-plan phase it returns `None` (keeps our phase),
  so a hand-set Test/Review/Commit survives. (Part B will add an explicit operator-pin so even a CC plan
  observation can't override an operator pick — the A>B guarantee.)
- ⚠️ **Runtime-verify**: (1) clicking a phase segment on a managed session flips the card + the PDP follows
  (e.g. Auto→Test stays writable, →Commit goes read-only, →Plan relaunches CC in plan mode); (2) the
  "→ next" hint reads correctly and Commit shows none; (3) no orphaned `claude` after crossing the plan
  boundary; (4) observed/external sessions show the stepper read-only.

### NEXT — Part B: workflow-advancement engine (the "missing review" link)
Build the auto-progression `Auto→Test→Review→Commit` (each step auto-suggested + operator-confirmed),
and **wire the code-review gate approval to advance the phase** (today `on_done` reverts to Plan and fires
`ReviewReady` but the phase never becomes `Review`/`Commit`). Reuse `apply_phase` as the transition fn.
Add an **operator-pin** flag on the session so a manual pick (Part A) always overrides B (operator rule:
human ≫ tools). Depends on **T4** (summary capture) + **T6** (precise done signals via hooks).

## CURRENT FOCUS — operator goals (2026-06-03)
- **G1. Clear task list** for cross-session continuity → this file. ✅
- **G2. Full session management clear in app**: start a session · **validate (approve) its plan** ·
  **review its code**. Gap: start ✅ (＋New session); plan/code review are **display-only** — no real
  accept/reject. → needs the **approval keystone** below (T3).
- **G3. Plan review clear & readable** (+ MCP?). → render markdown properly (UI, T14). MCP decision: it
  is **not** the lever for plan readability (the plan already arrives as markdown via `ExitPlanMode`).
  MCP (`crates/mcp-server`, T8) is for giving the *agent* actor verbs (run tests, open review) and is a
  later enabler — keep separate from readability.
- **G4. End-of-work review panel = per-file compare with previous state** → the code-review tab (T5)
  shows a **per-file diff** (current vs previous/HEAD), file list + side-or-inline diff. Uses the `git`
  module. Refines T5.
- **G5. The app's code-review gate runs `full-review` on the session's diff.** Integrate the
  multi-perspective review *into the app* (NOT a manual rule): when a session reaches review, the app
  triggers `full-review` (sheik + bmad) over its changes and surfaces findings in the review panel,
  alongside the per-file diff (G4). → **T16**. Open mechanism question: app shells out to headless
  `claude -p "/full-review …"` in the session repo? Confirm before building.

### Keystone design — interactive approval via *held* hooks
The hook is synchronous but CC waits for it (≈60s budget), so a `PreToolUse` hook **can hold** while the
operator decides. Mechanism: gate yields `NeedsApproval` → `ControlServer` registers a pending approval,
notifies the app (→ `EngineEvent::ApprovalRequested`), and **awaits** the operator's decision (oneshot,
~30s timeout → deny-with-reason). Operator approves/denies in the cockpit → app resolves the oneshot →
hook returns allow/deny → the session proceeds **in the same turn**. This makes plan-validate (hold on
`ExitPlanMode`) and danger-zone approval real, without owning sessions. Fail-open unchanged when the app
is down (socket absent → immediate allow).

## Lanes (avoid collisions)
- **Engine lane** (`crates/*` — domain, engine, control, detection, trust, persistence, mcp-server):
  events, ports, gating, parsing, supervisor. Edit freely; tested per slice.
- **UI lane** (`apps/desktop/src/**` — views, panels, workspace, term, git): the GPUI
  shell. **HOT** — actively edited by the parallel session. Touch additively, read-before-edit,
  or hand off via a note here. `EngineEvent` is the contract between lanes (exhaustive matches in
  `grid_home::FleetModel::apply_event` + `session_monitor`).

## References (authoritative, in order)
1. `.bmad-output/planning/architecture.md` (hexagonal, event-bus-only, PDP-single-authority, build order)
2. `.bmad-output/planning/prd.md`, `.bmad-output/planning/ux-design-specification.md`
3. `.bmad-output/planning/control-adapter-design.md` (hook posture, increments, open questions)
4. `.bmad-output/spike-0-findings.md` · prior notes in `.ai/handoffs/`

## SPACE-SCOPED CENTER TABS (#5) — DONE (2026-06-03) — green, 130 tests
Operator: *"Open code files should be linked to a project — a file opened in one space must
not be displayed in another."* Done: the center's **dynamic tabs are now per-space**.
- **`workspace.rs`**: `Workspace.open_panels` → `space_panels: HashMap<Option<SpaceId>,
  HashMap<key, Arc<PanelView>>>` (`None` = Overview) + `current_space`. `open_in_center` scopes the
  tab to `current_space`. A new `switch_space(new)` (driven by `cx.observe_in` on the `ProjectSpace`,
  fired when `active()` changes) **removes the outgoing space's tabs and mounts the incoming space's**.
- **GridHome stays mounted across all spaces** (never tracked in `space_panels`) — it must not be
  rebuilt, because detection emits **deltas only** (`crates/detection`: per-session tail state), so a
  fresh GridHome would show an empty fleet until activity. It re-scopes via its own `ProjectSpace`
  observe (`FleetModel::visible`).
- **Close-safe**: `switch_space` reconciles the outgoing set against a live `dump()` (`live_center_keys`
  walks the `PanelState` tree, reconstructing each tab's `OpenRequest::key` via `panel_state_key`), so
  a tab the operator closed with "×" isn't resurrected on return. `prune_closed_spaces` drops tab sets
  for closed spaces. **Live SessionMonitor terminals survive** a space switch (the `Arc` is retained
  while unmounted → PTY keeps running).
- **Clean center on restart**: after a successful layout restore, `reset_center_to_home` replaces the
  center with a fresh GridHome-only home (factored `home_center`), discarding restored dynamic tabs so
  per-space tracking starts known-empty. **Tradeoff (v1):** open files do **not** persist across an app
  restart yet — the rails + active space do. Follow-up: persist per-space open tabs (e.g. into
  `.moonlight/`) and rehydrate on space activation.
- ⚠️ **Runtime-verify**: open file F in space A → switch to B (F gone, B's own tabs shown) → back to A
  (F back); close F via "×", leave + return (F stays closed); a managed session's embedded terminal
  keeps running across a space switch; Overview has its own independent tab set. Minor: tab *order*
  within a space isn't guaranteed stable across switches (HashMap iteration) — cosmetic follow-up.

## ⚠️ BUGFIX: tests were clobbering the real user config (2026-06-03)
Operator saw **phantom spaces** they "never created / already closed" reappear. Root cause:
`ProjectSpace::save()` always wrote `~/Library/Application Support/MoonlightCode/projects.json`,
and the unit tests (which build `ProjectSpace::default()` and call `open_root`/`recent_is_capped`/…)
therefore **overwrote the live user config** with test data (`/tmp/p*` recents, test spaces) on every
`cargo test`. Fix: `ProjectSpace` now holds `persist: Option<PathBuf>` — `Default` (tests) = `None`
(in-memory only, never writes disk); `load()` sets it to `state_path()` so the live app persists
normally. Proven: `projects.json` mtime is unchanged across a `cargo test` run. Also reset the
polluted file to clean defaults (`spaces:[]`, `active:null`, `recent:[]`). **No auto-creation path
exists** — `main` only `ProjectSpace::load()`s; `focus`/`record_managed_session` only act on existing
spaces; `open_root` (＋ New / file-tree Open…) is the sole user-initiated creator. With zero spaces,
`active=None` → `visible(None)` → the fleet is global/unscoped (operator's required behavior).

## SPACES = TOP-TAB SWITCHER + SCOPED FLEET (#5 redesign) — DONE (2026-06-03) — green, 130 tests
Operator: *"Idealise spaces like projects in IntelliJ/GitKraken — a space contains & displays
only its related code + sessions; switch via a top tab bar; full-width global overview."* Done.
- **Top tab bar** (`panels/spaces.rs` rewritten from a dock Panel → `space_tab_bar(tabs,
  overview_active, cx)` render fn): rendered by `Workspace` **above** the `DockArea` — `⊞ Overview │
  <space tabs, each with ×> │ ＋`. It's window chrome, not a dock panel. Click a tab → `select_space`;
  `⊞ Overview` → `select_overview`; `＋` → folder picker → `open_root`; per-tab `×` → `remove_space`.
- **The left Spaces rail is GONE.** `reset_default_layout` left dock is back to `v_split(file tree /
  structure)` @300px. `register_panel("Spaces")` removed. **`DOCK_VERSION` 4→5** (old layouts rebuilt).
- **Space-scoped fleet** (`grid_home.rs`): `FleetModel::visible(active_root)` filters sessions to those
  whose `attached_path` resolves to the active space root (overview/`None` = whole fleet). `GridHome`
  now `cx.observe`s the `ProjectSpace` and reads `active_root()` so switching a tab re-scopes the grid
  live; the header counts (`N need you`, total) reflect the scoped set. `needs_count` removed (folded in).
- **Workspace** gained `pub(crate)` `select_space/select_overview/remove_space/new_space` (the tab bar
  calls them) + a `cx.observe(focus)` so the bar re-renders on switch. Render is now a `v_flex`
  `[ space_tab_bar | dock_area(flex_1) ]`.
- **Removed** the now-orphaned `OpenRequest::ResumeManagedSession` (added for the old rail's per-session
  resume sub-rows — resume now happens by clicking a tile in the scoped fleet, via `OpenRequest::Session`).
- **Per-space roster still persists** (`<root>/.moonlight/sessions.json`, `record_managed_session`) —
  it's just no longer *displayed separately*; a space's sessions surface as scoped tiles in the fleet.
- **Tests**: `visible_scopes_to_the_active_space_root` (grid_home) + the user-created-spaces suite from
  the refinement below. 58 desktop / 130 total, clippy clean.
- ⚠️ **Runtime-verify**: (1) top tab bar renders above the dock; clicking `⊞ Overview` shows the whole
  fleet, clicking a space scopes the grid to its sessions **and** roots the tree/terminal; (2) `＋` opens
  the folder picker → new active space tab; (3) per-tab `×` closes a space (falls back to Overview);
  (4) the v5 layout rebuilds (no stray left Spaces rail).

## USER-CREATED SPACES (#5 refinement) — DONE (2026-06-03) — model layer (the switcher UI is now the top-tab bar above)
Operator: *"Spaces are user-defined, not auto-defined — I create them via a New Space feature
(like opening a new project in IntelliJ)."* Spaces are **no longer auto-materialized** from
session activity; they are born only from an explicit folder-open. (The switcher that drives these
methods is the **top tab bar** from the redesign section above, not a rail.)
- **`project_space.rs`**: `open_root` (the explicit open) is now the **only** space creator
  (via `ensure_space`). `focus()` and `set_follow_focus(true)` **switch to a session's space
  only if it already exists** (new `space_id_for_root` non-creating lookup) — focusing a session
  in an unopened folder neither creates a space nor moves the rails. `record_managed_session`
  records only into an **existing** space (no-op otherwise). New **`remove_space(id)`** ("Close
  Project": drops from the list, falls back to overview, leaves files/`.moonlight/` intact).
- **Tests** for user-created semantics: `focus_does_not_create_a_space_only_switches_to_existing`,
  `remove_space_drops_it_and_falls_back_to_overview`, `record_managed_session_writes_roster_for_an_open_space`
  (no-op before `open_root`, writes after), `follow_toggle_gates_session_rerooting` (opens spaces first).

## PROJECT SPACES (#5) — DONE (2026-06-03, this session) — build+tests green (115)
Generalized the single-active-root `ProjectSpace` into **N project spaces** + an active
selection, with a left **Spaces monitor** rail to switch between them. UI lane.
- **Model** (`views/project_space.rs`): `ProjectSpace` now holds `Vec<Space{ id, root, label,
  sessions }>` + `active: Option<SpaceId>` (`None` = overview/"All" → `root()` falls back to cwd).
  `SpaceId` = the absolute root path (one space per root → trivial find-or-create dedup).
  `open_root` ensure-creates a space and activates it; `focus(session, attached_path)` **switches to
  the session's space** (creating it if new) and re-roots onto it *while follow-focus is on* — so
  focusing a session now switches to its space (follow-focus extended from 1 root → N spaces).
  New: `spaces()`, `active()`, `select_space(id)`, `select_overview()`. Public API (`root`, `focus`,
  `open_root`, `set_follow_focus`, `recent`, `follow_focus`, `session`) preserved, so file_tree /
  terminal / grid_home / workspace are unchanged callers.
- **Persistence** (global `projects.json`): stores `spaces` (id/root/label) + `active` + `recent` +
  `follow_focus` (`#[serde(default)]` on each → old/short files degrade gracefully). A space's
  `sessions` roster is **`#[serde(skip)]`** in `projects.json` — it is hydrated from the **per-space
  roster file** instead (see next section). A dangling `active` that no longer names a known space is
  dropped on load.

### Per-space managed-session roster — DONE (2026-06-03) — operator: "store per-space managed CC sessions"
**Storage split** (operator decision): global `projects.json` keeps the space *registry*; each space's
**managed CC sessions** are stored **repo-local** in `<root>/.moonlight/sessions.json` (JetBrains
`.idea`-style). Note: the engine lane's global `moonlight.db` (`ManagedSessionStore`) *also* records
managed sessions (with `root`) for live supervision + the FR33-34 audit log — it stays canonical for
*running state*; the per-space file is the space's **roster** (membership + resumable snapshot), keyed
by the same session ids. Resume is `claude --resume <id>` either way, so the two don't conflict.
- **`views/space_sessions.rs`** (NEW): `SpaceSession { id, mode, phase, label }` + `load/upsert/remove`
  over `<root>/.moonlight/sessions.json`. The `.moonlight/` dir self-ignores (writes a `.gitignore` of
  `*` on first save) so it never shows in the user's `git status`. Defensive IO (empty on missing/unreadable).
- **`project_space.rs`**: `Space.sessions: Vec<SpaceSession>` (was `Vec<SessionId>`), hydrated from the
  roster file in `Space::from_root` + on `ProjectSpace::load`. New `record_managed_session(root, rec)`
  write-through (file + in-memory). `focus()` **no longer** binds a session into the roster — only the
  app launching a managed session records one (observed/focused sessions don't pollute the roster).
- **`workspace.rs`**: the `NewManagedSession` launch arm now ALSO calls `record_managed_session` (after
  the existing `store.upsert_managed` to `moonlight.db`), so a launched session lands in its space's
  roster + the rail updates live. New **`OpenRequest::ResumeManagedSession { id, root, phase }`** arm →
  `claude --resume <id>` embedded, rooted at the space (same tab key as the session → dedups).
- **`center_requests.rs`**: added the `ResumeManagedSession` variant (+ `Phase` import, key reuse).
- **Rail** (`spaces.rs`): each space row now renders its managed sessions as indented **resumable
  sub-rows** (phase-colored ▸ + label/short-id); click → emits `ResumeManagedSession`. `SpacesPanel::new`
  gained a `center` arg. Coordination: added `pub mod space_sessions;` to `views/mod.rs` (UI-lane, mine).
- ⚠️ **Runtime-verify**: (1) launching a managed session writes `<root>/.moonlight/sessions.json` and the
  rail shows it under its space; (2) after restart the roster rehydrates and **clicking a session resumes
  it** (`claude --resume`) in an embedded terminal rooted at the space; (3) `.moonlight/` stays out of
  `git status`; (4) no drift surprises between the per-space file and `moonlight.db` (different consumers).
- **Rail** (`views/panels/spaces.rs`, NEW): `SpacesPanel` — lists "◎ All" (overview) + one row per
  space (label · root · session-count chip), highlights the active one, click → `select_space` /
  `select_overview` + `cx.notify()`. It only mutates the shared `ProjectSpace`; the tree/structure/
  terminal already `cx.observe` that entity and re-root on the `root()` change, so the rails follow
  for free. Observes `ProjectSpace` to re-render as spaces appear / active moves.
- **Layout** (`workspace.rs`): registered `"Spaces"` panel; left dock is now a **horizontal split**
  `[ Spaces rail (168px) | vertical(file tree / structure) ]` (was just the vertical tree/structure).
  Left dock default width 300→480px. **`DOCK_VERSION` 3→4** (left-rail shape changed → old layouts
  rebuilt). Spaces seeded with `deps.focus` (the shared `ProjectSpace`).
- **Coordination note:** added one additive line `pub mod spaces;` to `panels/mod.rs` (the
  Tab-polish-owned file) — unavoidable to register a new panel module; it's a top-of-file `mod`
  declaration, away from the close-button helper, so a merge conflict is unlikely.
- ⚠️ **Runtime-verify**: (1) the nested left split renders (thin Spaces column beside tree/structure)
  and the 168px rail size holds; (2) clicking a space row re-roots the tree **and** terminal live;
  (3) focusing a session in a *different* root switches the active space in the rail; (4) spaces +
  active selection survive a restart (and the rebuilt v4 layout shows the rail).

## DISCOVERY PHASE-ONLY + PLAN OPENS IN ITS PROJECT — DONE (2026-06-04) — green, 148 tests
Two operator fixes (complements the parallel "phase rework Part A": `phase_stepper`/`request_phase`/
`Phase::ALL`/`Phase::next`).
- **Discovery de-duplicated (phase-only).** Discovery was both a `Mode` *and* a `Phase`, and the session
  card showed BOTH a `phase_stepper` (full pipeline) and a redundant Plan/Discovery/Auto `mode_selector`.
  Now: **`Mode` is back to `{Plan, Auto}`** (the CC-native binary); Discovery lives only on `Phase`. The
  card keeps the single `phase_stepper`; the `mode_selector`/`request_mode`/`current_mode`/`mode_for_phase`
  (Mode-based dupes) are **deleted**. `Phase::operator_mode()` now: Plan→Plan, every other phase→Auto.
  `Mode::starting_phase()` removed. Launch/selector now carry a **`Phase`** end-to-end:
  `OpenRequest::NewManagedSession{ id, phase }`, grid_home `new_session_phase`, `Command::SetPhase`
  (the pre-existing `ToggleMode` command was removed — `SetPhase` already existed and is what the UI uses).
  Rules unchanged: Discovery = CC `auto` + PDP denies edits.
- **Plan opens in the emitting session's project**, not the live one. `OpenRequest::PlanReview` now carries
  the session `root` (the bus→center bridge fills it from its `roots` map; `CodeReview` already had it).
  `open_in_center` calls new **`activate_space_for_root`** for any request with a `target_root()` (plan /
  code review) → switches to that root's space (mounts its tab set + re-roots rails via `switch_space` +
  `select_space`) **before** scoping the tab, so the gate lands in the right project. No-op if that root
  has no open space (→ stays in the current/overview) or is already active. `space_id_for_root` made `pub`.
- ⚠️ **Runtime-verify**: (1) a plan proposed by a session in project B switches to B and opens there while
  you're in A; (2) the session card shows ONE phase control (the stepper), no separate Mode row; (3)
  launching from grid_home/tab-bar still respects the chosen starting phase (Plan/Discovery/Auto).

## TAB-BAR ＋SESSION + OPERATOR-SETTABLE TRUST — DONE (2026-06-04) — green, 145 tests
Two operator-UX asks.
- **Quick ＋Session in the top tab bar** (`panels/spaces.rs` + `workspace.rs`): the space tab bar now
  ends with a right-aligned **`＋ Session`** (accent) and **`＋ Space`** (muted) pair — no trip to the
  Sessions grid to launch CC. New `Workspace::new_session` mints a fresh uuid and emits
  `OpenRequest::NewManagedSession { mode: Plan }` on the center (safe default; switch live from the card).
  The old bare `＋` (folder picker) is relabeled `＋ Space` to disambiguate.
- **Trust tier is now operator-settable** (was always `Observed`, read-only). `session_monitor.rs`: the
  read-only Trust fact became a segmented selector **Observe · Read · Std · Trusted** (`trust_selector` /
  `request_trust`), mirroring the mode selector — optimistic card update + `Command::SetTrust`. Unlike
  Mode it doesn't steer CC (pure PDP policy), so it's settable for any tracked session.
  - **Engine** (`lib.rs` + `supervisor.rs`): `Command::SetTrust{session,tier}` → `set_trust` (sets
    `trust_tier`, republishes `SessionUpserted`; idempotent). `main::apply_gate_event` **already** folds
    `trust` from `SessionUpserted`, so the PDP picks up the new tier with no extra wiring. +1 test.
  - **Caveat (carried from T1)**: tool-use still passes `verb:None`, so trust doesn't yet modulate
    *tool* danger — it only gates MCP verbs today. So changing trust is visible + wired through the gate
    but has limited live effect until **T1** (graduated danger-by-tier) lands. Trust is **not persisted**
    yet (lives for the session; revisit with the store).
- ⚠️ **Runtime-verify**: (1) ＋Session in the tab bar launches a managed CC session in a new tab;
  (2) the Trust selector flips and the chip reflects it; (3) ＋Space still opens the folder picker.

## AUTO-ADOPT APP-CREATED/IMPORTED SESSIONS — DONE (2026-06-04) — green, 144 tests
Operator: *"auto-adopt sessions created or imported in the app."* Until now, adoption (opt-in to PDP
governance) was manual — so Discovery/Plan gating didn't bite on a freshly-launched session until the
operator clicked the tile's adopt toggle (the very gap that blocked *this* dogfooding session). Now the
app's own sessions are governed by default; **external sessions you only watch stay observe-only**
(day-one safety preserved).
- **Principle**: *app-managed (created or imported) ⇒ adopted*, driven by the store + engine seeding.
- **Engine** (`supervisor.rs`): the `DetectionEvent::Discovered` arm now **seeds `adopted`/`phase`/`mode`
  from `store.managed(id)`** when a record exists — so an app session is gated the instant detection
  sees it (no race: the app wrote the record at launch). New idempotent **`Command::SetAdopted{session,
  adopted}`** (`set_adopted`) for flipping an already-tracked session (publishes `SessionUpserted`,
  persists; no-op if unchanged). `ToggleAdoption` stays for the manual tile toggle.
- **App** (`workspace.rs`): `NewManagedSession` writes its store record with **`adopted: true`** (created
  → adopted via the seed). `OpenRequest::Session` (importing/opening a session into the cockpit) sends
  **`Command::SetAdopted{ true }`** so the live fleet entry is governed immediately.
- **Tests** (+3): discovered-with-record auto-adopts (+ phase/mode seeded); discovered-without-record
  stays observe-only; `SetAdopted` is idempotent (re-set publishes nothing).
- ⚠️ **Runtime-verify**: (1) ＋New session is governed from its first tool call (Discovery denies an edit
  without a manual adopt); (2) opening a discovered session's tile adopts it live; (3) a running external
  session you never open stays unadopted/observe-only. **Persistence note**: imported (observed) sessions
  adopt *live* only — adoption isn't persisted across restart unless they also have a managed row (created
  sessions do). Re-opening re-adopts. Fine for v1; revisit if imported-adoption should survive restart.

## PLAN GATE 3-WAY + ROBUST APPROVE + SESSION RESET — DONE (2026-06-04) — green, 154 tests
Three operator asks (UI lane, `plan_review.rs` + `session_monitor.rs`):
- **Approve wasn't progressing CC.** The held hook emits *no output* = "allow", so CC falls through to
  its **own** continuation dialog in the terminal; we drove it with a bare `Enter` at 500 ms. CC added a
  4th option (1 accept+auto · 2 step-by-step · **3 refine w/ ultraplan · 4 reject**), so Enter-on-default
  became fragile. Fix: send the **explicit digit + Enter** (`"1\r"`) after a longer **900 ms** delay (must
  outlast the cockpit→engine→held-oneshot→hook-return→render round-trip). Factored into
  `select_native_option(digit)`.
- **Reject is now a 3-button bar** + reason input. Footer (when pending) shows **Approve · ✦ Refine with
  ultraplan · ✕ Reject**. *Refine* = `ApproveAction` (allow the hook) then drive CC's **native option 3**
  (`select_native_option("3")`). *Reject* opens an inline `InputState` field (placeholder + autofocus) with
  **Send rejection / Cancel**; Send denies the held hook with the typed reason (empty → `DEFAULT_REJECT_REASON`).
  Reject still uses the hook-deny path (CC blocks `ExitPlanMode`, no keystroke). `reject_input` is cleared
  on re-proposal / resume.
- **Session Reset control.** A `↻ Reset` pill in the card header (managed sessions only) runs CC's
  **`/clear`** in the embedded terminal (a fresh conversation under the same id) and drops the cached
  `ai-title` (`session.title = None`) so the **card + tab name** revert to the short id immediately; CC mints
  a new title that detection folds back in. **Limitation (v1):** the optimistic title reset is local to the
  open card/tab — the **grid tile** keeps the old name until a real new `ai-title` arrives (no engine
  command resets the fleet read-model yet), and a `SessionUpserted` arriving before the new title can briefly
  flash the old name back. Detecting a manually-typed `/clear` (vs the button) isn't wired.
- ⚠️ **Runtime-verify**: (1) Approve advances CC past its 4-option dialog into auto (tune the 900 ms / digit
  if a CC version differs); (2) Refine triggers CC's "refine with ultraplan" (option 3); (3) Reject opens the
  reason field, Send delivers it to the agent, Cancel restores the buttons; (4) ↻ Reset clears CC and the
  card/tab name drops to the short id (then picks up the new title).

## MODE DECOUPLING + DISCOVERY PHASE — DONE (2026-06-04) — green, 134 tests
Operator: *"Can't switch to Auto live; and we're missing a pre-planning **Discovery** step —
let CC run lots of read/inspect commands un-prompted, but **no edits**."* Root-caused with the
operator's own `~/.claude` transcripts + fixed by **decoupling our phase from CC's permission mode**.
- **Root cause of the live-Auto bug** (`session_monitor::request_mode`): the selector injected
  Shift+Tab (`\x1b[Z`) and then *waited for detection* to confirm. But CC writes the
  `{"type":"permission-mode",…}` line **per-turn, not per-keypress** (proven: `plan ×5 → auto ×26`
  repeats), so toggling while the agent is idle never updates `current_mode()` inside the 6×1.2s
  window → loop gives up, card stays Plan. Open-loop racing a per-turn signal.
- **New posture — phase is OUR authority, CC mode is just a substrate.** CC runs in only two native
  modes: `plan` (for `Phase::Plan`, whose *behavior* makes the agent produce a plan) and `auto`
  (every other phase). The per-phase **write policy is enforced by the PDP/hook**, not CC's mode.
  So Discovery/Auto/Test/Review/Commit are all CC-`auto` sessions differing only in our policy.
- **`crates/domain/phase.rs`**: added **`Phase::Discovery`** (CC `auto`, PDP denies edits, *not*
  plan — no forced `ExitPlanMode`). Remapped: `allows_writes` = AutoImplement|Test|**Review**;
  `cc_permission_mode` = `plan` for Plan, **`auto` for everything else** (Commit was `default`→now
  `auto`, read-only via PDP). Added `Phase::operator_mode()` + `Mode::starting_phase()` as the single
  source of truth for the Plan/Discovery/Auto selector. `Mode` (`session.rs`) gained `Discovery`.
- **`crates/trust` (PDP)**: write policy cascades from `allows_writes` (no logic change). +3 tests
  (Discovery denies edits/allows reads, Review allows writes, Commit gate read-only).
- **`crates/engine/supervisor.rs`**: (1) `toggle_mode` now sets the **phase** the PDP gates on for
  *any* mode pick (was Plan-only) — instant, no keystroke; only crossing into Plan also nudges
  `control.set_phase` (no-op on observe-only, PDP enforces regardless). (2) `on_detection::PhaseObserved`
  **no longer clobbers a chosen non-plan phase** — it only reconciles the plan↔non-plan boundary
  (`auto` is consistent with Discovery/Auto/Test/Review/Commit), so Discovery isn't reset to Auto.
- **UI** (`session_monitor.rs`): `mode_selector` is now **Plan · Discovery · Auto**; `request_mode`
  routes through **`Command::ToggleMode`** (engine = authority) + an **optimistic** local card flip,
  and the flaky 6×-Shift+Tab loop is **deleted**. `grid_home` launch toggle gained Discovery; `theme`
  got a Discovery accent. `terminal::encode_key` now maps a human **Shift+Tab → `\x1b[Z`** so the
  operator can still cycle CC's native ring manually (`send_input` removed — no longer used).
- **CC's post-plan transition is CC-driven** (operator note): after a plan is approved, CC itself
  offers to *continue in auto* — so the Plan→Auto native-mode change isn't ours to force.

### Plan approval now drives CC's native continuation prompt — DONE (2026-06-04) — green, 140 tests
Operator: *"accepting the plan isn't reported to CC — at plan time it shows 3 options (1 accept+auto,
2 accept step-by-step, 3 reject+explain) and our Approve doesn't pick one."* Root cause: the held hook
returning **allow** lets `ExitPlanMode` through, but CC then renders its **own** continuation prompt in
the embedded terminal — a separate interactive choice our cockpit never selected, so CC just waited.
- **Fix**: on cockpit **Approve**, after resolving the hook we inject **Enter (`\r`)** into the
  session's embedded terminal to pick **option 1 (accept + auto)** — the default-highlighted choice
  (operator-confirmed: Enter selects #1). 500 ms delay so CC has rendered the prompt first.
- **Wiring** (engine can't reach the embedded PTY — only the UI can): new **`views/session_io.rs`**
  `SessionIo` = a `SessionId → WeakEntity<TerminalPanel>` registry on `ShellDeps`. `SessionMonitor`
  registers its terminal in `build`/`try_resume`; `plan_review::approve` looks it up and sends Enter.
  Restored `TerminalPanel::send_text`. **Reject** stays on the hook-deny path (a denied `ExitPlanMode`
  means CC never shows the continuation prompt, so no keystroke is needed there).
- ⚠️ **Runtime-verify**: (1) Approve in the Plan tab makes CC's terminal advance past the 3-option
  prompt into auto (Enter lands; tune the 500 ms if it's racy); (2) if CC's prompt isn't Enter-on-#1 in
  some version, change the injected bytes in `plan_review::approve` (number-key `"1"` is the fallback).

### Mode change drives CC's native mode via RELAUNCH — DONE (2026-06-04) — green, 141 tests
Operator: *"mode change doesn't trigger changes in CC; prefer relaunch; is there a direct CC command?"*
Answer: **no in-session command exists** — CC sets permission mode only via Shift+Tab (interactive) or
the `--permission-mode` **launch flag**. So we relaunch with the flag (the user's preferred, deterministic
route — and since *we* set the flag, the native mode is exactly known, no detection/tracking needed).
- **`session_monitor::request_mode`**: on a mode pick it (1) sends `Command::ToggleMode` (PDP phase) +
  optimistic card flip as before, and (2) **only when crossing the Plan boundary** (`plan`↔`auto`) calls
  new **`relaunch_terminal`** → respawns the embedded session as `claude --resume <id> --permission-mode
  <plan|auto>`. Discovery↔Auto are both native `auto`, so they **don't** relaunch (PDP-only flip).
- **Safe relaunch**: overwriting `self.terminal` drops the old `Entity<TerminalPanel>` →
  `Emulator::drop` sends `Msg::Shutdown` → the old PTY closes → old `claude` gets SIGHUP. So no
  two-clients-on-one-transcript. Re-registers the new terminal in `SessionIo`.
- ⚠️ **Runtime-verify**: (1) Plan→Auto / Auto→Plan in the cockpit restarts the embedded CC in the right
  mode and `--resume` reloads the conversation; (2) no orphaned `claude` process after a few toggles;
  (3) switching mode *very* early (before CC persisted the session) — `--resume` may need the session to
  exist; relaunch-on-boundary is the operator's explicit action so this is a minor edge.

### `rtk` wrapper made transparent to the classifier — DONE (2026-06-04) — **important for this setup**
Discovered by dogfooding (this CC session, adopted + in Discovery, had `rtk grep` **denied**). Root cause:
`classify::leading_program` didn't know `rtk`, so every `rtk <cmd>` resolved to the unknown program `rtk`
→ **Risky** → denied in read-only phases. Since the operator's CLAUDE.md wraps **every** command in `rtk`,
Discovery/Plan denied *all* bash. Fix: `leading_program` now skips `rtk` (and a following `proxy`) and
classifies the real program underneath (`rtk git push`→git, `rtk grep`→grep/Safe, `rtk rm -rf`→DangerZone).
+test `rtk_wrapper_is_transparent`. (Part of T2.)

### Manual Tab + Shift+Tab in the terminal — DONE (2026-06-04) — green, 141 tests
Operator: *"let plain Tab pass too — CC now proposes an autofill answer I can't accept."* Root cause
**confirmed in the gpui-component source**: its top-level `Root` view binds `tab`/`shift-tab` (context
`"Root"`) for focus traversal, consuming both before the terminal's `on_key_down` — so neither reached CC.
- **Fix**: `terminal.rs` now defines `SendTab`/`SendShiftTab` actions bound in a **`MoonlightTerminal`**
  key context (set on the terminal's focused element via `.key_context`). Because that context sits deeper
  in the dispatch tree than `"Root"`, the bindings **win**: a focused terminal sends `\t` (Tab → shell
  completion / CC autofill) and `\x1b[Z` (Shift+Tab → CC's permission-mode cycle) to the PTY. Bindings
  registered once via `terminal::init_keybindings(cx)` from `init_shell`. `encode_key` no longer emits
  Tab/Shift+Tab (the actions own them → no double-send); new `send_to_pty` helper does the write.
- Applies to **all** terminals (embedded session + standalone operator terminal), so shell Tab-completion
  works in both now.
- ⚠️ **Runtime-verify**: in a focused terminal, Tab accepts CC's autofill / triggers shell completion, and
  Shift+Tab cycles CC's mode — neither moves window focus anymore.
- ⚠️ **Runtime-verify**: (1) the **PreToolUse hook fires + can deny while CC is in `auto`** (the whole
  posture rests on this — the keystone already assumes it, but confirm an Edit is *denied* in Discovery
  and *allowed* in Auto against live CC). **NB:** gating only applies to **adopted** sessions (observe-only
  until then), so Discovery's edit-denial needs the session adopted — same rule as Plan today; consider
  auto-adopting app-launched managed sessions if that surprises the operator. (2) clicking
  Plan/Discovery/Auto flips the card instantly and the gate follows; (3) a human Shift+Tab in the
  embedded terminal moves CC's ring.

## SESSION MODE CONTROL — DONE (2026-06-03, this session) — build+tests green (113) — SUPERSEDED by MODE DECOUPLING above
Operator feedback: *"Cannot change Claude mode in the session form; Phase doesn't align
with CC mode (Phase Auto should use CC `auto`, Phase Plan should use `plan`)."* Both fixed.
- **Phase↔CC-mode mapping** (`crates/domain/src/phase.rs`): `Phase::cc_permission_mode()`
  (Plan→`plan`, Auto/Test/Review→`auto`, Commit→`default`) + `Phase::mode_label()` so the
  shown mode is *derived from phase* and can't drift. Verified CC 2.1.161 accepts
  `acceptEdits, auto, bypassPermissions, default, dontAsk, plan`.
- **Killed the Phase=Auto / Mode=Plan-gated contradiction**: `session_tile.rs` +
  `session_monitor.rs` now render the mode from `phase.mode_label()` (single source of
  truth); `supervisor.rs::on_detection` also aligns `Session.mode` on `PhaseObserved`.
  `phase_from_mode` extended for all six CC strings.
- **Launch-time mode**: a Plan/Auto toggle beside ＋New session (`grid_home.rs`,
  `new_session_mode`) → `OpenRequest::NewManagedSession { id, mode }` → `workspace.rs`
  launches `claude --session-id <id> --permission-mode <plan|auto>` (`starting_phase_for`).
  Default **Plan** (default-deny posture). Seeded into the focus card via
  `SessionMonitor::new_managed(.., phase, ..)`.
- **Live mode selector**: Plan/Auto segmented control in the session-form header
  (`session_monitor.rs::mode_selector`). For a session with an embedded terminal, clicking
  a segment steers CC by injecting its **Shift+Tab cycle** (`\x1b[Z`) via the new
  `TerminalPanel::send_input`, looping until the observed phase matches (≤6 presses, 1.2s
  apart — feedback comes from detection on the bus). **Commit** renders read-only ("Agent
  (commit gate)"). Sessions without a terminal show the selector as a read-only reflection.
- ⚠️ **Runtime-verify**: (1) CC's Shift+Tab cycle order/timing actually lands on the target
  and the card converges; (2) `--permission-mode auto` vs `acceptEdits` — **known caveat:**
  detection only distinguishes plan-vs-non-plan, so the *live* "Auto" toggle lands on the
  first non-plan mode in CC's cycle (may be `default`/`acceptEdits`, not `auto`). Launch-time
  is exact. To make live exact, thread the raw CC mode string through detection (deferred).

## IDE ORG POLISH — DONE (2026-06-03, this session) — build+tests green (50 desktop)
Operator feedback on IDE organization, all UI lane:
- **Editor tabs split from the "…" secondary menu.** Drag-to-split + tab-reorder
  were already library-provided (gpui-component `tab_panel.rs`: `on_panel_drag_move`
  picks the edge third → `split_panel` 4-way; enabled because the center is wrapped
  in a parent stack). Operator wanted a **menu** path too. `code_editor.rs`: added
  `SplitRight/Left/Up/Down` actions + a `Panel::dropdown_menu` impl that injects
  "Split Right/Left/Down/Up" into the tab's "…" menu. Actions are routed to the
  active panel via `menu.action_context(self.focus_handle)` so the `on_action`
  handlers fire (dock's own Zoom/Close still bubble to the parent `TabPanel`). Split
  = **duplicate** the file into a new pane (VS Code semantics) via
  `TabPanel::add_panel_at(placement)`; the duplicate persists across restarts through
  the existing `dump()`/`restore()` path (no `DOCK_VERSION` bump). Error tabs skip it.
- **Session tabs split too — but *moved*, not duplicated.** `session_monitor.rs`: same
  "…"-menu pattern (`moonlight_session` actions + `dropdown_menu` + `action_context`),
  but `split()` **relocates the same `Entity<SessionMonitor>`** (`remove_panel` +
  `add_panel_at`) rather than rebuilding it — a `SessionMonitor` owns a **live embedded
  terminal** (`claude --session-id/--resume`), so a duplicate would put two clients on
  one transcript. Common case is safe: a session shares the center tab panel with the
  always-present (non-closable) GridHome, so the source pane never empties. Edge: splitting
  a session that is **already alone** in its pane re-inserts it into the parent stack
  (harmless relocation, not a true split); a lone + cross-axis split may leave a stray
  blank pane (recoverable by closing). Plan/review tabs still excluded.
  ⚠️ **Runtime-verify** (1) the "…" split entries actually split for editors **and**
  sessions in the running app; (2) the moved session keeps its **live terminal** running
  after the split; (3) the lone-session re-split edge cases above.
- **Ctrl/Shift+Enter = soft newline in the embedded CC terminal.** `terminal.rs`
  `encode_key`: modifier+Enter now emits the xterm meta-Enter sequence `ESC-CR`
  (`\x1b\r`) — the soft newline Claude Code expects — while a bare Enter still
  submits (`\r`). (Alt+Enter already produced ESC-CR via the meta-prefix path.)
  ⚠️ **Runtime-verify** CC interprets ESC-CR as newline in the embedded PTY.
- **Structure beside the code tree.** `workspace.rs` `reset_default_layout`: the
  left rail is now a `v_split(file tree / structure)` (JetBrains-style outline under
  the project tree); the **right dock was removed** (Structure used to live there).
- **Center tabs are closable + draggable.** Root cause: `set_center(DockItem::tab(..))`
  gave the center tab panel **no parent stack**, so gpui-component's `is_locked()`
  made tabs neither closable nor draggable. Fix: wrap the center in
  `DockItem::split(Horizontal, vec![DockItem::tabs(vec![grid])])` (the gpui-component
  idiom) so the tab panel has a parent stack. GridHome stays non-closable
  (`closable()=false`) but is reorderable.
- `DOCK_VERSION` 2 → 3 so persisted v2 layouts are discarded and rebuilt into the
  new shape on next launch.
- **Visible "×" close in the tab bar** (follow-up — operator couldn't *see* a close
  option). gpui-component only exposes close via the easy-to-miss "…" overflow menu;
  no per-tab ×. Added `panels::tab_close_button(Option<WeakEntity<TabPanel>>)`
  (`panels/mod.rs`), returned from `Panel::title_suffix` so it renders in the tab bar
  for the active tab. Each closable center panel (`session_monitor`, `code_editor`,
  `plan_review`, `code_review`, `db_observer`) captures its `WeakEntity<TabPanel>` in
  `Panel::on_added_to` and the button calls `TabPanel::remove_panel(active_panel)` —
  which **bypasses the `closable`/lock gating**, so it works even if the center panel
  were still locked. `GridHome` gets no suffix (stays non-closable).

## TAB/UX POLISH (#3 + #4) — DONE (2026-06-03, Bucket A) — build+tests green (57 desktop)
Operator feedback: *"close option should be on each tab (only exists at the far right of
the bar today)"* (#4) and *"want a right-click opt menu on tabs"* (#3). Both done in the UI lane.
- **Root cause of #4**: gpui-component renders a panel's `title_suffix` **only for the active
  tab** (`tab_panel.rs` multi-tab path puts it in the bar `.suffix(...)`), but calls
  `panel.title()` **once per tab**. So the close affordance moved **out of `title_suffix`
  into `title()`** → the "×" now shows on *every* tab. The old active-only
  `tab_close_button` helper + all five `title_suffix` overrides were **removed**.
- **New shared helper** `panels::tab_title(label, id_seed, tab_panel, panel, focus, cx, extend_menu)`
  (`panels/mod.rs`): builds `label + ×`. The **×** removes *that specific* panel via
  `TabPanel::remove_panel(Arc<dyn PanelView>)` (built from `cx.entity()`), bypassing the
  lock/`closable` gating — so an **inactive** tab's × closes *that* tab, not the active one.
  `cx.stop_propagation()` on the × so the close click doesn't also re-activate the doomed tab.
- **#3 right-click menu — first attempt with `ContextMenuExt::context_menu` did NOT fire** (operator:
  "nothing happens"). Root cause: that extension uses a window-level mouse handler gated on
  `hitbox.is_hovered`, which loses to gpui-component's own interactive `Tab` hitbox when nested in
  the tab bar — gpui-component's `tree.rs` proves the point by hosting `.context_menu()` on the
  *outer container* and tracking the right-clicked row separately. The tab bar is vendored, so we
  can't host a container-level menu there. **Fix:** mirror `tree.rs`'s working half — a nested
  `on_mouse_down(MouseButton::Right)` on the tab title **does** fire — build a `PopupMenu`
  (`PopupMenu::build` + `action_context(focus)` + a shared **`CloseTab`** action) and stash it in
  the panel (`TabMenu` slot via the new **`TabMenuHost`** trait); the panel's `render` draws it as a
  `deferred` + `anchored` overlay at the cursor with a full-window dismiss backdrop
  (`tab_menu_overlay`). Per-panel menu items via `extend_menu`: **Close** (all), **Split Right/Down**
  (`code_editor`, `session_monitor`), **Copy Path** (`code_editor` → `cx.write_to_clipboard`).
  Menu items dispatch to `focus` → the panel's existing `on_action` handlers (`CloseTab` wired
  one-line per panel via `panels::close_this_tab`).
- ⚠️ **Scope/limitation**: the menu renders from the panel's `render`, and only the **active** tab's
  body renders — so **right-click works on the active tab** (the common case). Right-clicking an
  *inactive* tab records state but shows no menu (no panel body in the tree), and the library exposes
  no public tab-activation API (`set_active_ix` is private) nor per-tab menu host to fix it cleanly.
  Inactive tabs still close via the always-visible **×**. A full inactive-tab menu would also need
  closure-based menu items (the panel's `on_action` handlers aren't painted when inactive) — deferred.
- **#3b right-click menu in the editor *body* (the page, not the tab)** — operator: "secondary
  menu should be usable for common actions like splitting or searching in text". The editor body is
  gpui-component's `Input`, which hosts its own context menu reliably (a big container, unlike the
  tab bar) and is customizable via `Input::context_menu(builder)`. Wired a custom builder
  (`code_editor.rs` render): **Find…** (`input::Search` → opens the in-editor find panel) · Cut/Copy/
  Paste/Select All (native `input::*` actions) · **Split Right/Down** · **Copy Path**. The `Input`
  always sets the menu's `action_context` to its own focus handle, which **bubbles to the panel's
  render root** — so the native actions AND our `SplitRight/SplitDown/CopyPath` `on_action` handlers
  all fire from one menu. (Setting a custom builder *replaces* the Input's default menu, so the
  clipboard items are re-listed explicitly.) Only the editor has a text body; other center panels
  (plan/review/db/session) could get a body menu later via `ContextMenuExt` on their render root —
  deferred (session's body is a terminal that captures mouse).
- Touched only Bucket-A files: `panels/mod.rs` + `code_editor/code_review/db_observer/plan_review/
  session_monitor.rs`. (Coexisted with Bucket B's `pub mod spaces;` add to `mod.rs` — additive.)
- ⚠️ **Runtime-verify**: (1) × visible+clickable on **inactive** tabs, closes the right one;
  (2) right-click the **active** tab opens the tab menu at the cursor; Close/Split/Copy-Path fire;
  clicking outside or Escape dismisses it; (3) right-click **inside the editor** opens the body menu;
  **Find…** opens the search bar and Split/Copy-Path work.

## TERMINAL POLISH — DONE (2026-06-03, this session)
- **Scrollback history**: `Emulator::scroll(lines)` / `scroll_to_bottom()` drive
  `Term::scroll_display` (buffer is `Config` default 10k lines — already present, just never
  scrolled). `terminal.rs` `on_scroll_wheel` maps wheel delta → lines; typing snaps back to live.
  ⚠️ verify scroll **direction** feels right (positive = older); trivial sign flip if inverted.
- **Spacing**: pinned the grid `line_height` to `CELL_H` (17px) — gpui's default line height
  (~1.3–1.4×) didn't match the cell height the PTY is sized against, causing row drift / trailing
  gap. ⚠️ visually re-verify; if a horizontal margin remains, `CELL_W` (7.8) may need tuning.

## BUCKET C — PERSISTENCE (#1 + #2, T7 partial) — DONE (2026-06-03, engine lane)
Durable managed-session status + true restore-on-restart that **keeps managed identity**. Full
workspace **build green, 124 tests passing** (persistence +6, engine +2; verified after the parallel
UI-lane `tab_close_button`→`close_this_tab` rename landed).
- **#1 — `crates/persistence` built out** (was a 3-line stub). SQLite via `rusqlite` (bundled, in-tree,
  no system libsqlite). `Store` (`open(path)` / `open_in_memory()`), forward-only migrations tracked by
  `PRAGMA user_version` (`migrations.rs`): `managed_session` (id, root, mode, phase, adopted, paused,
  created_at, last_seen) + append-only `audit_entry` (id, session_id, at, action, revertible) with a
  `(session_id, at)` index. Enum fields stored as serde-JSON text (stable across variant changes).
  Crate stays **domain-only** (no GPUI/engine/tokio) — the store is synchronous. 6 in-memory round-trip
  unit tests (upsert preserves `created_at`; `update_managed_state` is UPDATE-only; ordering; audit
  newest-first + session isolation + complex-variant JSON round-trip).
- **New domain port** `ManagedSessionStore` (sync) + `ManagedSession`/`ManagedStateUpdate` records in
  `crates/domain/src/ports/store.rs` (re-exported). *Presence in the managed table* is what marks a
  session managed vs observed. `StoreError` reused.
- **#2 — wired end-to-end.** Engine: `SessionSupervisor::with_store(control, bus, Option<Arc<dyn
  ManagedSessionStore>>)` (`new` keeps the pure/None path for tests). On state change the supervisor
  `persist`s via **UPDATE-only** `update_managed_state` (never resurrects a row → table stays
  managed-only) and `audit`s autonomous actions (PhaseChanged on PhaseObserved + FR13 done-revert;
  FeedbackInjected on inject) with time-ordered ids. +2 engine tests (fake store: persist+audit fire;
  no-store path still works).
- **Composition (`apps/desktop/src/main.rs`)**: `open_store()` opens
  `~/Library/Application Support/MoonlightCode/moonlight.db` (in-memory fallback if no home/disk), one
  `Arc<dyn ManagedSessionStore>` shared into the supervisor **and** `ShellDeps`.
- **Restore keeps managed identity (`workspace.rs`)**: the `NewManagedSession` launch arm records a
  `ManagedSession`; the `register_panel("SessionMonitor")` arm now consults `deps.store.managed(&id)` —
  a managed session comes back via `SessionMonitor::new_managed(.., "claude --resume <id>", rec.phase)`
  (re-embeds its terminal) instead of degrading to `new_observed`. Non-managed sessions rehydrate exactly
  as before. Complements the existing `dump()`/`restore()` layout path (which carries session+root).
- ⚠️ **Runtime-verify** (build/tests already green): (1) a managed session survives quit → restart and
  re-embeds a live `--resume` terminal (vs degrading to read-only observed); (2) the DB file is created
  under Application Support and the in-memory fallback path doesn't panic; (3) restored `rec.phase` is
  current (supervisor's UPDATE-only persist keeps it fresh while the session runs).

## MANAGED SESSION = EMBEDDED TERMINAL — DONE (2026-06-03, this session)
**Dialog with CC + see history, in-app.** ＋New session now opens a **focus tab with a
live terminal embedded** running `claude --session-id <uuid>` (CC 2.1.161 supports the
flag) in the focused project — scrollback = message history, typing = dialog. The pinned
uuid means the launched terminal and the grid tile (discovered from JSONL) are the **same
session**, so clicking the tile later reuses the same tab (terminal kept alive).
- `apps/desktop/Cargo.toml`: `uuid` (v4).
- `workspace.rs` `open_in_center`: re-adds the **same** panel `Arc` to activate (no rebuild),
  so the embedded terminal survives re-focus. `OpenRequest::NewConsole` → **`NewManagedSession { id }`**
  (key `session:<id>`, shared with `Session` so tile+tab dedup to one).
- `panels/session_monitor.rs`: optional embedded `Entity<TerminalPanel>` via `new_managed(...)`;
  rendered in place of the transcript placeholder. External/discovered sessions keep the placeholder.
- `grid_home.rs`: ＋New session generates a v4 uuid → emits `NewManagedSession`.
- **Focus view = existing header/facts card + content region** (NOT terminal-centric): managed
  sessions seed the header immediately (`new_managed` builds an initial `Session`) and embed the
  terminal **chromeless** (`TerminalPanel::embedded()` hides the tab strip) in the content region.
- **Observed sessions resume in a terminal too — gated to "not actively working".** Opening a
  discovered session with a repo path: if the agent isn't **Running** it auto-resumes (`claude --resume
  <id>` embedded, rooted at its repo) so you can take over; while it **is Running** it shows the
  read-only transcript + a **"▶ Resume in terminal"** button **disabled until the agent stops** (avoids
  two clients on one transcript). NB: `resumable()` = `!Running` — `WaitingInput` ("your turn") is the
  common resumable state; literal `Idle` means *no turns observed yet* (≈never for real sessions) and
  `Done` only after the 6h stale window, so gating on Idle|Done would keep the button perpetually
  disabled. `session_monitor.rs`: `new_observed`, `try_resume`, `resumable()`, `resume_bar`. Terminal
  pinned to the repo, no focus-follow (`new_running_in`). `dump()` persists the root for rehydration.
  Panel reuse means re-clicking the tile doesn't re-spawn.
- **Read-only transcript** (`apps/desktop/src/transcript.rs`) is the **fallback** for sessions with
  no known repo path (can't root a `--resume` terminal): parses message history from on-disk JSONL,
  1.5s refresh. text + `⚙ tool` markers; tool-result echoes skipped. **Cleaned (refine pass):**
  `clean_text` strips injected `<system-reminder>`/command-wrapper noise + collapses blank runs;
  rendered as per-message cards with a role-colored left accent; bodies capped at 30 lines
  (`MAX_MESSAGE_LINES`) with a "… N more line(s)" trailer. Still TODO if wanted: markdown, auto-scroll.
- **Quieted degraded-control noise** (`supervisor.rs::surface_control_error`): `ControlError::
  Unavailable` (steer/deny with no capable adapter — observe-only L0) now logs at `debug` and audits
  as "<op> not delivered — control is observe-only", not a red ERROR. Real feedback delivery still
  needs the hooks-based `UserPromptSubmit` inject (later); for now, steer by typing in the terminal.
- ⚠️ **Runtime-verify**: `claude --session-id` / `--resume` start cleanly in the embedded PTY;
  embedded-terminal sizing/focus; tile reuses the managed tab; the resume button enables when a live
  session goes idle. (The two-clients-on-one-transcript risk is now gated: auto-resume only for
  idle/done, button disabled while live.)

## G2 KEYSTONE — DONE (2026-06-03, this session)
The held-hook approval keystone (**T3**) is built and wired end-to-end, plus plan
validation (**G2**) and the code-review gate (**G4/G5**). All green, 99 tests.
- **Keystone core** (`crates/control/pending.rs`): `PendingApprovals` registry (oneshot,
  keyed by session) + `Decision` + `ApprovalNotifier` trait. `gate::evaluate` →
  `GateDecision::{Allow,Deny,Hold(HoldKind::{Plan,Danger})}`; `ExitPlanMode` always holds,
  PDP `Prompt` → danger hold. `decide` kept as the sync hold→deny shim.
- **Server holds** (`control/server.rs`): `ControlServer::with_approvals` registers+notifies+
  awaits the oneshot (`DEFAULT_HOLD` 45s → deny "window elapsed"). Approve=Allow, Deny=deny+reason.
- **Timeouts** (`main.rs` `HOOK_TIMEOUT`=55s; `hook_install.rs` writes `timeout:60` on the
  PreToolUse entry) so CC waits out the hold; socket-absent still fails open instantly.
- **App wiring** (`main.rs`): `BusNotifier` publishes `PlanProposed`+`ApprovalRequested`+
  `WaitingInput`; `route_approval` resolves Approve/Deny against `pending` (republishes Running),
  else forwards to the supervisor.
- **Plan UI** (`panels/plan_review.rs`): Approve/Reject buttons (live when an approval is
  pending), folds the bus for pending state. Default reject reason (T14 markdown render still open).
- **Code-review gate** (`git/diff.rs` + `panels/code_review.rs`): per-file diff vs HEAD (G4),
  **Run full-review** button → `claude -p "/full-review"` in the repo (G5/T16, manual), Approve/
  Request-changes reuse the keystone. Opened on `ReviewReady` via the workspace bus→center bridge
  (tracks `attached_path` from `SessionUpserted` for the repo root). `OpenRequest::CodeReview` added.

### ⚠️ Runtime-verify before trusting (could not test against live CC here)
1. **ExitPlanMode deniability** — confirm CC fires a *deniable* `PreToolUse` for `ExitPlanMode`
   (the plan hold point). If it doesn't, move the plan hold to the first post-plan write.
2. **Hold ceiling** — 45s server / 55s client / 60s CC. Long reviews time out into a re-runnable
   deny; tune if needed.
3. **`claude -p /full-review` shell-out** — verify the headless invocation/permissions in a real
   session repo; output is captured raw into the review tab.
Also: T3's danger-zone hold has no dedicated cockpit surface yet (only the plan tab + tile
WaitingInput) — generic danger approvals still need a banner/needs-you affordance (T11).
Note: added a missing `Phase` import to `crates/trust/src/lib.rs` tests (was breaking the suite).

## DONE (this build)
- Live grid: real `~/.claude/projects` sessions; mtime-liveness status refresh; triage order; repo paths; adopt toggle.
- Engine: `EventBus` + `SessionSupervisor` (FR13 auto-revert); `Command::{ToggleMode,ToggleAdoption,…}`.
- Control adapter (hook posture, "enrich don't own"): classify(+Bash), `gate::decide`, Unix-socket
  `ControlServer` (fail-open), `moonlight hook`, `moonlight hooks {install,uninstall,status}`. `Session.adopted`.
- Plan-review **data + UI**: detection parses `ExitPlanMode` → `PlanProposed` → auto-opens "Plan" center tab.
- ＋New session console (`TerminalPanel::new_running`, runs `claude`).
- Detection: JSONL tail (status/phase/title/cwd/plan), defensive parsing.

## TASK LIST (prioritized)

### P0 — Fix before first commit (from full-review 2026-06-03; engine lane) — R1–R9 DONE
- [x] **R1. Bash danger classification bypassable** — DONE: `classify.rs` now splits on list/pipe
      segments and tokenizes (combined `rm -rf`/`-f -r`, `find -delete`, env-prefix, worst-segment-wins). +tests.
- [x] **R2. `2>&1` disables redirect-write detection** — DONE: `has_file_redirect` is fd-dup aware. +tests.
- [x] **R3. Hook event type unvalidated** — DONE: server short-circuits `Allow` for non-`PreToolUse`;
      `run_hook` emits canonical `"PreToolUse"`. +test.
- [x] **R4. Partial JSONL line lost** — DONE: `read_tail` returns raw bytes; poll consumes only to the
      last `\n` (offset in file-bytes), decodes the complete part lossily. +test.
- [x] **R5. Inactivity = `Done`** — DONE: `Ended` only after 3 consecutive misses (anti-flap);
      supervisor maps it to **Idle** (not Done → no false ReviewReady) and **keeps adopted** sessions. +test.
- [x] **R6. `paused` gate** — DONE (decision: WIRE): `Session.paused` + `Command::TogglePause` +
      supervisor + gate fold + **pause chip** on adopted tiles. Gate already denies when paused.
- [x] **R7. `is_ours` mutates foreign hook config** — DONE: matches the specific `hook pre-tool-use` subcommand.
- [x] **R8. Control socket hijackable / world-readable** — DONE: refuse-if-live (connect probe) + `0600` perms.
- [x] **R9. `apply_gate_event` clobbers operator state** — DONE: folds `adopted`/`paused` from the
      authoritative `Session`; dropped the `MOONLIGHT_ADOPT` env override (in-app toggle has landed).
- [~] **R10 (medium batch).** DONE: `.expect`→`into_inner`; `git config value` now Risky. DEFERRED (low-risk):
      `bypassPermissions`→AutoImplement (acceptable — we gate independently of CC's mode); future-mtime
      perpetual-Running (rare clock skew); session-id = file-stem collision (CC ids are UUIDs).
- [ ] **R11. Test gaps** `[sheik]` — poisoned-lock fail-open; control-port error path (FakeControl always Ok);
      `apply_gate_event` extraction. (Many other tests added across R1–R6.)

### P1 — Code-review gate runs full-review (G5)
- [x] **T16. Integrate full-review into the code-review gate.** DONE (2026-06-03): review tab has a
      manual **Run full-review** button → `claude -p "/full-review"` in the session repo, output rendered
      in-tab. ⚠️ shell-out mechanism still needs live verification. On the review tab (T5): show the G4
      per-file diff **+ a "Run full-review" button** the operator clicks (DECIDED 2026-06-03: **manual
      trigger only for now** — no auto-run on `ReviewReady`). Button → app shells out to headless
      `claude -p` over the session's diff → render findings in the tab. Depends on T5 + git diff.

### P1 — Authorization realism (engine lane)
- [ ] **T1. Trust-tier-aware gating.** PDP only uses tier for MCP verbs; tool-use passes `verb:None`,
      so tier doesn't modulate tool danger. Make danger handling graduated by tier (e.g. Trusted →
      danger-zone *prompts* not denies; lower tiers stricter). Files: `crates/trust/src/lib.rs`,
      `crates/control/src/gate.rs`. Done: tests for tier×danger matrix; auto-mode stays permissive.
- [ ] **T2. Tune classify marker lists from real runs.** `crates/control/src/classify.rs`. Done: refined DANGER/WRITE lists + tests.

### P1 — Interactive approval routing (the hard one; cross-lane)
- [x] **T3. Approve/reject channel.** DONE (2026-06-03) — held-hook keystone (see G2 KEYSTONE section
      above). Operator approves/denies plan + code from the cockpit; the session proceeds in-turn.
      ⚠️ ExitPlanMode deniability + hold ceiling need live verification. (Original note below.)
      Hook is synchronous + fail-open → can't block for a human, so
      `Prompt` currently hard-denies. Design+build a non-hook approval path (e.g. pre-set per-session
      policy the gate reads, or a deferred "held action" + operator decision). See control-design §10b.
      Done: operator can approve a danger-zone/plan action from the cockpit and the session proceeds.
      Unblocks plan-review & code-review *acceptance* (currently display-only).

### P2 — Code-review gate (Gate 2)
- [ ] **T4. Summary capture (engine lane).** Detection parses the session's final assistant text →
      `DetectionEvent::SummaryObserved`/`EngineEvent::SummaryObserved`. Mirror the `PlanProposed` impl.
      Files: `crates/domain/src/ports/detection.rs`, `crates/engine/src/lib.rs`+`supervisor.rs`,
      `crates/detection/src/{jsonl,lib}.rs`. Done: event flows + parser test.
- [x] **T5. Code-review tab — per-file diff (UI lane; G4).** DONE (2026-06-03) — `panels/code_review.rs`
      + `git/diff.rs` (per-file diff vs HEAD), file list + colorized diff, opened on `ReviewReady`.
      Still TODO: the **T4 summary** ("what this covers") — tab shows the diff only for now. Original:
      On `ReviewReady`: center tab
      with a **file list + per-file diff (current vs previous/HEAD)** via the `git` module, plus the T4
      summary ("what this covers"). Pattern: `plan_review.rs` panel + the workspace bus→center bridge.
      Handoff: `.ai/handoffs/review-gates-ui.md`.

### P2 — Detection precision (engine lane)
- [ ] **T6. Hooks→app event channel.** Add a server→app channel so hook observations (Stop/Notification/
      PreToolUse) feed the engine for *precise* working/waiting/done (today's JSONL status is coarse;
      see `status_for`). Also enables real-time plan capture. Done: status flips precisely on Stop/Notification.

### P3 — Engine build-order backlog (architecture §Implementation sequence)
- [~] **T7. Persistence + audit log** (`crates/persistence`, SQLite): DONE (partial, 2026-06-03 — see
      Bucket C below). Managed-session store + append-only audit foundation built, tested, and wired
      (supervisor persists state-on-change + audits; restore re-resumes managed sessions). Remaining for
      full T7: continuity summaries (`SessionStore`/`SessionSummary`), `BaselineStore`, in-app audit viewer.
- [ ] **T8. MCP-actor server** (`crates/mcp-server`, rmcp): actor verbs through the PDP + audit.
- [ ] **T9. Token economy + governor + HUD read-models.**
- [ ] **T10. Worktree isolation + lock mediator** (`crates/worktree`).

### P3 — UI backlog (UI lane / parallel)
- [ ] **T11.** Plan/code-review approve-reject affordance (needs T3). Needs-you queue panel. Command palette. HUD strip. Focus-mode live transcript feed.
- [ ] **T14. Render plan markdown (G3).** `panels/plan_review.rs` currently prints raw lines; render
      proper markdown (headings/lists/bold) — check for a `gpui-component` markdown widget first.

### Housekeeping
- [ ] **T12. First commit** — everything is uncommitted on top of the initial commit. Ask owner for granularity (one milestone vs logical splits). Do NOT push.
- [ ] **T13.** Keep `cargo clippy` clean (only the transitive `block` future-incompat is expected).

## Coordination contracts
- Adding an `EngineEvent` variant → update its exhaustive matches in `grid_home::FleetModel::apply_event`
  (ignore-arm if the grid doesn't care) and check `main::apply_gate_event` (has `_ =>`).
- New center tab = `OpenRequest` variant + `panels/<x>.rs` Panel + `workspace::open_in_center` arm +
  `register_panel` (rehydration). Auto-open from an engine fact = a bus→center bridge in `Workspace::new`.
- Control crate stays domain-only; bus→`GateState` fold lives in `main.rs`.
- `moonlight hooks install` is live in `~/.claude/settings.json` (debug binary). Rebuild = new behavior; restart CC sessions to pick up hook changes.
