# Frozen-phase MCP authorization prompt (once / always / refuse)

**Goal.** In a frozen phase (Discovery/Plan), an external MCP tool call that would
today be hard-denied by the project freeze should instead **prompt the operator**
with the Claude Code pattern: **authorize once / authorize always / refuse**.
"Authorize always" persists the tool (or its whole server) into the configurable
read-only allowlist so it never prompts again.

This also satisfies "enable phoenix read-only parts, configurable" — the operator
builds their allowlist by clicking *always*, and can pre-seed it by editing config.

## Decisions (confirmed with operator 2026-06-24)

1. **Prompt scope:** external MCP tools only — `mcp__*` excluding `mcp__moonlight__*`.
   Project-file `Edit`/`Write` and mutating Bash stay **hard-denied** (the phase
   freeze on the product is the point of Discovery/Plan).
2. **"Always" granularity:** the prompt offers *both* — "always allow this tool"
   (`mcp__phoenix__run_select_query`) and "always allow this server" (`mcp__phoenix__*`).
3. **Persist to:** user-level `~/.moonlight/config.json` `safe_tools` (applies across
   all the operator's projects).

## Current behavior (baseline)

- External MCP tools flow through the PreToolUse hook → `ControlServer::decide`
  (`crates/control/src/server.rs:150`) → `gate::evaluate` → `DefaultPdp::decide`
  (`crates/trust/src/lib.rs:16`).
- `classify_mcp_tool` (`classify.rs:58`) marks read-verb tools `Safe` (already pass
  in any phase) and everything else `Risky`. A `Risky` MCP call in a frozen phase
  has `write_scope=None` → treated as `Project` → `if !permitted` → **`Deny`**
  (`trust/src/lib.rs:52-77`).
- The hold/prompt machinery already exists for `PermissionOutcome::Prompt`
  (DangerZone, low trust tier): `Prompt` → `GateDecision::Hold(HoldKind::Danger)` →
  `server.hold()` → `ApprovalNotifier` → cockpit → `PendingApprovals::resolve`.
  But `Decision` is **binary** (`Approve`/`Deny`) and there is no config write-back.
- `safe_tools` is consulted via `AiWorkspace::is_safe_tool` (server.rs:169,
  `vouched_safe`). Matching is **exact** today (needs glob for `mcp__phoenix__*`).
- Config is loaded **once at startup** into `Resolver::Layered { user, cache }`
  (`config.rs:70`) and memoized per cwd → **persisting to disk alone won't take
  effect until restart**. A runtime overlay is required for immediate effect.

## Design

### 1. Mark "external MCP, prompt-on-freeze" into the decision

`PermissionRequest` (domain `ports.rs`) gains a flag, e.g.
`prompt_on_project_freeze: bool`. Set it in `gate::evaluate` when
`tool_name.starts_with("mcp__") && !tool_name.starts_with("mcp__moonlight__")`.

In `DefaultPdp::decide`, in the `if !permitted` block for `WriteScope::Project`:
if `req.prompt_on_project_freeze`, return `Prompt { reason }` (offer to authorize)
instead of `Deny`. Project `Edit`/`Write`/Bash leave the flag false → unchanged
hard `Deny`. (Seam is `trust/src/lib.rs:52-77`.)

### 2. Carry tool identity to the operator

- `HoldKind` gains a variant `McpAuthorize { tool_name: String, reason: String }`
  (distinct from `Danger`) so the UI knows to render the 4-way prompt. `gate.rs`
  maps the new `Prompt` to this when the tool is an external MCP tool.
- `ApprovalNotifier::approval_requested` + `EngineEvent::ApprovalRequested` +
  the `BusNotifier` (main.rs:542) carry the tool name and an "authorize kind" so
  the cockpit can show server/tool patterns. (Add a field; keep plan path intact.)

### 3. Operator decision → "always" persistence (UI side)

Keep `Decision` binary in `control` (the held hook only needs allow/deny).
"Always" is handled in the **composition root** (has FS + config-path access):

New `Command` variants: `AuthorizeAlwaysTool { session, pattern }` (pattern is the
exact tool or the `mcp__server__*` glob). Its handler:
  1. Append `pattern` to a **runtime safe-tools overlay** (new shared
     `RwLock<HashSet<String>>`) consulted by the resolver → immediate effect, no
     restart, no cache-clear race.
  2. Append `pattern` to `~/.moonlight/config.json` `safe_tools` on disk
     (read-modify-write, create dir/file if absent, preserve other keys) → durable.
  3. `pending.resolve(session, Decision::Approve)` → this call proceeds now.

New control helper: `AiWorkspace::is_safe_tool` extended to match a trailing-`*`
glob (`mcp__phoenix__*`), and the resolver consults the runtime overlay in addition
to config. Add a `persist::append_safe_tool(home, pattern)` helper in `control` (pure
file read-modify-write over `AiWorkspaceConfig`), unit-testable against a temp home.

### 4. Cockpit UI (`views/panels/plan_review.rs` + `workspace.rs:692`)

For `McpAuthorize`, render four affordances:
  `[Allow once]  [Always allow <tool>]  [Always allow <server>·mcp__phoenix__*]  [Refuse]`
- Allow once → `Command::ApproveAction`
- Always (tool) → `AuthorizeAlwaysTool { pattern: tool_name }`
- Always (server) → `AuthorizeAlwaysTool { pattern: "mcp__<server>__*" }`
- Refuse → `Command::DenyAction { reason }`
Plan/Danger holds keep the existing two-button rendering.

## Files to touch

- `crates/domain/src/ports.rs` — `PermissionRequest.prompt_on_project_freeze`.
- `crates/trust/src/lib.rs` — Prompt-instead-of-Deny seam + tests.
- `crates/control/src/gate.rs` — set flag for external MCP; new `HoldKind::McpAuthorize`; tests.
- `crates/control/src/classify.rs` — (no change; read-verb MCP already Safe).
- `crates/control/src/paths.rs` — `is_safe_tool` glob match + runtime overlay hook; tests.
- `crates/control/src/config.rs` — `append_safe_tool` persist helper; resolver overlay; tests.
- `crates/control/src/pending.rs` — `ApprovalNotifier` signature (tool name + kind).
- `crates/engine/src/lib.rs` — `ApprovalRequested` event fields.
- `apps/desktop/src/main.rs` — `BusNotifier`, `route_approval`/`Command` for `AuthorizeAlwaysTool`, overlay wiring + persist call.
- `apps/desktop/src/views/panels/plan_review.rs`, `views/workspace.rs` — 4-way prompt UI.

## Staging

- **S1 (core policy, no UI) — DONE 2026-06-25, 122 tests green:** `PermissionRequest.
  prompt_on_project_freeze` flag + PDP Prompt seam (`trust/src/lib.rs`) +
  `HoldKind::McpAuthorize` + `is_external_mcp_tool` gate routing (`control/src/gate.rs`)
  + trailing-`*` glob `is_safe_tool` (`control/src/paths.rs`). Behavior now: an external
  MCP tool in a frozen phase becomes a Hold — the existing binary cockpit renders
  approve/deny, so **"once" and "refuse" already work**; pre-seeding `mcp__phoenix__*`
  in `safe_tools` config also works now. ("always" buttons pending S2.)
  - Moved to S2 (no consumer in S1 → would be dead code): the **runtime overlay** and
    the **`append_safe_tool` persist helper**.
- **S2 (persistence + plumbing) — DONE 2026-06-25, 157 lib + 238 desktop tests green:**
  - Runtime safe-tools overlay `RuntimeSafeTools = Arc<RwLock<HashSet>>` on `ControlServer`
    (`server.rs`, `.with_runtime_safe`), consulted in `decide()` alongside config —
    immediate effect, no restart. Tested over the socket.
  - `append_safe_tool` user-config write-back (`config.rs`): raw-JSON read-modify-write
    that preserves unrelated keys (`lsp_servers`), dedupes, refuses to clobber malformed
    files. Unit-tested.
  - `ApprovalNotifier::approval_requested` gains `mcp_tool: Option<&str>`; `HoldKind::
    McpAuthorize` forwards the tool name; tested via `RecordingNotifier`.
  - `EngineEvent::ApprovalRequested` gains `authorize_tool: Option<String>`;
    `Command::AuthorizeAlwaysTool { session, pattern }`; `route_approval` vouches the
    pattern in the overlay + approves the held call (unit-tested), and `dispatch_command`
    does the durable `persist_always_allow` (HOME-based, kept out of the unit test).
  - `workspace.rs` surfaces the tool name in the approval notification.

- **S2-UI — DONE 2026-06-25, full workspace 423 tests green, desktop builds clean:**
  - New `McpAuthorizePanel` (`views/panels/mcp_authorize.rs`) mirroring `PlanReviewPanel`:
    4 buttons — **Allow once** → `ApproveAction`; **Always allow this tool** →
    `AuthorizeAlwaysTool { pattern: tool }`; **Always allow this server** →
    `AuthorizeAlwaysTool { pattern: server_glob(tool) }` (`mcp__<server>__*`, unit-tested);
    **Refuse** → `DenyAction`. Folds `SessionStateChanged`/`SessionRemoved` to disarm and
    re-arms on a fresh `ApprovalRequested` for the session.
  - `OpenRequest::McpAuthorize { session, tool, root }` (+ `key`/`target_root`); emitted
    from the `ApprovalRequested { authorize_tool: Some }` handler in `workspace.rs` into the
    requesting session's space; built as a center tab in the live `open_center` factory.
  - **Transient by design:** no `panel_state_key` arm, so a stale authorization tab is
    dropped on restart (no live hook to resolve) — matching the held-hook semantics.

  **Feature complete.** The full path works: frozen-phase external MCP call → hold →
  cockpit authorization tab → once/always-tool/always-server/refuse → overlay (immediate)
  + `~/.moonlight/config.json` persist (durable).

## Open risks / notes

- **Restart-to-effect** avoided via the runtime overlay (decision 3.1); disk write is
  for durability only. Do NOT rely on clearing the per-cwd cache.
- `mcp__moonlight__*` is explicitly excluded from prompt-on-freeze (its own verbs are
  gated by the embedded MCP-server PDP; `request_phase`/`report_blocked` are `Safe`).
- Fail-open posture preserved: any error in persist must not block the held hook —
  resolve `Approve` regardless of whether the disk write succeeded (log on failure).
