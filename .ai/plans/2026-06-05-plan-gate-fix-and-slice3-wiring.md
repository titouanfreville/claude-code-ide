# Plan — A) Plan-phase gating fix (exploration + plan/spec writes) · B) Slice 3 MCP wiring + live verify

> NOTE: this plan lives in `.ai/plans/` instead of `~/.claude/plans/` because the PDP denied the
> harness plan-file write in Plan phase — which is itself bug #3 below. Meta-proof included free of charge.

## Context
Two approved work items:
- **A (operator RQ, hit live 4× this session):** In Plan/Discovery the PDP must allow all read-only/exploration actions and deny only *project* writes — plan/spec writes stay allowed. Today it denied: `rtk grep "a\|b" | head` (Bash), `mcp__rustrover__search_in_files_by_regex` (read-only MCP tool), and `Write` to `~/.claude/plans/` (twice).
- **B (chosen next step):** wire the built-but-unwired per-session embedded HTTP MCP server into the app and verify `run_with_coverage` against live CC (Slice 3, the status file's explicit "next").

Do **A first** — it unblocks gated sessions (including this one) for the rest of the work.

## Part A — PDP/classify fixes (engine lane: crates/control, crates/trust — free lane)

Root causes (all confirmed in code):
1. **Quoted-pipe bug** — `classify_bash` (`crates/control/src/classify.rs:67-93`) normalizes `&&`/`||`/`;`/`|` → `\n` with plain `str::replace`, ignoring quotes. A quoted pattern containing `|` (e.g. `grep "a\|b"`) splits mid-string → segment led by an unknown token → Risky → denied in frozen phases.
2. **Unknown tool ⇒ Risky** — `classify` (`classify.rs:37-38`) defaults `_ => Risky`. Read-only MCP tools (`mcp__rustrover__search_in_files_by_regex`, `lsp_*`…) are frozen in Plan.
3. **Plan files outside the repo ⇒ Project** — `AiWorkspace::scope_of` (`crates/control/src/paths.rs:89-130`): roots are repo-relative only; `~/.claude/plans/...` is absolute outside cwd → Project → frozen.

Changes:
1. **Quote-aware segmentation** (`classify.rs`): replace the `replace()` normalization with a small scanner that splits on `&&`/`||`/`;`/`|`/newline only **outside** single/double quotes (backslash-escape aware inside double quotes). Unbalanced quotes → treat the remainder as one segment (conservative). Keep `2>&1`/`&` behavior. Tests: `quoted_pipe_does_not_split_segments` (`grep "a\|b" f | head` → Safe), unbalanced-quote case, existing suite green.
2. **Read-only MCP tool names** (`classify.rs`): for `mcp__<server>__<tool>` names, classify by the tool's leading verb: `get|list|read|search|find|query|preview|describe|...` → Safe; everything else (and non-matching names) stays Risky. Plus a config escape hatch: `safe_tools: [..]` in `.moonlight/config.json` (extend the config in `crates/control/src/config.rs`) threaded into the server like the AI-roots resolver. Tests: rustrover search tool Safe, `mcp__x__delete_y` Risky, config-listed tool Safe.
3. **Home-anchored AI roots** (`paths.rs`): allow roots starting with `~/` or `/` (absolute): `scope_of` matches them against the *absolute* tool path (expand `~` via `$HOME`) before the repo-relative pass. Add `~/.claude/plans` to `DEFAULT_AI_ROOTS`. Tests: `~/.claude/plans/x.md` → AiWorkspace from any cwd; `/etc/hosts` stays Project.
4. End-to-end test in `crates/control/src/server.rs` mirroring `frozen_phase_allows_ai_workspace_write_over_socket`: Plan phase allows Write to `~/.claude/plans/…` and Bash `grep "a\|b" | head`; still denies `cargo build` and project Edit.

⚠️ After A lands: **rebuild the debug binary and restart CC sessions** (the hook runs the installed binary — handoff rule).

## Part B — Slice 3 MCP wiring (composition root + UI lane: HOT — read-before-edit)

Recipe from `.ai/handoffs/00-status-and-tasks.md` §"Slice 3 (1) rmcp transport", adjusted for current code:
1. **`apps/desktop/src/main.rs`** — build once, before `spawn_control_server`:
   - Hoist `BusNotifier` out of `spawn_control_server` (`main.rs:431`) into a shared `Arc` (used by both server and approval gate).
   - `let policy = Arc::new(BusPolicyView::new());` (`crates/mcp-server/src/adapters.rs:31`)
   - `let actor: Arc<dyn McpActor> = Arc::new(ActorService::new(Arc::new(DefaultPdp), policy.clone(), Arc::new(ShellVerbExecutor::new(test_command)), Arc::new(StoreAuditSink::new(store.clone())), Arc::new(KeystoneApprovalGate::new(pending.clone(), notifier.clone(), Duration::from_secs(45)))));`
     - `test_command`: read `test_command` from `~/.moonlight/config.json` (same loader as `ai_workspace_resolver`, `main.rs:362`), default `["cargo","test"]`.
   - Bus-fold task mirroring the gate fold (`main.rs:131-142`): `rx.recv → policy.apply(&event)`.
2. **Cross-runtime host**: `McpHost::spawn_for` (`crates/mcp-server/src/transport.rs:147`) needs a tokio reactor; the control server's runtime is `current_thread` inside its own thread (`main.rs:404`) and not exposed. Create one shared **multi-thread tokio Runtime** in `main` (small, 1–2 workers), keep it alive, and add an app-side wrapper (e.g. `views/mcp_host.rs`): `McpHostHandle { host: McpHost, handle: tokio::runtime::Handle }` with sync `url_for(&SessionId) -> Option<String>` doing `handle.block_on(host.spawn_for(..))`, caching per-session URLs (don't double-bind on relaunch).
3. **`ShellDeps`** (`apps/desktop/src/views/workspace.rs:84-121` + `init_shell:128`): add `mcp_host: McpHostHandle` field + param; seed from the `init_shell` call (`main.rs:174-187`).
4. **Launch sites** — append `--mcp-config '{"mcpServers":{"moonlight":{"type":"http","url":"<url>"}}}'` (verify CC's exact flag shape live; single-quoted JSON like the `--settings` pattern in `obs::with_statusline`, `obs.rs:453-459`):
   - `workspace.rs:1073-1077` `NewManagedSession` arm
   - `session_monitor.rs:519-530` `attach_command()` — give it an `mcp_url: Option<&str>` param (callers: session_monitor.rs:147, :285, workspace.rs:213, :1142)
   - `session_monitor.rs:1343-1367` `relaunch_terminal()`
5. **Tests**: McpHostHandle url caching; attach_command carries the flag when a URL is given; existing 11 mcp-server tests stay green.
6. **⚠️ Live-CC verify** (RustRover ▶ Run moonlight): launch a managed session → `/mcp` in CC lists `moonlight` with `run_with_coverage`; calling it runs the test command and returns the compact summary; a `VerbExecuted` audit row lands; an over-tier verb raises a cockpit approval (KeystoneApprovalGate) before running; deny refuses with feedback.

## Housekeeping
- Update `.ai/handoffs/00-status-and-tasks.md` (status date, Part A section, Slice 3 wiring → done-pending-verify).
- Build/test per slice: `rtk cargo build -p moonlight-desktop`, `rtk cargo test`, clippy clean on touched files.

## Verification
1. `cargo test` full workspace green (new classify/paths/server/mcp tests included).
2. Hook re-test in a gated Plan session: quoted-pipe grep allowed, rustrover search allowed, plan-file Write allowed, project Edit still denied, `rm -rf` still DangerZone.
3. Live-CC MCP verify per B.6.
